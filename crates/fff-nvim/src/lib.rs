use crate::path_shortening::shorten_path_with_cache;
use error::IntoLuaResult;
use fff::file_picker::FilePicker;
use fff::frecency::FrecencyTracker;
use fff::path_utils::expand_tilde;
use fff::query_tracker::QueryTracker;
use fff::{
    DbHealthChecker, DirSearchConfig, Error, FFFMode, FFFQuery, FileSearchConfig,
    FuzzySearchOptions, MixedSearchConfig, PaginationArgs, QueryParser, Score, SearchResult,
    SharedFilePicker, SharedFrecency, SharedQueryTracker,
};
use mimalloc::MiMalloc;
use mlua::prelude::*;
use once_cell::sync::Lazy;
use path_shortening::PathShortenStrategy;
use std::path::{Path, PathBuf};
use std::time::Duration;
use user_config::{NvimGrepConfig, UserConfigOptions, set_global_user_config};

mod error;
mod hex_dump;
mod log;
mod lua_types;
mod path_shortening;
mod user_config;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

// the global state for neovim lives here for efficiency
// lua ffi is pretty bad with the overhead of converting raw pointer into tables
pub static FILE_PICKER: Lazy<SharedFilePicker> = Lazy::new(SharedFilePicker::default);
pub static FRECENCY: Lazy<SharedFrecency> = Lazy::new(SharedFrecency::default);
pub static QUERY_TRACKER: Lazy<SharedQueryTracker> = Lazy::new(SharedQueryTracker::default);

pub fn init_db(
    _: &Lua,
    (frecency_db_path, history_db_path, _use_unsafe_no_lock): (String, String, bool),
) -> LuaResult<bool> {
    // Route through SharedFrecency::init / SharedQueryTracker::init so the
    // GC thread gets spawned + bound to the shared handle's RwLock.
    FRECENCY
        .init(FrecencyTracker::open(&frecency_db_path).into_lua_result()?)
        .into_lua_result()?;
    tracing::info!("Frecency database initialized at {}", frecency_db_path);

    QUERY_TRACKER
        .init(QueryTracker::open(&history_db_path).into_lua_result()?)
        .into_lua_result()?;
    tracing::info!("Query tracker database initialized at {}", history_db_path);
    Ok(true)
}

pub fn destroy_frecency_db(_: &Lua, _: ()) -> LuaResult<bool> {
    Ok(FRECENCY.destroy().into_lua_result()?.is_some())
}

pub fn destroy_query_db(_: &Lua, _: ()) -> LuaResult<bool> {
    Ok(QUERY_TRACKER.destroy().into_lua_result()?.is_some())
}

/// Opts table accepted by `init_file_picker` / `restart_index_in_path`.
/// Backwards compat: positional `follow_symlinks: bool` argument still works
/// — callers that pass a table use the named fields instead.
#[derive(Default)]
struct PickerInitOpts {
    follow_symlinks: bool,
    enable_fs_root_scanning: bool,
    enable_home_dir_scanning: bool,
    enable_filename_constraint: bool,
}

impl PickerInitOpts {
    fn from_lua_value(value: Option<mlua::Value>) -> LuaResult<Self> {
        let Some(value) = value else {
            return Ok(Self::default());
        };
        match value {
            mlua::Value::Nil => Ok(Self::default()),
            mlua::Value::Boolean(b) => Ok(Self {
                follow_symlinks: b,
                ..Default::default()
            }),
            mlua::Value::Table(t) => Ok(Self {
                follow_symlinks: t.get::<Option<bool>>("follow_symlinks")?.unwrap_or(false),
                enable_fs_root_scanning: t
                    .get::<Option<bool>>("enable_fs_root_scanning")?
                    .unwrap_or(false),
                enable_home_dir_scanning: t
                    .get::<Option<bool>>("enable_home_dir_scanning")?
                    .unwrap_or(false),
                enable_filename_constraint: t
                    .get::<Option<bool>>("enable_filename_constraint")?
                    .unwrap_or(false),
            }),
            other => Err(LuaError::RuntimeError(format!(
                "init opts must be a table, boolean, or nil — got {}",
                other.type_name()
            ))),
        }
    }
}

pub fn init_file_picker(
    _: &Lua,
    (base_path, opts): (String, Option<mlua::Value>),
) -> LuaResult<bool> {
    {
        let guard = FILE_PICKER.read().into_lua_result()?;
        if guard.is_some() {
            return Ok(false);
        }
    }

    let opts = PickerInitOpts::from_lua_value(opts)?;
    set_global_user_config(UserConfigOptions {
        enable_filename_constraint: opts.enable_filename_constraint,
    });

    FilePicker::new_with_shared_state(
        FILE_PICKER.clone(),
        FRECENCY.clone(),
        fff::FilePickerOptions {
            base_path,
            enable_mmap_cache: true,
            enable_content_indexing: true,
            mode: FFFMode::Neovim,
            follow_symlinks: opts.follow_symlinks,
            enable_fs_root_scanning: opts.enable_fs_root_scanning,
            enable_home_dir_scanning: opts.enable_home_dir_scanning,
            ..Default::default()
        },
    )
    .into_lua_result()?;

    Ok(true)
}

pub fn restart_index_in_path(
    _: &Lua,
    (new_path, opts): (String, Option<mlua::Value>),
) -> LuaResult<()> {
    let path = std::path::PathBuf::from(&new_path);
    if !path.exists() {
        return Err(LuaError::RuntimeError(format!(
            "Path does not exist: {}",
            new_path
        )));
    }

    let canonical_path = fff::path_utils::canonicalize(&path).map_err(|e| {
        LuaError::RuntimeError(format!("Failed to canonicalize path '{}': {}", new_path, e))
    })?;

    let opts = PickerInitOpts::from_lua_value(opts)?;
    set_global_user_config(UserConfigOptions {
        enable_filename_constraint: opts.enable_filename_constraint,
    });

    // Spawn a background thread BEFORE touching the picker lock. The
    // same-dir short-circuit previously called `FILE_PICKER.read()` on
    // the lua/UI thread, which blocks if a reindex writer is already in
    // flight — on repeated DirChanged events (e.g. LSP root switching
    // right after closing the picker) that could freeze the nvim main
    // loop for the entire duration of an in-progress scan. Move the
    // check into the spawned worker so the main thread always returns
    // instantly; the internal reinit path also re-checks and no-ops if
    // the picker is already pointing at `canonical_path`.
    std::thread::spawn(move || {
        ::tracing::info!(
            ?canonical_path,
            "restart_index_in_path: spawned worker running"
        );

        // Inherit current picker's scanning flags when caller didn't pass
        // explicit opts — otherwise a `:cd ~` after init would silently lose
        // the user's `enable_home_dir_scanning = true` setting.
        let (follow_symlinks, fs_root, home_dir) = {
            let guard = match FILE_PICKER.read() {
                Ok(g) => g,
                Err(_) => return,
            };

            if let Some(ref picker) = *guard
                && picker.base_path() == canonical_path
            {
                tracing::info!(?canonical_path, "restart_index_in_path: same dir, skipping");
                return;
            }

            match guard.as_ref() {
                Some(p) => (
                    p.follows_symlinks() || opts.follow_symlinks,
                    p.fs_root_scanning_enabled() || opts.enable_fs_root_scanning,
                    p.home_dir_scanning_enabled() || opts.enable_home_dir_scanning,
                ),
                None => (
                    opts.follow_symlinks,
                    opts.enable_fs_root_scanning,
                    opts.enable_home_dir_scanning,
                ),
            }
        };

        ::tracing::info!(
            ?canonical_path,
            "restart_index_in_path: calling reinit_file_picker_internal"
        );

        // this will AUTOMATICALLY drop the old picker within a write lock inside the implementation
        // that will stop all the ongoing work and drop all the workeres
        if let Err(e) = FilePicker::new_with_shared_state(
            FILE_PICKER.clone(),
            FRECENCY.clone(),
            fff::FilePickerOptions {
                base_path: canonical_path.to_string_lossy().to_string(),
                enable_mmap_cache: true,
                enable_content_indexing: true,
                mode: FFFMode::Neovim,
                follow_symlinks,
                enable_fs_root_scanning: fs_root,
                enable_home_dir_scanning: home_dir,
                ..Default::default()
            },
        ) {
            tracing::error!(
                ?e,
                ?canonical_path,
                "Failed to index directory after changing"
            );
        }
    });

    Ok(())
}

pub fn scan_files(_: &Lua, _: ()) -> LuaResult<()> {
    // Async: spawns a BG thread that walks off-lock, swaps sync_data
    // under a brief write, then applies git + frecency off-lock. Returns
    // immediately so the lua/UI thread never waits on the walk.
    FILE_PICKER
        .trigger_full_rescan_async(&FRECENCY)
        .into_lua_result()?;
    ::tracing::info!("scan_files rescan spawned");
    Ok(())
}

#[allow(clippy::type_complexity)]
pub fn fuzzy_search_files(
    lua: &Lua,
    (
        query,
        max_threads,
        current_file,
        combo_boost_score_multiplier,
        min_combo_count,
        page_index,
        page_size,
    ): (
        String,
        usize,
        Option<String>,
        i32,
        Option<u32>,
        Option<usize>,
        Option<usize>,
    ),
) -> LuaResult<LuaValue> {
    let file_picker_guard = FILE_PICKER.read().into_lua_result()?;
    let Some(ref picker) = *file_picker_guard else {
        return Err(error::to_lua_error(Error::FilePickerMissing));
    };

    let base_path = picker.base_path();
    let min_combo_count = min_combo_count.unwrap_or(3);

    let query_tracker_guard = QUERY_TRACKER.read().into_lua_result()?;

    if query_tracker_guard.as_ref().is_none() {
        tracing::warn!("Query tracker not initialized");
    }

    tracing::debug!(
        ?base_path,
        ?query,
        ?min_combo_count,
        ?page_index,
        ?page_size,
        "Fuzzy search parameters"
    );

    let parsed_query = FFFQuery::parse(&query, FileSearchConfig);
    let results = picker.fuzzy_search(
        &parsed_query,
        query_tracker_guard.as_ref(),
        FuzzySearchOptions {
            max_threads,
            current_file: current_file.as_deref(),
            project_path: Some(picker.base_path()),
            combo_boost_score_multiplier,
            min_combo_count,
            pagination: PaginationArgs {
                offset: page_index.unwrap_or(0),
                limit: page_size.unwrap_or(0),
            },
        },
    );

    if results.items.is_empty() && query.contains(std::path::MAIN_SEPARATOR) {
        let pure_query = match &parsed_query.fuzzy_query {
            fff_query_parser::FuzzyQuery::Text(t) => t.trim(),
            _ => query.trim(),
        };

        let path = expand_tilde(pure_query);
        if path.is_absolute() && path.is_file() {
            if let Some(found_file) = picker.get_file_by_path(&path) {
                let found = SearchResult {
                    items: vec![found_file],
                    scores: vec![Score {
                        exact_match: true,
                        match_type: "path",
                        ..Default::default()
                    }],
                    match_byte_offsets: vec![Default::default()],
                    total_matched: 1,
                    total_files: results.total_files,
                    location: parsed_query.location,
                };

                return lua_types::SearchResultLua::new(found, picker).into_lua(lua);
            }

            return build_file_path_fallback(lua, &path, results.total_files);
        }
    }

    lua_types::SearchResultLua::new(results, picker).into_lua(lua)
}

#[allow(clippy::type_complexity)]
pub fn fuzzy_search_directories(
    lua: &Lua,
    (query, max_threads, current_file, page_index, page_size): (
        String,
        usize,
        Option<String>,
        Option<usize>,
        Option<usize>,
    ),
) -> LuaResult<LuaValue> {
    let file_picker_guard = FILE_PICKER.read().into_lua_result()?;
    let Some(ref picker) = *file_picker_guard else {
        return Err(error::to_lua_error(Error::FilePickerMissing));
    };

    let parser = QueryParser::new(DirSearchConfig);
    let parsed = parser.parse(&query);

    let results = picker.fuzzy_search_directories(
        &parsed,
        FuzzySearchOptions {
            max_threads,
            current_file: current_file.as_deref(),
            project_path: Some(picker.base_path()),
            combo_boost_score_multiplier: 0,
            min_combo_count: 0,
            pagination: PaginationArgs {
                offset: page_index.unwrap_or(0),
                limit: page_size.unwrap_or(0),
            },
        },
    );

    lua_types::DirSearchResultLua::new(results, picker).into_lua(lua)
}

#[allow(clippy::type_complexity)]
pub fn fuzzy_search_mixed(
    lua: &Lua,
    (
        query,
        max_threads,
        current_file,
        combo_boost_score_multiplier,
        min_combo_count,
        page_index,
        page_size,
    ): (
        String,
        usize,
        Option<String>,
        i32,
        Option<u32>,
        Option<usize>,
        Option<usize>,
    ),
) -> LuaResult<LuaValue> {
    let file_picker_guard = FILE_PICKER.read().into_lua_result()?;
    let Some(ref picker) = *file_picker_guard else {
        return Err(error::to_lua_error(Error::FilePickerMissing));
    };

    let query_tracker_guard = QUERY_TRACKER.read().into_lua_result()?;
    let parser = QueryParser::new(MixedSearchConfig);
    let parsed = parser.parse(&query);

    let results = picker.fuzzy_search_mixed(
        &parsed,
        query_tracker_guard.as_ref(),
        FuzzySearchOptions {
            max_threads,
            current_file: current_file.as_deref(),
            project_path: Some(picker.base_path()),
            combo_boost_score_multiplier,
            min_combo_count: min_combo_count.unwrap_or(3),
            pagination: PaginationArgs {
                offset: page_index.unwrap_or(0),
                limit: page_size.unwrap_or(0),
            },
        },
    );

    lua_types::MixedSearchResultLua::new(results, picker).into_lua(lua)
}

#[allow(clippy::type_complexity)]
pub fn live_grep(
    lua: &Lua,
    (
        query,
        file_offset,
        page_size,
        max_file_size,
        max_matches_per_file,
        smart_case,
        grep_mode,
        time_budget_ms,
        trim_whitespace,
        enforce_time_budget,
    ): (
        String,
        Option<usize>,
        Option<usize>,
        Option<u64>,
        Option<usize>,
        Option<bool>,
        Option<String>,
        Option<u64>,
        Option<bool>,
        Option<bool>,
    ),
) -> LuaResult<LuaValue> {
    let file_picker_guard = FILE_PICKER.read().into_lua_result()?;
    let Some(ref picker) = *file_picker_guard else {
        return Err(error::to_lua_error(Error::FilePickerMissing));
    };

    let parsed_query = FFFQuery::parse(&query, NvimGrepConfig);
    let mode = match grep_mode.as_deref() {
        Some("regex") => fff::GrepMode::Regex,
        Some("fuzzy") => fff::GrepMode::Fuzzy,
        _ => fff::GrepMode::PlainText, // "plain" or nil or unknown
    };

    let options = fff::GrepSearchOptions {
        max_file_size: max_file_size.unwrap_or(10 * 1024 * 1024),
        max_matches_per_file: max_matches_per_file.unwrap_or(200),
        smart_case: smart_case.unwrap_or(true),
        file_offset: file_offset.unwrap_or(0),
        page_limit: page_size.unwrap_or(50),
        mode,
        time_budget_ms: time_budget_ms.unwrap_or(0),
        enforce_time_budget: enforce_time_budget.unwrap_or(false),
        before_context: 0,
        after_context: 0,
        classify_definitions: false,
        trim_whitespace: trim_whitespace.unwrap_or(false),
        abort_signal: None,
    };

    let result = picker.grep(&parsed_query, &options);
    lua_types::GrepResultLua::new(result, picker).into_lua(lua)
}

/// Build a file-picker result for an absolute path that exists on disk but
/// isn't in the picker index (e.g. file from a different project).
fn build_file_path_fallback(lua: &Lua, path: &Path, total_files: usize) -> LuaResult<LuaValue> {
    let table = lua.create_table()?;

    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let path_str = path.to_string_lossy().to_string();

    let item = lua.create_table()?;
    item.set("relative_path", path_str.as_str())?;
    item.set("name", name.as_str())?;
    item.set("size", path.metadata().map(|m| m.len()).unwrap_or(0))?;
    item.set("modified", 0u64)?;
    item.set("access_frecency_score", 0i32)?;
    item.set("modification_frecency_score", 0i32)?;
    item.set("total_frecency_score", 0i32)?;
    item.set("git_status", "")?;
    item.set("is_binary", false)?;

    let items_table = lua.create_table()?;
    items_table.set(1, item)?;
    table.set("items", items_table)?;

    let score = lua.create_table()?;
    score.set("total", 0)?;
    score.set("base_score", 0)?;
    score.set("filename_bonus", 0)?;
    score.set("special_filename_bonus", 0)?;
    score.set("frecency_boost", 0)?;
    score.set("git_status_boost", 0)?;
    score.set("distance_penalty", 0)?;
    score.set("current_file_penalty", 0)?;
    score.set("combo_match_boost", 0)?;
    score.set("exact_match", true)?;
    score.set("match_type", "path")?;

    let scores_table = lua.create_table()?;
    scores_table.set(1, score)?;
    table.set("scores", scores_table)?;

    table.set("total_matched", 1)?;
    table.set("total_files", total_files)?;

    Ok(LuaValue::Table(table))
}

pub fn track_access(_: &Lua, file_path: String) -> LuaResult<bool> {
    // must be async and capture a local copy of the path because the write lock
    // is unsafe and can be held forever which is unavoidable, but at least we can
    // prevent users from deadlock of the main thread if someone deletes a lock
    let file_path = PathBuf::from(&file_path);
    std::thread::spawn(move || {
        {
            let frecency_guard = match FRECENCY.read() {
                Ok(g) => g,
                Err(e) => {
                    ::tracing::debug!(?e, "track_access: frecency read lock poisoned");
                    return;
                }
            };
            let Some(ref frecency) = *frecency_guard else {
                return;
            };
            if let Err(e) = frecency.track_access(file_path.as_path()) {
                ::tracing::debug!(?e, ?file_path, "track_access: frecency DB write failed");
                return;
            }
        }

        let mut file_picker = match FILE_PICKER.write() {
            Ok(g) => g,
            Err(e) => {
                ::tracing::debug!(?e, "track_access: file picker write lock poisoned");
                return;
            }
        };
        let Some(ref mut picker) = *file_picker else {
            return;
        };

        let frecency_guard = match FRECENCY.read() {
            Ok(g) => g,
            Err(e) => {
                ::tracing::debug!(?e, "track_access: frecency read lock poisoned on update");
                return;
            }
        };
        let Some(ref frecency) = *frecency_guard else {
            return;
        };
        if let Err(e) = picker.update_single_file_frecency(&file_path, frecency) {
            ::tracing::debug!(
                ?e,
                ?file_path,
                "track_access: update_single_file_frecency failed"
            );
        }
    });

    Ok(true)
}

pub fn get_scan_progress(lua: &Lua, _: ()) -> LuaResult<LuaValue> {
    let file_picker = FILE_PICKER.read().into_lua_result()?;
    let picker = file_picker
        .as_ref()
        .ok_or(Error::FilePickerMissing)
        .into_lua_result()?;
    let progress = picker.get_scan_progress();

    let table = lua.create_table()?;
    table.set("scanned_files_count", progress.scanned_files_count)?;
    table.set("is_scanning", progress.is_scanning)?;
    Ok(LuaValue::Table(table))
}

pub fn is_scanning(_: &Lua, _: ()) -> LuaResult<bool> {
    let file_picker = FILE_PICKER.read().into_lua_result()?;
    let picker = file_picker
        .as_ref()
        .ok_or(Error::FilePickerMissing)
        .into_lua_result()?;
    Ok(picker.is_scan_active())
}

pub fn get_git_root(_: &Lua, _: ()) -> LuaResult<Option<String>> {
    let file_picker = FILE_PICKER.read().into_lua_result()?;
    let Some(ref picker) = *file_picker else {
        return Ok(None);
    };

    Ok(picker.git_root().map(|p| p.to_string_lossy().into_owned()))
}

pub fn refresh_git_status(_: &Lua, _: ()) -> LuaResult<usize> {
    FILE_PICKER.refresh_git_status(&FRECENCY).into_lua_result()
}

pub fn get_file_access_count(_: &Lua, file_path: String) -> LuaResult<u64> {
    let path = PathBuf::from(&file_path);
    let frecency_guard = FRECENCY.read().into_lua_result()?;
    let Some(ref frecency) = *frecency_guard else {
        return Ok(0);
    };
    let count = frecency.access_count(&path).into_lua_result()?;
    Ok(count as u64)
}

pub fn update_single_file_frecency(_: &Lua, file_path: String) -> LuaResult<bool> {
    let frecency_guard = FRECENCY.read().into_lua_result()?;
    let Some(ref frecency) = *frecency_guard else {
        return Ok(false);
    };

    let mut file_picker = FILE_PICKER.write().into_lua_result()?;
    let Some(ref mut picker) = *file_picker else {
        return Err(error::to_lua_error(Error::FilePickerMissing));
    };

    picker
        .update_single_file_frecency(&file_path, frecency)
        .into_lua_result()?;
    Ok(true)
}

pub fn stop_background_monitor(_: &Lua, _: ()) -> LuaResult<bool> {
    // `stop_background_monitor` is non-blocking — the debouncer /
    // owner threads exit on their next iteration, so it is safe to
    // call under the FILE_PICKER write lock.
    let mut file_picker = FILE_PICKER.write().into_lua_result()?;
    let Some(ref mut picker) = *file_picker else {
        return Err(error::to_lua_error(Error::FilePickerMissing));
    };
    picker.stop_background_monitor();
    Ok(true)
}

pub fn cleanup_file_picker(_: &Lua, _: ()) -> LuaResult<bool> {
    let mut file_picker = FILE_PICKER.write().into_lua_result()?;
    if let Some(picker) = file_picker.take() {
        drop(picker);
        ::tracing::info!("FilePicker cleanup completed");
        Ok(true)
    } else {
        Ok(false)
    }
}

pub fn cancel_scan(_: &Lua, _: ()) -> LuaResult<bool> {
    Ok(true)
}

pub fn track_query_completion(_: &Lua, (query, file_path): (String, String)) -> LuaResult<bool> {
    // Everything runs on a background thread — including the
    // `FILE_PICKER.read()` used to fetch `project_path`. Doing that read
    // on the main (UI) thread stalled nvim whenever a reindex writer was
    // in flight: parking_lot's fair queue makes a read block behind a
    // pending writer, so the main thread would freeze for the full
    // duration of the scan.
    let query_tracker = QUERY_TRACKER.clone();
    std::thread::spawn(move || {
        let project_path = match FILE_PICKER.read() {
            Ok(guard) => match *guard {
                Some(ref picker) => picker.base_path().to_path_buf(),
                None => return,
            },
            Err(_) => return,
        };

        let file_path = match fff::path_utils::canonicalize(&file_path) {
            Ok(path) => path,
            Err(e) => {
                tracing::warn!(?file_path, error = ?e, "Failed to canonicalize file path for tracking");
                return;
            }
        };

        if let Ok(mut guard) = query_tracker.write()
            && let Some(tracker) = guard.as_mut()
            && let Err(e) = tracker.track_query_completion(&query, &project_path, &file_path)
        {
            tracing::error!(
                query = %query,
                file = %file_path.display(),
                error = ?e,
                "Failed to track query completion"
            );
        }
    });

    Ok(true)
}

pub fn get_historical_query(_: &Lua, offset: usize) -> LuaResult<Option<String>> {
    let project_path = {
        let file_picker = FILE_PICKER.read().into_lua_result()?;
        let Some(ref picker) = *file_picker else {
            return Ok(None);
        };
        picker.base_path().to_path_buf()
    };

    let query_tracker = QUERY_TRACKER.read().into_lua_result()?;
    let Some(ref tracker) = *query_tracker else {
        return Ok(None);
    };

    tracker
        .get_historical_query(&project_path, offset)
        .into_lua_result()
}

pub fn track_grep_query(_: &Lua, query: String) -> LuaResult<bool> {
    // Move the `FILE_PICKER.read()` into the spawned worker too —
    // see the note on `track_query_completion` above for why.
    let query_tracker = QUERY_TRACKER.clone();
    std::thread::spawn(move || {
        let project_path = match FILE_PICKER.read() {
            Ok(guard) => match *guard {
                Some(ref picker) => picker.base_path().to_path_buf(),
                None => return,
            },
            Err(_) => return,
        };

        if let Ok(mut guard) = query_tracker.write()
            && let Some(ref mut tracker) = *guard
            && let Err(e) = tracker.track_grep_query(&query, &project_path)
        {
            tracing::error!(
                query = %query,
                error = ?e,
                "Failed to track grep query"
            );
        }
    });

    Ok(true)
}

pub fn get_historical_grep_query(_: &Lua, offset: usize) -> LuaResult<Option<String>> {
    let project_path = {
        let file_picker = FILE_PICKER.read().into_lua_result()?;
        let Some(ref picker) = *file_picker else {
            return Ok(None);
        };
        picker.base_path().to_path_buf()
    };

    let query_tracker = QUERY_TRACKER.read().into_lua_result()?;
    let Some(ref tracker) = *query_tracker else {
        return Ok(None);
    };

    tracker
        .get_historical_grep_query(&project_path, offset)
        .into_lua_result()
}

/// Parse a grep query string and return its text portion (with constraints stripped).
///
/// Uses the Neovim grep parser config so filename-constraint detection follows
/// the user's setting, keeping Lua from re-implementing constraint detection.
pub fn parse_grep_query(lua: &Lua, query: String) -> LuaResult<LuaTable> {
    let parser = QueryParser::new(NvimGrepConfig);
    let parsed = parser.parse(&query);
    let table = lua.create_table()?;
    table.set("grep_text", parsed.grep_text())?;
    Ok(table)
}

pub fn wait_for_initial_scan(_: &Lua, timeout_ms: Option<u64>) -> LuaResult<bool> {
    let scanned = FILE_PICKER.wait_for_scan(Duration::from_millis(timeout_ms.unwrap_or(500)));

    Ok(scanned)
}

pub fn init_tracing(
    _: &Lua,
    (log_file_path, log_level, retain_runs): (String, Option<String>, Option<usize>),
) -> LuaResult<String> {
    crate::log::init_tracing(&log_file_path, log_level.as_deref(), retain_runs)
        .map_err(|e| LuaError::RuntimeError(format!("Failed to initialize tracing: {}", e)))
}

/// Returns health check information including version, git2 status, and repository detection
pub fn health_check(lua: &Lua, test_path: Option<String>) -> LuaResult<LuaValue> {
    let table = lua.create_table()?;
    table.set("version", env!("CARGO_PKG_VERSION"))?;

    let test_path = test_path
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

    let git_info = lua.create_table()?;
    let git_version = git2::Version::get();
    let (major, minor, rev) = git_version.libgit2_version();
    let libgit2_version_str = format!("{}.{}.{}", major, minor, rev);

    match git2::Repository::discover(&test_path) {
        Ok(repo) => {
            git_info.set("available", true)?;
            git_info.set("repository_found", true)?;
            if let Some(workdir) = repo.workdir() {
                git_info.set("workdir", workdir.to_string_lossy().to_string())?;
            }
            // Get git2 version info
            git_info.set("libgit2_version", libgit2_version_str.clone())?;
        }
        Err(e) => {
            git_info.set("available", true)?;
            git_info.set("repository_found", false)?;
            git_info.set("error", e.message().to_string())?;
            git_info.set("libgit2_version", libgit2_version_str)?;
        }
    }
    table.set("git", git_info)?;

    // Check file picker status
    let picker_info = lua.create_table()?;
    match FILE_PICKER.read() {
        Ok(guard) => {
            if let Some(ref picker) = *guard {
                picker_info.set("initialized", true)?;
                picker_info.set(
                    "base_path",
                    picker.base_path().to_string_lossy().to_string(),
                )?;
                picker_info.set("is_scanning", picker.is_scan_active())?;
                let progress = picker.get_scan_progress();
                picker_info.set("indexed_files", progress.scanned_files_count)?;
            } else {
                picker_info.set("initialized", false)?;
            }
        }
        Err(_) => {
            picker_info.set("initialized", false)?;
            picker_info.set("error", "Failed to acquire file picker lock")?;
        }
    }
    table.set("file_picker", picker_info)?;

    let frecency_info = lua.create_table()?;
    match FRECENCY.read() {
        Ok(guard) => {
            frecency_info.set("initialized", guard.is_some())?;

            if let Some(ref frecency) = *guard {
                match frecency.get_health() {
                    Ok(health) => {
                        let healthcheck_table = lua.create_table()?;
                        healthcheck_table.set("path", health.path)?;
                        healthcheck_table.set("disk_size", health.disk_size)?;
                        healthcheck_table.set("healthy", health.healthy)?;
                        for (name, count) in health.entry_counts {
                            healthcheck_table.set(name, count)?;
                        }
                        frecency_info.set("db_healthcheck", healthcheck_table)?;
                    }
                    Err(e) => {
                        frecency_info.set("db_healthcheck_error", e.to_string())?;
                    }
                }
            }
        }
        Err(_) => {
            frecency_info.set("initialized", false)?;
            frecency_info.set("error", "Failed to acquire frecency lock")?;
        }
    }
    table.set("frecency", frecency_info)?;

    let query_tracker_info = lua.create_table()?;
    match QUERY_TRACKER.read() {
        Ok(guard) => {
            query_tracker_info.set("initialized", guard.is_some())?;
            if let Some(ref query_history) = *guard {
                match query_history.get_health() {
                    Ok(health) => {
                        let healthcheck_table = lua.create_table()?;
                        healthcheck_table.set("path", health.path)?;
                        healthcheck_table.set("disk_size", health.disk_size)?;
                        healthcheck_table.set("healthy", health.healthy)?;
                        for (name, count) in health.entry_counts {
                            healthcheck_table.set(name, count)?;
                        }
                        query_tracker_info.set("db_healthcheck", healthcheck_table)?;
                    }
                    Err(e) => {
                        query_tracker_info.set("db_healthcheck_error", e.to_string())?;
                    }
                }
            }
        }
        Err(_) => {
            query_tracker_info.set("initialized", false)?;
            query_tracker_info.set("error", "Failed to acquire query tracker lock")?;
        }
    }
    table.set("query_tracker", query_tracker_info)?;

    Ok(LuaValue::Table(table))
}

pub fn shorten_path(
    _: &Lua,
    (path, max_size, strategy): (String, usize, Option<mlua::Value>),
) -> LuaResult<String> {
    let strategy = strategy
        .map(|v| -> LuaResult<PathShortenStrategy> {
            match v {
                mlua::Value::String(ref s) => {
                    let name = s
                        .to_str()
                        .map(|s| s.to_owned())
                        .unwrap_or_else(|_| "middle_number".to_string());
                    Ok(PathShortenStrategy::from_name(&name))
                }
                _ => Ok(PathShortenStrategy::default()),
            }
        })
        .transpose()?
        .unwrap_or_default();

    shorten_path_with_cache(strategy, max_size, Path::new(&path)).map_err(LuaError::RuntimeError)
}

fn create_exports(lua: &Lua) -> LuaResult<LuaTable> {
    let exports = lua.create_table()?;
    exports.set("init_db", lua.create_function(init_db)?)?;
    exports.set(
        "destroy_frecency_db",
        lua.create_function(destroy_frecency_db)?,
    )?;
    exports.set("init_file_picker", lua.create_function(init_file_picker)?)?;
    exports.set(
        "restart_index_in_path",
        lua.create_function(restart_index_in_path)?,
    )?;
    exports.set("scan_files", lua.create_function(scan_files)?)?;
    exports.set(
        "fuzzy_search_files",
        lua.create_function(fuzzy_search_files)?,
    )?;
    exports.set(
        "fuzzy_search_directories",
        lua.create_function(fuzzy_search_directories)?,
    )?;
    exports.set(
        "fuzzy_search_mixed",
        lua.create_function(fuzzy_search_mixed)?,
    )?;
    exports.set("live_grep", lua.create_function(live_grep)?)?;
    exports.set("track_access", lua.create_function(track_access)?)?;
    exports.set(
        "get_file_access_count",
        lua.create_function(get_file_access_count)?,
    )?;
    exports.set("cancel_scan", lua.create_function(cancel_scan)?)?;
    exports.set("get_scan_progress", lua.create_function(get_scan_progress)?)?;
    exports.set(
        "refresh_git_status",
        lua.create_function(refresh_git_status)?,
    )?;
    exports.set("get_git_root", lua.create_function(get_git_root)?)?;
    exports.set(
        "stop_background_monitor",
        lua.create_function(stop_background_monitor)?,
    )?;
    exports.set("init_tracing", lua.create_function(init_tracing)?)?;
    exports.set(
        "wait_for_initial_scan",
        lua.create_function(wait_for_initial_scan)?,
    )?;
    exports.set(
        "cleanup_file_picker",
        lua.create_function(cleanup_file_picker)?,
    )?;
    exports.set("destroy_query_db", lua.create_function(destroy_query_db)?)?;
    exports.set(
        "track_query_completion",
        lua.create_function(track_query_completion)?,
    )?;
    exports.set(
        "get_historical_query",
        lua.create_function(get_historical_query)?,
    )?;
    exports.set("track_grep_query", lua.create_function(track_grep_query)?)?;
    exports.set(
        "get_historical_grep_query",
        lua.create_function(get_historical_grep_query)?,
    )?;
    exports.set("health_check", lua.create_function(health_check)?)?;
    exports.set("shorten_path", lua.create_function(shorten_path)?)?;
    exports.set("hex_dump", lua.create_function(hex_dump::hex_dump)?)?;
    exports.set("parse_grep_query", lua.create_function(parse_grep_query)?)?;

    Ok(exports)
}

// https://github.com/mlua-rs/mlua/issues/318
#[mlua::lua_module(skip_memory_check)]
fn fff_nvim(lua: &Lua) -> LuaResult<LuaTable> {
    // Install panic hook + SIGSEGV chain handler IMMEDIATELY on module load.
    // Without this, a crash inside fff produces a silent nvim death — neovim's
    // own handler does not log a Rust-side backtrace.
    crate::log::install_panic_hook();

    create_exports(lua)
}
