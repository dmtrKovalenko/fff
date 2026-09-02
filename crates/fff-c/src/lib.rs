//! C FFI bindings for fff-core, usable from any language with C FFI
//! (Bun, Node.js, Python, Ruby, etc.).
//!
//! All state is owned by an opaque instance handle: create with
//! `fff_create_instance*`, pass to every call, free with `fff_destroy`.
//! Multiple instances can coexist in one process.
//!
//! Conventions: every returned `*mut FffResult` is freed with
//! `fff_free_result`; optional string params take NULL/empty; numeric 0 means
//! "use default" unless documented otherwise; grep mode `u8` is 0 = plain
//! text, 1 = regex, 2 = fuzzy; multi-grep patterns are `\n`-separated.

use std::ffi::{CStr, CString, c_char, c_void};
use std::path::PathBuf;
use std::time::Duration;

use fff::shared::SharedQueryTracker;

mod accessors;
mod ffi_types;
mod watch;

use fff::file_picker::FilePicker;
use fff::frecency::FrecencyTracker;
use fff::query_tracker::QueryTracker;
use fff::{DbHealthChecker, FFFMode, FuzzySearchOptions, PaginationArgs, QueryParser};
use fff::{SharedFilePicker, SharedFrecency};
use ffi_types::{
    FFF_CREATE_OPTIONS_VERSION, FffCreateOptions, FffDirItem, FffDirSearchResult, FffFileItem,
    FffGrepMatch, FffGrepResult, FffMixedItem, FffMixedSearchResult, FffResult, FffScanProgress,
    FffScore, FffSearchResult,
};

/// Opaque handle holding all per-instance state; freed by `fff_destroy`.
struct FffInstance {
    picker: SharedFilePicker,
    frecency: SharedFrecency,
    query_tracker: SharedQueryTracker,
    // we keep a single callback type
    watch_callback: std::sync::Arc<watch::WatchCallbackSlot>,
}

/// Convert a C string to `&str`; `None` if null or invalid UTF-8.
pub(crate) unsafe fn cstr_to_str<'a>(s: *const c_char) -> Option<&'a str> {
    if s.is_null() {
        None
    } else {
        unsafe { CStr::from_ptr(s).to_str().ok() }
    }
}

/// Optional C string param: `None` if null, empty, or invalid UTF-8.
unsafe fn optional_cstr<'a>(s: *const c_char) -> Option<&'a str> {
    unsafe { cstr_to_str(s) }.filter(|s| !s.is_empty())
}

/// Recover a `&FffInstance` from the opaque pointer; error `FffResult` if null.
pub(crate) unsafe fn instance_ref<'a>(
    fff_handle: *mut c_void,
) -> Result<&'a FffInstance, *mut FffResult> {
    if fff_handle.is_null() {
        Err(FffResult::err(
            "Instance handle is null. Create one with fff_create_instance first.",
        ))
    } else {
        Ok(unsafe { &*(fff_handle as *const FffInstance) })
    }
}

/// Decode a `u8` grep mode into the core enum.
fn grep_mode_from_u8(mode: u8) -> fff::GrepMode {
    match mode {
        1 => fff::GrepMode::Regex,
        2 => fff::GrepMode::Fuzzy,
        _ => fff::GrepMode::PlainText,
    }
}

/// Apply "0 means default" convention.
fn default_u32(val: u32, default: u32) -> u32 {
    if val == 0 { default } else { val }
}

fn default_u64(val: u64, default: u64) -> u64 {
    if val == 0 { default } else { val }
}

fn default_i32(val: i32, default: i32) -> i32 {
    if val == 0 { default } else { val }
}

/// Create a new file finder instance (legacy 8-arg positional signature).
///
/// @deprecated Use [`fff_create_instance_with`] (or [`fff_create_instance_with_value`]
/// for FFI bindings). The `use_unsafe_no_lock` parameter is ignored.
///
/// ## Safety
/// See `fff_create_instance_with`.
#[deprecated(
    since = "0.8.5",
    note = "Use fff_create_instance_with (by pointer) or fff_create_instance_with_value (by value) with FffCreateOptions instead. The struct evolves without ABI breaks."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_create_instance(
    base_path: *const c_char,
    frecency_db_path: *const c_char,
    history_db_path: *const c_char,
    _use_unsafe_no_lock: bool,
    enable_mmap_cache: bool,
    enable_content_indexing: bool,
    watch: bool,
    ai_mode: bool,
) -> *mut FffResult {
    let mut opts = FffCreateOptions::defaults();
    opts.base_path = base_path;
    opts.frecency_db_path = frecency_db_path;
    opts.history_db_path = history_db_path;
    opts.enable_mmap_cache = enable_mmap_cache;
    opts.enable_content_indexing = enable_content_indexing;
    opts.watch = watch;
    opts.ai_mode = ai_mode;
    unsafe { fff_create_instance_with(&opts as *const FffCreateOptions) }
}

/// Create a new file finder instance (legacy 13-arg positional signature).
///
/// @deprecated Use [`fff_create_instance_with`] (or [`fff_create_instance_with_value`]
/// for FFI bindings). The `use_unsafe_no_lock` parameter is ignored.
///
/// ## Safety
/// See `fff_create_instance_with`.
#[deprecated(
    since = "0.8.5",
    note = "Use fff_create_instance_with (by pointer) or fff_create_instance_with_value (by value) with FffCreateOptions instead. The struct evolves without ABI breaks."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_create_instance2(
    base_path: *const c_char,
    frecency_db_path: *const c_char,
    history_db_path: *const c_char,
    _use_unsafe_no_lock: bool,
    enable_mmap_cache: bool,
    enable_content_indexing: bool,
    watch: bool,
    ai_mode: bool,
    log_file_path: *const c_char,
    log_level: *const c_char,
    cache_budget_max_files: u64,
    cache_budget_max_bytes: u64,
    cache_budget_max_file_size: u64,
) -> *mut FffResult {
    let mut opts = FffCreateOptions::defaults();
    opts.base_path = base_path;
    opts.frecency_db_path = frecency_db_path;
    opts.history_db_path = history_db_path;
    opts.enable_mmap_cache = enable_mmap_cache;
    opts.enable_content_indexing = enable_content_indexing;
    opts.watch = watch;
    opts.ai_mode = ai_mode;
    opts.log_file_path = log_file_path;
    opts.log_level = log_level;
    opts.cache_budget_max_files = cache_budget_max_files;
    opts.cache_budget_max_bytes = cache_budget_max_bytes;
    opts.cache_budget_max_file_size = cache_budget_max_file_size;
    unsafe { fff_create_instance_with(&opts as *const FffCreateOptions) }
}

/// Create a new file finder instance from a versioned [`FffCreateOptions`] struct.
///
/// Populate the struct, set `version` to [`FFF_CREATE_OPTIONS_VERSION`], pass by
/// pointer. New fields are only appended; older `version` values keep working.
/// FFI bindings needing struct-by-value should use [`fff_create_instance_with_value`].
///
/// `opts.base_path` is required (non-NULL, non-empty). Zero `cache_budget_*`
/// values are auto-computed from repo size after the initial scan.
///
/// ## Safety
/// * `opts` must be a valid pointer to an `FffCreateOptions` whose `version`
///   is in the range `1..=FFF_CREATE_OPTIONS_VERSION`.
/// * All string pointers inside `opts` must be valid null-terminated UTF-8
///   or NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_create_instance_with(opts: *const FffCreateOptions) -> *mut FffResult {
    if opts.is_null() {
        return FffResult::err("opts is null");
    }
    let opts = unsafe { &*opts };
    if opts.version == 0 || opts.version > FFF_CREATE_OPTIONS_VERSION {
        return FffResult::err(&format!(
            "Unsupported FffCreateOptions version {} (library understands up to {})",
            opts.version, FFF_CREATE_OPTIONS_VERSION
        ));
    }

    let base_path_str = match unsafe { cstr_to_str(opts.base_path) } {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return FffResult::err("opts.base_path is null or empty"),
    };

    if let Some(log_path) = unsafe { optional_cstr(opts.log_file_path) } {
        let level = unsafe { optional_cstr(opts.log_level) };
        if let Err(e) = fff::log::init_tracing(log_path, level, None) {
            return FffResult::err(&format!("Failed to init tracing: {}", e));
        }
    }

    let frecency_path = unsafe { optional_cstr(opts.frecency_db_path) }.map(|s| s.to_string());
    let history_path = unsafe { optional_cstr(opts.history_db_path) }.map(|s| s.to_string());

    let shared_picker = SharedFilePicker::default();
    let shared_frecency = SharedFrecency::default();
    let query_tracker = SharedQueryTracker::default();

    if let Some(ref frecency_path) = frecency_path {
        if let Some(parent) = PathBuf::from(frecency_path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        match FrecencyTracker::open(frecency_path) {
            Ok(tracker) => {
                if let Err(e) = shared_frecency.init(tracker) {
                    return FffResult::err(&format!("Failed to acquire frecency lock: {}", e));
                }
            }
            Err(e) => return FffResult::err(&format!("Failed to init frecency db: {}", e)),
        }
    }

    if let Some(ref history_path) = history_path {
        if let Some(parent) = PathBuf::from(history_path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        match QueryTracker::open(history_path) {
            Ok(tracker) => {
                if let Err(e) = query_tracker.init(tracker) {
                    return FffResult::err(&format!("Failed to acquire query tracker lock: {}", e));
                }
            }
            Err(e) => return FffResult::err(&format!("Failed to init query tracker db: {}", e)),
        }
    }

    let mode = if opts.ai_mode {
        FFFMode::Ai
    } else {
        FFFMode::Neovim
    };

    let cache_budget = fff::ContentCacheBudget::from_overrides(
        opts.cache_budget_max_files as usize,
        opts.cache_budget_max_bytes,
        opts.cache_budget_max_file_size,
    );

    if let Err(e) = FilePicker::new_with_shared_state(
        shared_picker.clone(),
        shared_frecency.clone(),
        fff::FilePickerOptions {
            base_path: base_path_str,
            enable_mmap_cache: opts.enable_mmap_cache,
            enable_content_indexing: opts.enable_content_indexing,
            watch: opts.watch,
            mode,
            cache_budget,
            follow_symlinks: opts.version >= 2 && opts.follow_symlinks,
            enable_fs_root_scanning: opts.enable_fs_root_scanning,
            enable_home_dir_scanning: opts.enable_home_dir_scanning,
        },
    ) {
        return FffResult::err(&format!("Failed to init file picker: {}", e));
    }

    let instance = Box::new(FffInstance {
        picker: shared_picker,
        frecency: shared_frecency,
        query_tracker,
        watch_callback: std::sync::Arc::new(watch::WatchCallbackSlot::default()),
    });

    let fff_handle = Box::into_raw(instance) as *mut c_void;
    FffResult::ok_handle(fff_handle)
}

/// [`fff_create_instance_with`] adapter taking [`FffCreateOptions`] **by value**,
/// for FFI libraries that pass native structs by value (e.g. Node's `ffi-rs`).
///
/// ## Safety
/// All `*const c_char` fields inside `opts` must be valid null-terminated
/// UTF-8 or NULL. The struct itself is consumed by value.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_create_instance_with_value(opts: FffCreateOptions) -> *mut FffResult {
    unsafe { fff_create_instance_with(&opts as *const FffCreateOptions) }
}

/// Destroy a file finder instance and free all its resources.
///
/// ## Safety
/// `fff_handle` must be a valid pointer returned by `fff_create_instance`, or null (no-op).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_destroy(fff_handle: *mut c_void) {
    if fff_handle.is_null() {
        return;
    }

    let instance = unsafe { Box::from_raw(fff_handle as *mut FffInstance) };

    // The C callback and user_data may be freed as soon as this returns.
    instance.picker.shutdown_watches_and_wait();
    instance.watch_callback.clear();

    if let Ok(mut guard) = instance.picker.write()
        && let Some(picker) = guard.take()
    {
        drop(picker);
    }

    if let Ok(mut guard) = instance.frecency.write() {
        *guard = None;
    }
    if let Ok(mut guard) = instance.query_tracker.write() {
        *guard = None;
    }
}

/// Perform fuzzy search on indexed files.
///
/// `current_file` deprioritizes the currently open file (NULL/empty to skip).
/// Zero picks the default: `max_threads` auto, `page_size` 100,
/// `combo_boost_multiplier` 100, `min_combo_count` 3.
///
/// ## Safety
/// * `fff_handle` must be a valid instance pointer from `fff_create_instance`.
/// * `query` and `current_file` must be valid null-terminated UTF-8 strings or NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_search(
    fff_handle: *mut c_void,
    query: *const c_char,
    current_file: *const c_char,
    max_threads: u32,
    page_index: u32,
    page_size: u32,
    combo_boost_multiplier: i32,
    min_combo_count: u32,
) -> *mut FffResult {
    let inst = match unsafe { instance_ref(fff_handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };

    let query_str = match unsafe { cstr_to_str(query) } {
        Some(s) => s,
        None => return FffResult::err("Query is null or invalid UTF-8"),
    };

    let current_file_str = unsafe { optional_cstr(current_file) };
    let page_size = default_u32(page_size, 100) as usize;
    let min_combo_count = default_u32(min_combo_count, 3);
    let combo_boost_multiplier = default_i32(combo_boost_multiplier, 100);

    let picker_guard = match inst.picker.read() {
        Ok(g) => g,
        Err(e) => return FffResult::err(&format!("Failed to acquire file picker lock: {}", e)),
    };

    let picker = match picker_guard.as_ref() {
        Some(p) => p,
        None => {
            return FffResult::err("File picker not initialized. Call fff_create_instance first.");
        }
    };

    // Get query tracker ref for combo matching
    let qt_guard = match inst.query_tracker.read() {
        Ok(q) => q,
        Err(_) => return FffResult::err("Failed to acquire query tracker lock"),
    };
    let query_tracker_ref = qt_guard.as_ref();

    let parser = QueryParser::default();
    let parsed = parser.parse(query_str);

    let results = picker.fuzzy_search(
        &parsed,
        query_tracker_ref,
        FuzzySearchOptions {
            max_threads: max_threads as usize,
            current_file: current_file_str,
            project_path: Some(picker.base_path()),
            combo_boost_score_multiplier: combo_boost_multiplier,
            min_combo_count,
            pagination: PaginationArgs {
                offset: page_index as usize,
                limit: page_size,
            },
        },
    );

    let search_result = FffSearchResult::from_core(&results, picker);
    FffResult::ok_handle(search_result as *mut c_void)
}

/// Glob-only search: filter indexed files by a single glob pattern (passed
/// through verbatim, no query parsing), rank by frecency, and paginate.
///
/// `current_file` deprioritizes the currently open file (NULL/empty to skip).
/// Zero picks the default: `max_threads` auto, `page_size` 100.
///
/// ## Safety
/// * `fff_handle` must be a valid instance pointer from `fff_create_instance`.
/// * `pattern` and `current_file` must be valid null-terminated UTF-8 strings or NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_glob(
    fff_handle: *mut c_void,
    pattern: *const c_char,
    current_file: *const c_char,
    max_threads: u32,
    page_index: u32,
    page_size: u32,
) -> *mut FffResult {
    let inst = match unsafe { instance_ref(fff_handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };

    let pattern_str = match unsafe { cstr_to_str(pattern) } {
        Some(s) if !s.is_empty() => s,
        _ => return FffResult::err("Pattern is null, empty, or invalid UTF-8"),
    };

    let current_file_str = unsafe { optional_cstr(current_file) };
    let page_size = default_u32(page_size, 100) as usize;

    let picker_guard = match inst.picker.read() {
        Ok(g) => g,
        Err(e) => return FffResult::err(&format!("Failed to acquire file picker lock: {}", e)),
    };

    let picker = match picker_guard.as_ref() {
        Some(p) => p,
        None => {
            return FffResult::err("File picker not initialized. Call fff_create_instance first.");
        }
    };

    let results = picker.glob(
        pattern_str,
        FuzzySearchOptions {
            max_threads: max_threads as usize,
            current_file: current_file_str,
            project_path: Some(picker.base_path()),
            combo_boost_score_multiplier: 0,
            min_combo_count: 0,
            pagination: PaginationArgs {
                offset: page_index as usize,
                limit: page_size,
            },
        },
    );

    let search_result = FffSearchResult::from_core(&results, picker);
    FffResult::ok_handle(search_result as *mut c_void)
}

/// Perform fuzzy search on indexed directories.
///
/// `current_file` is used for distance scoring (NULL/empty to skip).
/// Zero picks the default: `max_threads` auto, `page_size` 100.
///
/// ## Safety
/// * `fff_handle` must be a valid instance pointer from `fff_create_instance`.
/// * `query` and `current_file` must be valid null-terminated UTF-8 strings or NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_search_directories(
    fff_handle: *mut c_void,
    query: *const c_char,
    current_file: *const c_char,
    max_threads: u32,
    page_index: u32,
    page_size: u32,
) -> *mut FffResult {
    let inst = match unsafe { instance_ref(fff_handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };

    let query_str = match unsafe { cstr_to_str(query) } {
        Some(s) => s,
        None => return FffResult::err("Query is null or invalid UTF-8"),
    };

    let current_file_str = unsafe { optional_cstr(current_file) };
    let page_size = default_u32(page_size, 100) as usize;

    let picker_guard = match inst.picker.read() {
        Ok(g) => g,
        Err(e) => return FffResult::err(&format!("Failed to acquire file picker lock: {}", e)),
    };

    let picker = match picker_guard.as_ref() {
        Some(p) => p,
        None => {
            return FffResult::err("File picker not initialized. Call fff_create_instance first.");
        }
    };

    let parser = QueryParser::new(fff_query_parser::DirSearchConfig);
    let parsed = parser.parse(query_str);

    let results = picker.fuzzy_search_directories(
        &parsed,
        FuzzySearchOptions {
            max_threads: max_threads as usize,
            current_file: current_file_str,
            project_path: Some(picker.base_path()),
            combo_boost_score_multiplier: 0,
            min_combo_count: 0,
            pagination: PaginationArgs {
                offset: page_index as usize,
                limit: page_size,
            },
        },
    );

    let dir_result = FffDirSearchResult::from_core(&results, picker);
    FffResult::ok_handle(dir_result as *mut c_void)
}

/// Perform a mixed fuzzy search across both files and directories.
///
/// Returns one flat list interleaved by descending total score; each item's
/// `item_type` is 0 = file, 1 = directory. Parameters as in [`fff_search`].
///
/// ## Safety
/// * `fff_handle` must be a valid instance pointer from `fff_create_instance`.
/// * `query` and `current_file` must be valid null-terminated UTF-8 strings or NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_search_mixed(
    fff_handle: *mut c_void,
    query: *const c_char,
    current_file: *const c_char,
    max_threads: u32,
    page_index: u32,
    page_size: u32,
    combo_boost_multiplier: i32,
    min_combo_count: u32,
) -> *mut FffResult {
    let inst = match unsafe { instance_ref(fff_handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };

    let query_str = match unsafe { cstr_to_str(query) } {
        Some(s) => s,
        None => return FffResult::err("Query is null or invalid UTF-8"),
    };

    let current_file_str = unsafe { optional_cstr(current_file) };
    let page_size = default_u32(page_size, 100) as usize;
    let min_combo_count = default_u32(min_combo_count, 3);
    let combo_boost_multiplier = default_i32(combo_boost_multiplier, 100);

    let picker_guard = match inst.picker.read() {
        Ok(g) => g,
        Err(e) => return FffResult::err(&format!("Failed to acquire file picker lock: {}", e)),
    };

    let picker = match picker_guard.as_ref() {
        Some(p) => p,
        None => {
            return FffResult::err("File picker not initialized. Call fff_create_instance first.");
        }
    };

    let qt_guard = match inst.query_tracker.read() {
        Ok(q) => q,
        Err(_) => return FffResult::err("Failed to acquire query tracker lock"),
    };
    let query_tracker_ref = qt_guard.as_ref();

    let parser = QueryParser::new(fff_query_parser::MixedSearchConfig);
    let parsed = parser.parse(query_str);

    let results = picker.fuzzy_search_mixed(
        &parsed,
        query_tracker_ref,
        FuzzySearchOptions {
            max_threads: max_threads as usize,
            current_file: current_file_str,
            project_path: Some(picker.base_path()),
            combo_boost_score_multiplier: combo_boost_multiplier,
            min_combo_count,
            pagination: PaginationArgs {
                offset: page_index as usize,
                limit: page_size,
            },
        },
    );

    let mixed_result = FffMixedSearchResult::from_core(&results, picker);
    FffResult::ok_handle(mixed_result as *mut c_void)
}

/// Perform content search (grep) across indexed files.
///
/// `query` supports constraint syntax like `*.rs pattern`; `mode` is
/// 0 = plain text (SIMD), 1 = regex, 2 = fuzzy. Zero picks the default:
/// `max_file_size` 10 MB, `page_limit` 50, `max_matches_per_file` and
/// `time_budget_ms` unlimited. `smart_case` is case-insensitive for
/// all-lowercase queries; `classify_definitions` tags code definitions.
///
/// ## Safety
/// * `fff_handle` must be a valid instance pointer from `fff_create_instance`.
/// * `query` must be a valid null-terminated UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_live_grep(
    fff_handle: *mut c_void,
    query: *const c_char,
    mode: u8,
    max_file_size: u64,
    max_matches_per_file: u32,
    smart_case: bool,
    file_offset: u32,
    page_limit: u32,
    time_budget_ms: u64,
    before_context: u32,
    after_context: u32,
    classify_definitions: bool,
) -> *mut FffResult {
    unsafe {
        fff_live_grep_ex(
            fff_handle,
            query,
            mode,
            max_file_size,
            max_matches_per_file,
            smart_case,
            file_offset,
            page_limit,
            time_budget_ms,
            false,
            before_context,
            after_context,
            classify_definitions,
        )
    }
}

/// [`fff_live_grep`] plus `enforce_time_budget`: when true the budget also bounds
/// zero-match searches and `next_file_offset` resumes at the first unsearched file.
///
/// ## Safety
/// Same as [`fff_live_grep`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_live_grep_ex(
    fff_handle: *mut c_void,
    query: *const c_char,
    mode: u8,
    max_file_size: u64,
    max_matches_per_file: u32,
    smart_case: bool,
    file_offset: u32,
    page_limit: u32,
    time_budget_ms: u64,
    enforce_time_budget: bool,
    before_context: u32,
    after_context: u32,
    classify_definitions: bool,
) -> *mut FffResult {
    let inst = match unsafe { instance_ref(fff_handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };

    let query_str = match unsafe { cstr_to_str(query) } {
        Some(s) => s,
        None => return FffResult::err("Query is null or invalid UTF-8"),
    };

    let picker_guard = match inst.picker.read() {
        Ok(g) => g,
        Err(e) => return FffResult::err(&format!("Failed to acquire file picker lock: {}", e)),
    };

    let picker = match picker_guard.as_ref() {
        Some(p) => p,
        None => {
            return FffResult::err("File picker not initialized. Call fff_create_instance first.");
        }
    };

    let is_ai = picker.mode().is_ai();
    let parsed = if is_ai {
        fff::QueryParser::new(fff_query_parser::AiGrepConfig).parse(query_str)
    } else {
        fff::grep::parse_grep_query(query_str)
    };

    let options = fff::GrepSearchOptions {
        max_file_size: default_u64(max_file_size, 10 * 1024 * 1024),
        max_matches_per_file: max_matches_per_file as usize,
        smart_case,
        file_offset: file_offset as usize,
        page_limit: default_u32(page_limit, 50) as usize,
        mode: grep_mode_from_u8(mode),
        time_budget_ms,
        enforce_time_budget,
        before_context: before_context as usize,
        after_context: after_context as usize,
        classify_definitions,
        trim_whitespace: false,
        abort_signal: None,
    };

    let result = picker.grep(&parsed, &options);
    let grep_result = FffGrepResult::from_core(&result, picker);
    FffResult::ok_handle(grep_result as *mut c_void)
}

/// Multi-pattern OR search (SIMD Aho-Corasick): lines matching ANY pattern.
///
/// `patterns_joined` is `\n`-separated (e.g. `"foo\nbar"`); `constraints` is an
/// optional file filter like `"*.rs"` or `"/src/"` (NULL/empty to skip).
/// Remaining parameters as in [`fff_live_grep`].
///
/// ## Safety
/// * `fff_handle` must be a valid instance pointer from `fff_create_instance`.
/// * `patterns_joined` and `constraints` must be valid null-terminated UTF-8 or NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_multi_grep(
    fff_handle: *mut c_void,
    patterns_joined: *const c_char,
    constraints: *const c_char,
    max_file_size: u64,
    max_matches_per_file: u32,
    smart_case: bool,
    file_offset: u32,
    page_limit: u32,
    time_budget_ms: u64,
    before_context: u32,
    after_context: u32,
    classify_definitions: bool,
) -> *mut FffResult {
    unsafe {
        fff_multi_grep_ex(
            fff_handle,
            patterns_joined,
            constraints,
            max_file_size,
            max_matches_per_file,
            smart_case,
            file_offset,
            page_limit,
            time_budget_ms,
            false,
            before_context,
            after_context,
            classify_definitions,
        )
    }
}

/// [`fff_multi_grep`] plus `enforce_time_budget`, as in [`fff_live_grep_ex`].
///
/// ## Safety
/// Same as [`fff_multi_grep`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_multi_grep_ex(
    fff_handle: *mut c_void,
    patterns_joined: *const c_char,
    constraints: *const c_char,
    max_file_size: u64,
    max_matches_per_file: u32,
    smart_case: bool,
    file_offset: u32,
    page_limit: u32,
    time_budget_ms: u64,
    enforce_time_budget: bool,
    before_context: u32,
    after_context: u32,
    classify_definitions: bool,
) -> *mut FffResult {
    let inst = match unsafe { instance_ref(fff_handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };

    let patterns_str = match unsafe { cstr_to_str(patterns_joined) } {
        Some(s) if !s.is_empty() => s,
        _ => return FffResult::err("patterns_joined is null or empty"),
    };

    let patterns: Vec<&str> = patterns_str.split('\n').collect();
    if patterns.is_empty() || patterns.iter().all(|p| p.is_empty()) {
        return FffResult::err("patterns must not be empty");
    }

    let constraints_str = unsafe { optional_cstr(constraints) };

    let picker_guard = match inst.picker.read() {
        Ok(g) => g,
        Err(e) => return FffResult::err(&format!("Failed to acquire file picker lock: {}", e)),
    };

    let picker = match picker_guard.as_ref() {
        Some(p) => p,
        None => {
            return FffResult::err("File picker not initialized. Call fff_create_instance first.");
        }
    };

    // Parse constraints from the optional string (e.g. "*.rs /src/")
    let parsed_constraints = constraints_str
        .map(|c| fff::QueryParser::new(fff_query_parser::AiGrepConfig).parse_constraints(c));

    let constraint_refs: &[fff::Constraint<'_>] = match &parsed_constraints {
        Some(constraints) => constraints,
        None => &[],
    };

    let options = fff::GrepSearchOptions {
        max_file_size: default_u64(max_file_size, 10 * 1024 * 1024),
        max_matches_per_file: max_matches_per_file as usize,
        smart_case,
        file_offset: file_offset as usize,
        page_limit: default_u32(page_limit, 50) as usize,
        mode: fff::GrepMode::PlainText, // ignored by multi_grep_search
        time_budget_ms,
        enforce_time_budget,
        before_context: before_context as usize,
        after_context: after_context as usize,
        classify_definitions,
        trim_whitespace: false,
        abort_signal: None,
    };

    let result = picker.multi_grep(&patterns, constraint_refs, &options);
    let grep_result = FffGrepResult::from_core(&result, picker);
    FffResult::ok_handle(grep_result as *mut c_void)
}

/// Trigger a rescan of the file index.
///
/// ## Safety
/// `fff_handle` must be a valid instance pointer from `fff_create_instance`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_scan_files(fff_handle: *mut c_void) -> *mut FffResult {
    let inst = match unsafe { instance_ref(fff_handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };

    // Async: rescan runs on a BG thread, caller returns immediately.
    // Use `fff_is_scanning` / `fff_wait_for_scan` to observe progress.
    match inst.picker.trigger_full_rescan_async(&inst.frecency) {
        Ok(()) => FffResult::ok_empty(),
        Err(e) => FffResult::err(&format!("Failed to trigger rescan: {}", e)),
    }
}

/// Check if a scan is currently in progress.
///
/// ## Safety
/// `fff_handle` must be a valid instance pointer from `fff_create_instance`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_is_scanning(fff_handle: *mut c_void) -> bool {
    let inst = match unsafe { instance_ref(fff_handle) } {
        Ok(i) => i,
        Err(_) => return false,
    };

    inst.picker
        .read()
        .ok()
        .and_then(|guard| guard.as_ref().map(|p| p.is_scan_active()))
        .unwrap_or(false)
}

/// Get the picker's base path as a heap C string in `handle`;
/// free it with `fff_free_string`.
///
/// ## Safety
/// `fff_handle` must be a valid instance pointer from `fff_create_instance`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_get_base_path(fff_handle: *mut c_void) -> *mut FffResult {
    let inst = match unsafe { instance_ref(fff_handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };

    let guard = match inst.picker.read() {
        Ok(g) => g,
        Err(e) => return FffResult::err(&format!("Failed to acquire file picker lock: {}", e)),
    };

    let picker = match guard.as_ref() {
        Some(p) => p,
        None => return FffResult::err("File picker not initialized"),
    };

    FffResult::ok_string(&picker.base_path().to_string_lossy())
}

/// Get scan progress information.
///
/// ## Safety
/// `fff_handle` must be a valid instance pointer from `fff_create_instance`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_get_scan_progress(fff_handle: *mut c_void) -> *mut FffResult {
    let inst = match unsafe { instance_ref(fff_handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };

    let guard = match inst.picker.read() {
        Ok(g) => g,
        Err(e) => return FffResult::err(&format!("Failed to acquire file picker lock: {}", e)),
    };

    let picker = match guard.as_ref() {
        Some(p) => p,
        None => return FffResult::err("File picker not initialized"),
    };

    let result = Box::into_raw(Box::new(FffScanProgress::from(picker.get_scan_progress())));
    FffResult::ok_handle(result as *mut c_void)
}

/// Wait for initial scan to complete.
///
/// ## Safety
/// `fff_handle` must be a valid instance pointer from `fff_create_instance`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_wait_for_scan(
    fff_handle: *mut c_void,
    timeout_ms: u64,
) -> *mut FffResult {
    let FffInstance { picker, .. } = match unsafe { instance_ref(fff_handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };

    let completed = picker.wait_for_scan(Duration::from_millis(timeout_ms));
    FffResult::ok_int(completed as i64)
}

/// Wait for the background file watcher to be ready.
///
/// ## Safety
/// `fff_handle` must be a valid instance pointer from `fff_create_instance`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_wait_for_watcher(
    fff_handle: *mut c_void,
    timeout_ms: u64,
) -> *mut FffResult {
    let inst = match unsafe { instance_ref(fff_handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };

    let completed = inst
        .picker
        .wait_for_watcher(Duration::from_millis(timeout_ms));
    FffResult::ok_int(completed as i64)
}

/// Restart indexing in a new directory.
///
/// ## Safety
/// * `fff_handle` must be a valid instance pointer from `fff_create_instance`.
/// * `new_path` must be a valid null-terminated UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_restart_index(
    fff_handle: *mut c_void,
    new_path: *const c_char,
) -> *mut FffResult {
    let inst = match unsafe { instance_ref(fff_handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };

    let path_str = match unsafe { cstr_to_str(new_path) } {
        Some(s) => s,
        None => return FffResult::err("Path is null or invalid UTF-8"),
    };

    let path = PathBuf::from(&path_str);
    if !path.exists() {
        return FffResult::err(&format!("Path does not exist: {}", path_str));
    }

    let canonical_path = match fff::path_utils::canonicalize(&path) {
        Ok(p) => p,
        Err(e) => return FffResult::err(&format!("Failed to canonicalize path: {}", e)),
    };

    let guard = match inst.picker.write() {
        Ok(g) => g,
        Err(e) => return FffResult::err(&format!("Failed to acquire file picker lock: {}", e)),
    };

    let (warmup_caches, content_indexing, watch, mode, fs_root, home_dir, follow_symlinks) =
        if let Some(ref picker) = *guard {
            (
                picker.has_mmap_cache(),
                picker.has_content_indexing(),
                picker.has_watcher(),
                picker.mode(),
                picker.fs_root_scanning_enabled(),
                picker.home_dir_scanning_enabled(),
                picker.follows_symlinks(),
            )
        } else {
            (false, true, true, FFFMode::default(), false, false, false)
        };

    drop(guard);

    match FilePicker::new_with_shared_state(
        inst.picker.clone(),
        inst.frecency.clone(),
        fff::FilePickerOptions {
            base_path: canonical_path.to_string_lossy().to_string(),
            enable_mmap_cache: warmup_caches,
            enable_content_indexing: content_indexing,
            watch,
            mode,
            cache_budget: None,
            follow_symlinks,
            enable_fs_root_scanning: fs_root,
            enable_home_dir_scanning: home_dir,
        },
    ) {
        Ok(()) => FffResult::ok_empty(),
        Err(e) => FffResult::err(&format!("Failed to init file picker: {}", e)),
    }
}

/// Refresh git status cache.
///
/// ## Safety
/// `fff_handle` must be a valid instance pointer from `fff_create_instance`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_refresh_git_status(fff_handle: *mut c_void) -> *mut FffResult {
    let inst = match unsafe { instance_ref(fff_handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };

    match inst.picker.refresh_git_status(&inst.frecency) {
        Ok(count) => FffResult::ok_int(count as i64),
        Err(e) => FffResult::err(&format!("Failed to refresh git status: {}", e)),
    }
}

/// Track query completion for smart suggestions.
///
/// ## Safety
/// * `fff_handle` must be a valid instance pointer from `fff_create_instance`.
/// * `query` and `file_path` must be valid null-terminated UTF-8 strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_track_query(
    fff_handle: *mut c_void,
    query: *const c_char,
    file_path: *const c_char,
) -> *mut FffResult {
    let inst = match unsafe { instance_ref(fff_handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };

    let query_str = match unsafe { cstr_to_str(query) } {
        Some(s) => s,
        None => return FffResult::err("Query is null or invalid UTF-8"),
    };

    let path_str = match unsafe { cstr_to_str(file_path) } {
        Some(s) => s,
        None => return FffResult::err("File path is null or invalid UTF-8"),
    };

    let file_path = match fff::path_utils::canonicalize(path_str) {
        Ok(p) => p,
        Err(e) => return FffResult::err(&format!("Failed to canonicalize path: {}", e)),
    };

    let project_path = {
        let guard = match inst.picker.read() {
            Ok(g) => g,
            Err(_) => return FffResult::ok_int(0),
        };
        match guard.as_ref() {
            Some(p) => p.base_path().to_path_buf(),
            None => return FffResult::ok_int(0),
        }
    };

    let mut qt_guard = match inst.query_tracker.write() {
        Ok(q) => q,
        Err(_) => return FffResult::ok_int(0),
    };

    if let Some(ref mut tracker) = *qt_guard
        && let Err(e) = tracker.track_query_completion(query_str, &project_path, &file_path)
    {
        return FffResult::err(&format!("Failed to track query: {}", e));
    }

    FffResult::ok_int(1)
}

/// Get historical query by offset (0 = most recent).
///
/// ## Safety
/// `fff_handle` must be a valid instance pointer from `fff_create_instance`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_get_historical_query(
    fff_handle: *mut c_void,
    offset: u64,
) -> *mut FffResult {
    let inst = match unsafe { instance_ref(fff_handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };

    let project_path = {
        let guard = match inst.picker.read() {
            Ok(g) => g,
            Err(_) => return FffResult::ok_empty(),
        };
        match guard.as_ref() {
            Some(p) => p.base_path().to_path_buf(),
            None => return FffResult::ok_empty(),
        }
    };

    let qt_guard = match inst.query_tracker.read() {
        Ok(q) => q,
        Err(_) => return FffResult::ok_empty(),
    };

    let tracker = match qt_guard.as_ref() {
        Some(t) => t,
        None => return FffResult::ok_empty(),
    };

    match tracker.get_historical_query(&project_path, offset as usize) {
        Ok(Some(query)) => FffResult::ok_string(&query),
        Ok(None) => FffResult::ok_empty(),
        Err(e) => FffResult::err(&format!("Failed to get historical query: {}", e)),
    }
}

/// Get health check information.
///
/// ## Safety
/// * `fff_handle` must be a valid instance pointer from `fff_create_instance`, or null for
///   a limited health check (version + git only).
/// * `test_path` can be null or a valid null-terminated UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_health_check(
    fff_handle: *mut c_void,
    test_path: *const c_char,
) -> *mut FffResult {
    let test_path = unsafe { optional_cstr(test_path) }
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

    let mut health = serde_json::Map::new();
    health.insert(
        "version".to_string(),
        serde_json::Value::String(env!("CARGO_PKG_VERSION").to_string()),
    );

    // Git info
    let mut git_info = serde_json::Map::new();
    let git_version = git2::Version::get();
    let (major, minor, rev) = git_version.libgit2_version();
    git_info.insert(
        "libgit2_version".to_string(),
        serde_json::Value::String(format!("{}.{}.{}", major, minor, rev)),
    );

    match git2::Repository::discover(&test_path) {
        Ok(repo) => {
            git_info.insert("available".to_string(), serde_json::Value::Bool(true));
            git_info.insert(
                "repository_found".to_string(),
                serde_json::Value::Bool(true),
            );
            if let Some(workdir) = repo.workdir() {
                git_info.insert(
                    "workdir".to_string(),
                    serde_json::Value::String(workdir.to_string_lossy().to_string()),
                );
            }
        }
        Err(e) => {
            git_info.insert("available".to_string(), serde_json::Value::Bool(true));
            git_info.insert(
                "repository_found".to_string(),
                serde_json::Value::Bool(false),
            );
            git_info.insert(
                "error".to_string(),
                serde_json::Value::String(e.message().to_string()),
            );
        }
    }
    health.insert("git".to_string(), serde_json::Value::Object(git_info));

    let inst: Option<&FffInstance> = if fff_handle.is_null() {
        None
    } else {
        Some(unsafe { &*(fff_handle as *const FffInstance) })
    };

    // File picker info
    let mut picker_info = serde_json::Map::new();
    if let Some(inst) = inst {
        match inst.picker.read() {
            Ok(guard) => {
                if let Some(ref picker) = *guard {
                    picker_info.insert("initialized".to_string(), serde_json::Value::Bool(true));
                    picker_info.insert(
                        "base_path".to_string(),
                        serde_json::Value::String(picker.base_path().to_string_lossy().to_string()),
                    );
                    picker_info.insert(
                        "is_scanning".to_string(),
                        serde_json::Value::Bool(picker.is_scan_active()),
                    );
                    let progress = picker.get_scan_progress();
                    picker_info.insert(
                        "indexed_files".to_string(),
                        serde_json::Value::Number(progress.scanned_files_count.into()),
                    );
                } else {
                    picker_info.insert("initialized".to_string(), serde_json::Value::Bool(false));
                }
            }
            Err(_) => {
                picker_info.insert("initialized".to_string(), serde_json::Value::Bool(false));
                picker_info.insert(
                    "error".to_string(),
                    serde_json::Value::String("Failed to acquire lock".to_string()),
                );
            }
        }
    } else {
        picker_info.insert("initialized".to_string(), serde_json::Value::Bool(false));
    }
    health.insert(
        "file_picker".to_string(),
        serde_json::Value::Object(picker_info),
    );

    // Frecency info
    let mut frecency_info = serde_json::Map::new();
    if let Some(inst) = inst {
        match inst.frecency.read() {
            Ok(guard) => {
                frecency_info.insert(
                    "initialized".to_string(),
                    serde_json::Value::Bool(guard.is_some()),
                );
                if let Some(ref frecency) = *guard
                    && let Ok(health_data) = frecency.get_health()
                {
                    let mut db_health = serde_json::Map::new();
                    db_health.insert(
                        "path".to_string(),
                        serde_json::Value::String(health_data.path),
                    );
                    db_health.insert(
                        "disk_size".to_string(),
                        serde_json::Value::Number(health_data.disk_size.into()),
                    );
                    frecency_info.insert(
                        "db_healthcheck".to_string(),
                        serde_json::Value::Object(db_health),
                    );
                }
            }
            Err(_) => {
                frecency_info.insert("initialized".to_string(), serde_json::Value::Bool(false));
            }
        }
    } else {
        frecency_info.insert("initialized".to_string(), serde_json::Value::Bool(false));
    }
    health.insert(
        "frecency".to_string(),
        serde_json::Value::Object(frecency_info),
    );

    // Query tracker info
    let mut query_info = serde_json::Map::new();
    if let Some(inst) = inst {
        match inst.query_tracker.read() {
            Ok(guard) => {
                query_info.insert(
                    "initialized".to_string(),
                    serde_json::Value::Bool(guard.is_some()),
                );
                if let Some(ref tracker) = *guard
                    && let Ok(health_data) = tracker.get_health()
                {
                    let mut db_health = serde_json::Map::new();
                    db_health.insert(
                        "path".to_string(),
                        serde_json::Value::String(health_data.path),
                    );
                    db_health.insert(
                        "disk_size".to_string(),
                        serde_json::Value::Number(health_data.disk_size.into()),
                    );
                    query_info.insert(
                        "db_healthcheck".to_string(),
                        serde_json::Value::Object(db_health),
                    );
                }
            }
            Err(_) => {
                query_info.insert("initialized".to_string(), serde_json::Value::Bool(false));
            }
        }
    } else {
        query_info.insert("initialized".to_string(), serde_json::Value::Bool(false));
    }
    health.insert(
        "query_tracker".to_string(),
        serde_json::Value::Object(query_info),
    );

    match serde_json::to_string(&health) {
        Ok(json) => FffResult::ok_string(&json),
        Err(e) => FffResult::err(&format!("Failed to serialize health check: {}", e)),
    }
}

/// Free a search result returned by `fff_search`: the struct, its `items`
/// and `scores` arrays, and every string within.
///
/// ## Safety
/// `result` must be a valid pointer previously returned via `FffResult.handle`
/// from `fff_search`, or null (no-op).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_free_search_result(result: *mut FffSearchResult) {
    if result.is_null() {
        return;
    }

    unsafe {
        let result = Box::from_raw(result);
        let count = result.count as usize;

        if !result.items.is_null() {
            let mut items = Vec::from_raw_parts(result.items, count, count);
            for item in &mut items {
                item.free_strings();
            }
        }
        if !result.scores.is_null() {
            let mut scores = Vec::from_raw_parts(result.scores, count, count);
            for score in &mut scores {
                score.free_strings();
            }
        }
    }
}

/// Pointer to the `index`-th `FffFileItem`; null if `result` is null or
/// `index >= count`. Valid until the search result is freed.
///
/// ## Safety
/// `result` must be a valid `FffSearchResult` pointer from `fff_search`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_search_result_get_item(
    result: *const FffSearchResult,
    index: u32,
) -> *const FffFileItem {
    if result.is_null() {
        return std::ptr::null();
    }
    let result = unsafe { &*result };
    if index >= result.count || result.items.is_null() {
        return std::ptr::null();
    }
    unsafe { result.items.add(index as usize) }
}

/// Pointer to the `index`-th `FffScore`; null if `result` is null or
/// `index >= count`. Valid until the search result is freed.
///
/// ## Safety
/// `result` must be a valid `FffSearchResult` pointer from `fff_search`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_search_result_get_score(
    result: *const FffSearchResult,
    index: u32,
) -> *const FffScore {
    if result.is_null() {
        return std::ptr::null();
    }
    let result = unsafe { &*result };
    if index >= result.count || result.scores.is_null() {
        return std::ptr::null();
    }
    unsafe { result.scores.add(index as usize) }
}

/// Free a grep result returned by `fff_live_grep` or `fff_multi_grep`:
/// the struct, its `items` array, and all strings/ranges/context within.
///
/// ## Safety
/// `result` must be a valid pointer previously returned via `FffResult.handle`
/// from `fff_live_grep` or `fff_multi_grep`, or null (no-op).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_free_grep_result(result: *mut FffGrepResult) {
    if result.is_null() {
        return;
    }

    unsafe {
        let result = Box::from_raw(result);
        let count = result.count as usize;

        if !result.items.is_null() {
            let mut items = Vec::from_raw_parts(result.items, count, count);
            for item in &mut items {
                item.free_fields();
            }
        }
        if !result.regex_fallback_error.is_null() {
            drop(CString::from_raw(result.regex_fallback_error));
        }
    }
}

/// Pointer to the `index`-th `FffGrepMatch`; null if `result` is null or
/// `index >= count`. Valid until the grep result is freed.
///
/// ## Safety
/// `result` must be a valid `FffGrepResult` pointer from `fff_live_grep` or `fff_multi_grep`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_grep_result_get_match(
    result: *const FffGrepResult,
    index: u32,
) -> *const FffGrepMatch {
    if result.is_null() {
        return std::ptr::null();
    }
    let result = unsafe { &*result };
    if index >= result.count || result.items.is_null() {
        return std::ptr::null();
    }
    unsafe { result.items.add(index as usize) }
}

/// Free a scan progress result returned by `fff_get_scan_progress`.
///
/// ## Safety
/// `result` must be a valid pointer previously returned via `FffResult.handle`
/// from `fff_get_scan_progress`, or null (no-op).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_free_scan_progress(result: *mut FffScanProgress) {
    if !result.is_null() {
        unsafe { drop(Box::from_raw(result)) };
    }
}

/// Offset a pointer by `byte_offset` bytes (FFI array iteration helper).
/// Returns null if `base` is null.
///
/// ## Safety
/// The resulting pointer must be within the bounds of the original allocation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_ptr_offset(base: *const c_void, byte_offset: usize) -> *const c_void {
    if base.is_null() {
        return std::ptr::null();
    }
    unsafe { (base as *const u8).add(byte_offset) as *const c_void }
}

/// Free a result envelope returned by any `fff_*` function.
/// **IMPORTANT:** the `handle` payload is NOT freed release it separately
/// using handle specific cleaning methods (`fff_destroy`, `fff_free_search_result`, etc.).
///
/// ## Safety
/// `result_ptr` must be a valid pointer returned by a `fff_*` function.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_free_result(result_ptr: *mut FffResult) {
    if result_ptr.is_null() {
        return;
    }

    unsafe {
        let result = Box::from_raw(result_ptr);
        if !result.error.is_null() {
            drop(CString::from_raw(result.error));
        }

        // note: handle is not freed by design
    }
}

/// Free a string returned by `fff_*` functions.
///
/// ## Safety
/// `s` must be a valid C string allocated by this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_free_string(s: *mut c_char) {
    unsafe {
        if !s.is_null() {
            drop(CString::from_raw(s));
        }
    }
}

// ---------------------------------------------------------------------------
// Directory search: free and accessor functions
// ---------------------------------------------------------------------------

/// Free a directory search result returned by `fff_search_directories`.
///
/// ## Safety
/// `result` must be a valid pointer previously returned via `FffResult.handle`
/// from `fff_search_directories`, or null (no-op).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_free_dir_search_result(result: *mut FffDirSearchResult) {
    if result.is_null() {
        return;
    }

    unsafe {
        let result = Box::from_raw(result);
        let count = result.count as usize;

        if !result.items.is_null() {
            let mut items = Vec::from_raw_parts(result.items, count, count);
            for item in &mut items {
                item.free_strings();
            }
        }
        if !result.scores.is_null() {
            let mut scores = Vec::from_raw_parts(result.scores, count, count);
            for score in &mut scores {
                score.free_strings();
            }
        }
    }
}

/// Get a pointer to the `index`-th `FffDirItem` in a directory search result.
///
/// ## Safety
/// `result` must be a valid `FffDirSearchResult` pointer from `fff_search_directories`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_dir_search_result_get_item(
    result: *const FffDirSearchResult,
    index: u32,
) -> *const FffDirItem {
    if result.is_null() {
        return std::ptr::null();
    }
    let result = unsafe { &*result };
    if index >= result.count || result.items.is_null() {
        return std::ptr::null();
    }
    unsafe { result.items.add(index as usize) }
}

/// Get a pointer to the `index`-th `FffScore` in a directory search result.
///
/// ## Safety
/// `result` must be a valid `FffDirSearchResult` pointer from `fff_search_directories`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_dir_search_result_get_score(
    result: *const FffDirSearchResult,
    index: u32,
) -> *const FffScore {
    if result.is_null() {
        return std::ptr::null();
    }
    let result = unsafe { &*result };
    if index >= result.count || result.scores.is_null() {
        return std::ptr::null();
    }
    unsafe { result.scores.add(index as usize) }
}

// ---------------------------------------------------------------------------
// Mixed search: free and accessor functions
// ---------------------------------------------------------------------------

/// Free a mixed search result returned by `fff_search_mixed`.
///
/// ## Safety
/// `result` must be a valid pointer previously returned via `FffResult.handle`
/// from `fff_search_mixed`, or null (no-op).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_free_mixed_search_result(result: *mut FffMixedSearchResult) {
    if result.is_null() {
        return;
    }

    unsafe {
        let result = Box::from_raw(result);
        let count = result.count as usize;

        if !result.items.is_null() {
            let mut items = Vec::from_raw_parts(result.items, count, count);
            for item in &mut items {
                item.free_strings();
            }
        }
        if !result.scores.is_null() {
            let mut scores = Vec::from_raw_parts(result.scores, count, count);
            for score in &mut scores {
                score.free_strings();
            }
        }
    }
}

/// Get a pointer to the `index`-th `FffMixedItem` in a mixed search result.
///
/// ## Safety
/// `result` must be a valid `FffMixedSearchResult` pointer from `fff_search_mixed`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_mixed_search_result_get_item(
    result: *const FffMixedSearchResult,
    index: u32,
) -> *const FffMixedItem {
    if result.is_null() {
        return std::ptr::null();
    }
    let result = unsafe { &*result };
    if index >= result.count || result.items.is_null() {
        return std::ptr::null();
    }
    unsafe { result.items.add(index as usize) }
}

/// Get a pointer to the `index`-th `FffScore` in a mixed search result.
///
/// ## Safety
/// `result` must be a valid `FffMixedSearchResult` pointer from `fff_search_mixed`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_mixed_search_result_get_score(
    result: *const FffMixedSearchResult,
    index: u32,
) -> *const FffScore {
    if result.is_null() {
        return std::ptr::null();
    }
    let result = unsafe { &*result };
    if index >= result.count || result.scores.is_null() {
        return std::ptr::null();
    }
    unsafe { result.scores.add(index as usize) }
}
