use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use fff::file_picker::FilePicker;
use fff::frecency::FrecencyTracker;
use fff::query_tracker::QueryTracker;
use fff::{
    FFFMode, FilePickerOptions, FuzzySearchOptions, GrepSearchOptions, PaginationArgs, QueryParser,
    SharedFilePicker, SharedFrecency, SharedQueryTracker,
};
use pyo3::exceptions::PyTypeError;
use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::conversions::MixedItem;
use crate::types::{
    DirItem, DirSearchResult, FileItem, GrepCursor, GrepMatch, GrepResult, MixedDirItem,
    MixedFileItem, MixedSearchResult, ScanProgress, Score, SearchResult, WatchEvent,
};
use crate::watch::WatchSubscription;
use crate::{parse_grep_mode, py_err};

const DEFAULT_SEARCH_PAGE_SIZE: usize = 100;
// Sentinel-to-default conversion for combo boosting, mirroring the C/Node
// bindings: `0` means "use the engine default", not "disable".
const DEFAULT_COMBO_BOOST_MULTIPLIER: i32 = 100;
const DEFAULT_MIN_COMBO_COUNT: u32 = 3;

fn defaulted_usize(value: u32, default: usize) -> usize {
    if value == 0 { default } else { value as usize }
}

fn defaulted_u64(value: u64, default: u64) -> u64 {
    if value == 0 { default } else { value }
}

fn defaulted_i32(value: i32, default: i32) -> i32 {
    if value == 0 { default } else { value }
}

fn defaulted_u32(value: u32, default: u32) -> u32 {
    if value == 0 { default } else { value }
}

fn create_parent_dir(path: &Path) -> PyResult<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(py_err)?;
    }
    Ok(())
}

fn pagination_args(page_index: u32, page_size: u32) -> PaginationArgs {
    PaginationArgs {
        offset: page_index as usize,
        limit: defaulted_usize(page_size, DEFAULT_SEARCH_PAGE_SIZE),
    }
}

fn fuzzy_options<'a>(
    max_threads: u32,
    current_file: Option<&'a str>,
    project_path: &'a Path,
    page_index: u32,
    page_size: u32,
    combo_boost_score_multiplier: i32,
    min_combo_count: u32,
) -> FuzzySearchOptions<'a> {
    FuzzySearchOptions {
        max_threads: max_threads as usize,
        current_file,
        project_path: Some(project_path),
        combo_boost_score_multiplier,
        min_combo_count,
        pagination: pagination_args(page_index, page_size),
    }
}

#[allow(clippy::too_many_arguments)]
fn grep_options(
    mode: fff::GrepMode,
    cursor_offset: usize,
    max_file_size: u64,
    max_matches_per_file: u32,
    smart_case: bool,
    page_limit: u32,
    time_budget_ms: u64,
    before_context: u32,
    after_context: u32,
    classify_definitions: bool,
) -> GrepSearchOptions {
    let defaults = GrepSearchOptions::default();
    GrepSearchOptions {
        max_file_size: defaulted_u64(max_file_size, defaults.max_file_size),
        max_matches_per_file: max_matches_per_file as usize,
        smart_case,
        file_offset: cursor_offset,
        page_limit: defaulted_usize(page_limit, defaults.page_limit),
        mode,
        time_budget_ms,
        before_context: before_context as usize,
        after_context: after_context as usize,
        classify_definitions,
        trim_whitespace: false,
        abort_signal: None,
    }
}

fn convert_scores(scores: &[fff::Score]) -> Vec<Score> {
    scores.iter().map(Score::from).collect()
}

fn convert_grep_result(result: fff::grep::GrepResult<'_>, picker: &FilePicker) -> GrepResult {
    let items = result
        .matches
        .iter()
        .map(|m| GrepMatch::from((m, result.files[m.file_index], picker)))
        .collect();

    GrepResult {
        items,
        total_matched: result.matches.len() as u32,
        total_files_searched: result.total_files_searched as u32,
        total_files: result.total_files as u32,
        filtered_file_count: result.filtered_file_count as u32,
        next_file_offset: result.next_file_offset as u32,
        regex_fallback_error: result.regex_fallback_error,
    }
}

fn clear_shared_state(
    picker: &SharedFilePicker,
    frecency: &SharedFrecency,
    query_tracker: &SharedQueryTracker,
) {
    if let Ok(mut guard) = picker.write() {
        guard.take();
    }
    if let Ok(mut guard) = frecency.write() {
        *guard = None;
    }
    if let Ok(mut guard) = query_tracker.write() {
        *guard = None;
    }
}

#[pyclass(subclass)]
pub struct FileFinder {
    picker: SharedFilePicker,
    frecency: SharedFrecency,
    query_tracker: SharedQueryTracker,
    cache_budget_max_files: usize,
    cache_budget_max_bytes: u64,
    cache_budget_max_file_size: u64,
}

impl Drop for FileFinder {
    fn drop(&mut self) {
        clear_shared_state(&self.picker, &self.frecency, &self.query_tracker);
    }
}

#[pymethods]
impl FileFinder {
    #[new]
    #[pyo3(signature = (
        base_path,
        *,
        frecency_db_path=None,
        history_db_path=None,
        enable_mmap_cache=true,
        enable_content_indexing=true,
        watch=true,
        ai_mode=false,
        log_file_path=None,
        log_level=None,
        cache_budget_max_files=0,
        cache_budget_max_bytes=0,
        cache_budget_max_file_size=0,
        enable_fs_root_scanning=false,
        enable_home_dir_scanning=false,
        follow_symlinks=false,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        py: Python<'_>,
        base_path: PathBuf,
        frecency_db_path: Option<PathBuf>,
        history_db_path: Option<PathBuf>,
        enable_mmap_cache: bool,
        enable_content_indexing: bool,
        watch: bool,
        ai_mode: bool,
        log_file_path: Option<PathBuf>,
        log_level: Option<String>,
        cache_budget_max_files: u64,
        cache_budget_max_bytes: u64,
        cache_budget_max_file_size: u64,
        enable_fs_root_scanning: bool,
        enable_home_dir_scanning: bool,
        follow_symlinks: bool,
    ) -> PyResult<Self> {
        let shared_picker = SharedFilePicker::default();
        let shared_frecency = SharedFrecency::default();
        let query_tracker = SharedQueryTracker::default();
        let cache_budget_max_files = cache_budget_max_files as usize;

        let init_picker = shared_picker.clone();
        let init_frecency = shared_frecency.clone();
        let init_query_tracker = query_tracker.clone();

        py.detach(move || -> PyResult<()> {
            if let Some(path) = frecency_db_path {
                create_parent_dir(&path)?;
                let tracker = FrecencyTracker::open(&path).map_err(py_err)?;
                init_frecency.init(tracker).map_err(py_err)?;
            }

            if let Some(path) = history_db_path {
                create_parent_dir(&path)?;
                let tracker = QueryTracker::open(&path).map_err(py_err)?;
                init_query_tracker.init(tracker).map_err(py_err)?;
            }

            if let Some(path) = log_file_path {
                create_parent_dir(&path)?;
                let path = path.to_string_lossy();
                fff::log::init_tracing(&path, log_level.as_deref(), None).map_err(py_err)?;
            }

            let mode = if ai_mode {
                FFFMode::Ai
            } else {
                FFFMode::Neovim
            };

            FilePicker::new_with_shared_state(
                init_picker,
                init_frecency,
                FilePickerOptions {
                    base_path: base_path.to_string_lossy().to_string(),
                    enable_mmap_cache,
                    enable_content_indexing,
                    watch,
                    mode,
                    cache_budget: fff::ContentCacheBudget::from_overrides(
                        cache_budget_max_files,
                        cache_budget_max_bytes,
                        cache_budget_max_file_size,
                    ),
                    follow_symlinks,
                    enable_fs_root_scanning,
                    enable_home_dir_scanning,
                    git_recency: Default::default(),
                },
            )
            .map_err(py_err)
        })?;

        Ok(Self {
            picker: shared_picker,
            frecency: shared_frecency,
            query_tracker,
            cache_budget_max_files,
            cache_budget_max_bytes,
            cache_budget_max_file_size,
        })
    }

    fn __enter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __exit__(
        &mut self,
        py: Python<'_>,
        _exc_type: Py<PyAny>,
        _exc_value: Py<PyAny>,
        _traceback: Py<PyAny>,
    ) {
        let _ = self.close(py);
    }

    fn close(&mut self, py: Python<'_>) -> PyResult<()> {
        // Release the GIL while an in-flight callback finishes.
        let picker = self.picker.clone();
        py.detach(move || picker.shutdown_watches_and_wait());
        clear_shared_state(&self.picker, &self.frecency, &self.query_tracker);
        Ok(())
    }

    #[getter]
    fn closed(&self) -> PyResult<bool> {
        Ok(self.picker.read().map_err(py_err)?.is_none())
    }

    #[getter]
    fn base_path(&self) -> PyResult<Option<String>> {
        let guard = self.picker.read().map_err(py_err)?;
        Ok(guard
            .as_ref()
            .map(|p| p.base_path().to_string_lossy().to_string()))
    }

    #[getter]
    fn scan_progress(&self) -> PyResult<ScanProgress> {
        let guard = self.picker.read().map_err(py_err)?;
        let picker = guard
            .as_ref()
            .ok_or_else(|| py_err("File picker not initialized"))?;
        let p = picker.get_scan_progress();
        Ok(ScanProgress {
            scanned_files_count: p.scanned_files_count as u64,
            is_scanning: p.is_scanning,
            is_watcher_ready: p.is_watcher_ready,
            is_warmup_complete: p.is_warmup_complete,
        })
    }

    fn __repr__(&self) -> PyResult<String> {
        let guard = self.picker.read().map_err(py_err)?;
        if let Some(ref picker) = *guard {
            Ok(format!(
                "FileFinder(base_path={:?}, closed=False)",
                picker.base_path().to_string_lossy()
            ))
        } else {
            Ok("FileFinder(closed=True)".to_string())
        }
    }

    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (
        query,
        *,
        current_file=None,
        max_threads=0,
        page_index=0,
        page_size=0,
        combo_boost_score_multiplier=0,
        min_combo_count=0,
    ))]
    fn search(
        &self,
        py: Python<'_>,
        query: &str,
        current_file: Option<String>,
        max_threads: u32,
        page_index: u32,
        page_size: u32,
        combo_boost_score_multiplier: i32,
        min_combo_count: u32,
    ) -> PyResult<SearchResult> {
        let picker = self.picker.clone();
        let query_tracker = self.query_tracker.clone();
        let query = query.to_string();

        py.detach(move || -> PyResult<_> {
            let picker_guard = picker.read().map_err(py_err)?;
            let picker = picker_guard
                .as_ref()
                .ok_or_else(|| py_err("File picker not initialized"))?;
            let qt_guard = query_tracker.read().map_err(py_err)?;

            let parsed = QueryParser::default().parse(&query);
            let result = picker.fuzzy_search(
                &parsed,
                qt_guard.as_ref(),
                fuzzy_options(
                    max_threads,
                    current_file.as_deref(),
                    picker.base_path(),
                    page_index,
                    page_size,
                    defaulted_i32(combo_boost_score_multiplier, DEFAULT_COMBO_BOOST_MULTIPLIER),
                    defaulted_u32(min_combo_count, DEFAULT_MIN_COMBO_COUNT),
                ),
            );

            Ok(SearchResult {
                items: result
                    .items
                    .iter()
                    .map(|i| FileItem::from((*i, picker)))
                    .collect(),
                scores: convert_scores(&result.scores),
                total_matched: result.total_matched as u32,
                total_files: result.total_files as u32,
            })
        })
    }

    #[pyo3(signature = (
        pattern,
        *,
        current_file=None,
        max_threads=0,
        page_index=0,
        page_size=0,
    ))]
    fn glob(
        &self,
        py: Python<'_>,
        pattern: &str,
        current_file: Option<String>,
        max_threads: u32,
        page_index: u32,
        page_size: u32,
    ) -> PyResult<SearchResult> {
        let picker = self.picker.clone();
        let pattern = pattern.to_string();

        py.detach(move || -> PyResult<_> {
            let picker_guard = picker.read().map_err(py_err)?;
            let picker = picker_guard
                .as_ref()
                .ok_or_else(|| py_err("File picker not initialized"))?;

            let result = picker.glob(
                &pattern,
                fuzzy_options(
                    max_threads,
                    current_file.as_deref(),
                    picker.base_path(),
                    page_index,
                    page_size,
                    0,
                    0,
                ),
            );

            Ok(SearchResult {
                items: result
                    .items
                    .iter()
                    .map(|i| FileItem::from((*i, picker)))
                    .collect(),
                scores: convert_scores(&result.scores),
                total_matched: result.total_matched as u32,
                total_files: result.total_files as u32,
            })
        })
    }

    #[pyo3(signature = (
        query,
        *,
        current_file=None,
        max_threads=0,
        page_index=0,
        page_size=0,
    ))]
    fn directory_search(
        &self,
        py: Python<'_>,
        query: &str,
        current_file: Option<String>,
        max_threads: u32,
        page_index: u32,
        page_size: u32,
    ) -> PyResult<DirSearchResult> {
        let picker = self.picker.clone();
        let query = query.to_string();

        py.detach(move || -> PyResult<_> {
            let picker_guard = picker.read().map_err(py_err)?;
            let picker = picker_guard
                .as_ref()
                .ok_or_else(|| py_err("File picker not initialized"))?;

            let parsed = QueryParser::new(fff_query_parser::DirSearchConfig).parse(&query);
            let result = picker.fuzzy_search_directories(
                &parsed,
                fuzzy_options(
                    max_threads,
                    current_file.as_deref(),
                    picker.base_path(),
                    page_index,
                    page_size,
                    0,
                    0,
                ),
            );

            Ok(DirSearchResult {
                items: result
                    .items
                    .iter()
                    .map(|i| DirItem::from((*i, picker)))
                    .collect(),
                scores: convert_scores(&result.scores),
                total_matched: result.total_matched as u32,
                total_dirs: result.total_dirs as u32,
            })
        })
    }

    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (
        query,
        *,
        current_file=None,
        max_threads=0,
        page_index=0,
        page_size=0,
        combo_boost_score_multiplier=0,
        min_combo_count=0,
    ))]
    fn mixed_search(
        &self,
        py: Python<'_>,
        query: &str,
        current_file: Option<String>,
        max_threads: u32,
        page_index: u32,
        page_size: u32,
        combo_boost_score_multiplier: i32,
        min_combo_count: u32,
    ) -> PyResult<MixedSearchResult> {
        let picker = self.picker.clone();
        let query_tracker = self.query_tracker.clone();
        let query = query.to_string();

        let (items, scores, total_matched, total_files, total_dirs) =
            py.detach(move || -> PyResult<_> {
                let picker_guard = picker.read().map_err(py_err)?;
                let picker = picker_guard
                    .as_ref()
                    .ok_or_else(|| py_err("File picker not initialized"))?;
                let qt_guard = query_tracker.read().map_err(py_err)?;

                let parsed = QueryParser::new(fff_query_parser::MixedSearchConfig).parse(&query);
                let result = picker.fuzzy_search_mixed(
                    &parsed,
                    qt_guard.as_ref(),
                    fuzzy_options(
                        max_threads,
                        current_file.as_deref(),
                        picker.base_path(),
                        page_index,
                        page_size,
                        defaulted_i32(combo_boost_score_multiplier, DEFAULT_COMBO_BOOST_MULTIPLIER),
                        defaulted_u32(min_combo_count, DEFAULT_MIN_COMBO_COUNT),
                    ),
                );

                let items: Vec<MixedItem> = result
                    .items
                    .iter()
                    .map(|item| match item {
                        fff::MixedItemRef::File(file) => {
                            MixedItem::File(MixedFileItem::from((*file, picker)))
                        }
                        fff::MixedItemRef::Dir(dir) => {
                            MixedItem::Dir(MixedDirItem::from((*dir, picker)))
                        }
                    })
                    .collect();

                Ok((
                    items,
                    convert_scores(&result.scores),
                    result.total_matched as u32,
                    result.total_files as u32,
                    result.total_dirs as u32,
                ))
            })?;

        let items: PyResult<Vec<Py<PyAny>>> = items
            .into_iter()
            .map(|item| match item {
                MixedItem::File(file) => Ok(Py::new(py, file)?.into_any()),
                MixedItem::Dir(dir) => Ok(Py::new(py, dir)?.into_any()),
            })
            .collect();

        Ok(MixedSearchResult {
            items: items?,
            scores,
            total_matched,
            total_files,
            total_dirs,
        })
    }

    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (
        query,
        *,
        mode="plain",
        max_file_size=0,
        max_matches_per_file=0,
        smart_case=true,
        cursor=None,
        page_limit=0,
        time_budget_ms=0,
        before_context=0,
        after_context=0,
        classify_definitions=false,
    ))]
    fn grep(
        &self,
        py: Python<'_>,
        query: &str,
        mode: &str,
        max_file_size: u64,
        max_matches_per_file: u32,
        smart_case: bool,
        cursor: Option<&GrepCursor>,
        page_limit: u32,
        time_budget_ms: u64,
        before_context: u32,
        after_context: u32,
        classify_definitions: bool,
    ) -> PyResult<GrepResult> {
        let picker = self.picker.clone();
        let query = query.to_string();
        let mode = parse_grep_mode(mode)?;
        let cursor_offset = cursor.map(|c| c.offset as usize).unwrap_or(0);

        py.detach(move || -> PyResult<_> {
            let picker_guard = picker.read().map_err(py_err)?;
            let picker = picker_guard
                .as_ref()
                .ok_or_else(|| py_err("File picker not initialized"))?;

            let parsed = if picker.mode().is_ai() {
                QueryParser::new(fff_query_parser::AiGrepConfig).parse(&query)
            } else {
                fff::grep::parse_grep_query(&query)
            };
            let options = grep_options(
                mode,
                cursor_offset,
                max_file_size,
                max_matches_per_file,
                smart_case,
                page_limit,
                time_budget_ms,
                before_context,
                after_context,
                classify_definitions,
            );

            Ok(convert_grep_result(picker.grep(&parsed, &options), picker))
        })
    }

    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (
        patterns,
        *,
        constraints=None,
        mode="plain",
        max_file_size=0,
        max_matches_per_file=0,
        smart_case=true,
        cursor=None,
        page_limit=0,
        time_budget_ms=0,
        before_context=0,
        after_context=0,
        classify_definitions=false,
    ))]
    fn multi_grep(
        &self,
        py: Python<'_>,
        patterns: Vec<String>,
        constraints: Option<String>,
        mode: &str,
        max_file_size: u64,
        max_matches_per_file: u32,
        smart_case: bool,
        cursor: Option<&GrepCursor>,
        page_limit: u32,
        time_budget_ms: u64,
        before_context: u32,
        after_context: u32,
        classify_definitions: bool,
    ) -> PyResult<GrepResult> {
        let picker = self.picker.clone();
        let mode = parse_grep_mode(mode)?;
        let cursor_offset = cursor.map(|c| c.offset as usize).unwrap_or(0);

        py.detach(move || -> PyResult<_> {
            let picker_guard = picker.read().map_err(py_err)?;
            let picker = picker_guard
                .as_ref()
                .ok_or_else(|| py_err("File picker not initialized"))?;

            if patterns.is_empty() || patterns.iter().all(|p| p.is_empty()) {
                return Err(py_err("patterns must not be empty"));
            }
            let pattern_refs: Vec<&str> = patterns.iter().map(|s| s.as_str()).collect();

            let parsed_constraints = constraints
                .as_ref()
                .map(|c| QueryParser::new(fff_query_parser::AiGrepConfig).parse_constraints(c));
            let constraint_refs: &[fff::Constraint<'_>] = match &parsed_constraints {
                Some(constraints) => constraints,
                None => &[],
            };
            let options = grep_options(
                mode,
                cursor_offset,
                max_file_size,
                max_matches_per_file,
                smart_case,
                page_limit,
                time_budget_ms,
                before_context,
                after_context,
                classify_definitions,
            );

            Ok(convert_grep_result(
                picker.multi_grep(&pattern_refs, constraint_refs, &options),
                picker,
            ))
        })
    }

    fn scan_files(&self) -> PyResult<()> {
        self.picker
            .trigger_full_rescan_async(&self.frecency)
            .map_err(py_err)
    }

    fn is_scanning(&self) -> PyResult<bool> {
        let guard = self.picker.read().map_err(py_err)?;
        Ok(guard.as_ref().map(|p| p.is_scan_active()).unwrap_or(false))
    }

    #[pyo3(signature = (timeout_ms=5000))]
    fn wait_for_scan_blocking(&self, py: Python<'_>, timeout_ms: u64) -> PyResult<bool> {
        let picker = self.picker.clone();
        py.detach(move || Ok(picker.wait_for_scan(Duration::from_millis(timeout_ms))))
    }

    /// Subscribe to filesystem changes matching `pattern`.
    ///
    /// Patterns may be base-relative globs (./ works), exact paths inside the indexed
    /// tree, or existing directories. An empty pattern watches the whole tree.
    ///
    /// Events are debounced and submitted in batches per 100-ms window at most 128 events.
    /// Gitignored and other ignored files are never triggering watcher.
    #[pyo3(signature = (pattern, callback, *, ignore = None))]
    fn watch(
        &self,
        py: Python<'_>,
        pattern: Option<&str>,
        callback: Py<PyAny>,
        ignore: Option<Vec<String>>,
    ) -> PyResult<WatchSubscription> {
        if !callback.bind(py).is_callable() {
            return Err(PyTypeError::new_err("callback must be callable"));
        }

        let pattern = pattern.unwrap_or_default().to_string();
        let options = fff::WatchOptions {
            ignore: ignore.unwrap_or_default(),
        };

        // Suppresses invocations racing an unsubscribe: the flag flips before
        // core unwatch, so user code never runs after unsubscribe() returns.
        let active = Arc::new(AtomicBool::new(true));
        let active_cb = Arc::clone(&active);

        let id = {
            let picker = self.picker.clone();
            py.detach(move || {
                picker.watch(&pattern, options, move |_id, events| {
                    Python::attach(|py| {
                        if !active_cb.load(Ordering::Acquire) {
                            return;
                        }
                        let batch: Vec<WatchEvent> = events
                            .iter()
                            .map(|ev| WatchEvent {
                                path: ev.path.to_string_lossy().to_string(),
                                kind: ev.kind.as_str().to_string(),
                            })
                            .collect();
                        if let Err(e) = callback.call1(py, (batch,)) {
                            e.write_unraisable(py, None);
                        }
                    })
                })
            })
            .map_err(py_err)?
        };

        Ok(WatchSubscription::new(self.picker.clone(), id.0, active))
    }

    fn reindex(&self, py: Python<'_>, new_path: PathBuf) -> PyResult<()> {
        let picker = self.picker.clone();
        let frecency = self.frecency.clone();
        let cache_budget_max_files = self.cache_budget_max_files;
        let cache_budget_max_bytes = self.cache_budget_max_bytes;
        let cache_budget_max_file_size = self.cache_budget_max_file_size;

        py.detach(move || -> PyResult<()> {
            if !new_path.exists() {
                return Err(py_err(format!(
                    "Path does not exist: {}",
                    new_path.display()
                )));
            }
            let canonical = fff::path_utils::canonicalize(&new_path).map_err(py_err)?;

            let (
                warmup_caches,
                content_indexing,
                watch,
                mode,
                fs_root,
                home_dir,
                follow_symlinks,
                git_recency,
            ) = {
                let guard = picker.read().map_err(py_err)?;
                if let Some(ref picker) = *guard {
                    (
                        picker.has_mmap_cache(),
                        picker.has_content_indexing(),
                        picker.has_watcher(),
                        picker.mode(),
                        picker.fs_root_scanning_enabled(),
                        picker.home_dir_scanning_enabled(),
                        picker.follows_symlinks(),
                        picker.git_recency_config(),
                    )
                } else {
                    (
                        // just the defaults
                        false,
                        true,
                        true,
                        FFFMode::default(),
                        false,
                        false,
                        false,
                        Default::default(),
                    )
                }
            };

            FilePicker::new_with_shared_state(
                picker.clone(),
                frecency,
                FilePickerOptions {
                    base_path: canonical.to_string_lossy().to_string(),
                    enable_mmap_cache: warmup_caches,
                    enable_content_indexing: content_indexing,
                    watch,
                    mode,
                    cache_budget: fff::ContentCacheBudget::from_overrides(
                        cache_budget_max_files,
                        cache_budget_max_bytes,
                        cache_budget_max_file_size,
                    ),
                    follow_symlinks,
                    enable_fs_root_scanning: fs_root,
                    enable_home_dir_scanning: home_dir,
                    git_recency,
                },
            )
            .map_err(py_err)
        })
    }

    fn refresh_git_status(&self, py: Python<'_>) -> PyResult<i64> {
        let picker = self.picker.clone();
        let frecency = self.frecency.clone();
        py.detach(move || {
            picker
                .refresh_git_status(&frecency)
                .map_err(py_err)
                .map(|c| c as i64)
        })
    }

    #[pyo3(signature = (query, selected_file_path))]
    fn track_query(
        &self,
        py: Python<'_>,
        query: &str,
        selected_file_path: PathBuf,
    ) -> PyResult<bool> {
        let picker = self.picker.clone();
        let query_tracker = self.query_tracker.clone();
        let query = query.to_string();

        py.detach(move || -> PyResult<bool> {
            let file_path = fff::path_utils::canonicalize(&selected_file_path).map_err(py_err)?;
            let project_path = {
                let guard = picker.read().map_err(py_err)?;
                guard.as_ref().map(|p| p.base_path().to_path_buf())
            };
            let project_path = match project_path {
                Some(p) => p,
                None => return Ok(false),
            };

            let mut qt_guard = query_tracker.write().map_err(py_err)?;
            if let Some(ref mut tracker) = *qt_guard {
                tracker
                    .track_query_completion(&query, &project_path, &file_path)
                    .map_err(py_err)?;
                Ok(true)
            } else {
                Ok(false)
            }
        })
    }

    fn get_historical_query(&self, py: Python<'_>, offset: u64) -> PyResult<Option<String>> {
        let picker = self.picker.clone();
        let query_tracker = self.query_tracker.clone();

        py.detach(move || -> PyResult<Option<String>> {
            let project_path = {
                let guard = picker.read().map_err(py_err)?;
                guard.as_ref().map(|p| p.base_path().to_path_buf())
            };
            let project_path = match project_path {
                Some(p) => p,
                None => return Ok(None),
            };

            let qt_guard = query_tracker.read().map_err(py_err)?;
            if let Some(ref tracker) = *qt_guard {
                tracker
                    .get_historical_query(&project_path, offset as usize)
                    .map_err(py_err)
            } else {
                Ok(None)
            }
        })
    }

    #[pyo3(signature = (test_path=None))]
    fn health_check(&self, py: Python<'_>, test_path: Option<PathBuf>) -> PyResult<Py<PyDict>> {
        let picker = self.picker.clone();
        let frecency = self.frecency.clone();
        let query_tracker = self.query_tracker.clone();

        let (
            git_version,
            repository_found,
            workdir,
            git_error,
            picker_initialized,
            picker_base_path,
            picker_is_scanning,
            picker_indexed_files,
            frecency_initialized,
            query_tracker_initialized,
        ) = py.detach(move || -> PyResult<_> {
            // Resolve the path to inspect: explicit arg → indexed base path →
            // process cwd. Report a cwd-resolution failure instead of silently
            // discovering from an empty path.
            let (test_path, cwd_error) = match test_path {
                Some(p) => (Some(p), None),
                None => {
                    let base = picker
                        .read()
                        .ok()
                        .and_then(|g| g.as_ref().map(|p| p.base_path().to_path_buf()));
                    match base {
                        Some(p) => (Some(p), None),
                        None => match std::env::current_dir() {
                            Ok(p) => (Some(p), None),
                            Err(e) => (
                                None,
                                Some(format!("could not determine current directory: {}", e)),
                            ),
                        },
                    }
                }
            };

            let git_version = git2::Version::get();
            let (major, minor, rev) = git_version.libgit2_version();
            let git_version = format!("{}.{}.{}", major, minor, rev);
            let (repository_found, workdir, git_error) = match test_path {
                None => (false, None, cwd_error),
                Some(test_path) => match git2::Repository::discover(&test_path) {
                    Ok(repo) => (
                        true,
                        repo.workdir().map(|p| p.to_string_lossy().to_string()),
                        None,
                    ),
                    Err(e) => (false, None, Some(e.message().to_string())),
                },
            };

            let (picker_initialized, picker_base_path, picker_is_scanning, picker_indexed_files) = {
                let guard = picker.read().map_err(py_err)?;
                if let Some(ref picker) = *guard {
                    let progress = picker.get_scan_progress();
                    (
                        true,
                        Some(picker.base_path().to_string_lossy().to_string()),
                        Some(picker.is_scan_active()),
                        Some(progress.scanned_files_count),
                    )
                } else {
                    (false, None, None, None)
                }
            };
            let frecency_initialized = frecency.read().map_err(py_err)?.is_some();
            let query_tracker_initialized = query_tracker.read().map_err(py_err)?.is_some();

            Ok((
                git_version,
                repository_found,
                workdir,
                git_error,
                picker_initialized,
                picker_base_path,
                picker_is_scanning,
                picker_indexed_files,
                frecency_initialized,
                query_tracker_initialized,
            ))
        })?;

        let dict = PyDict::new(py);
        dict.set_item("version", env!("CARGO_PKG_VERSION"))?;

        let git_info = PyDict::new(py);
        git_info.set_item("available", true)?;
        git_info.set_item("libgit2_version", git_version)?;
        git_info.set_item("repository_found", repository_found)?;
        if let Some(workdir) = workdir {
            git_info.set_item("workdir", workdir)?;
        }
        if let Some(error) = git_error {
            git_info.set_item("error", error)?;
        }
        dict.set_item("git", git_info)?;

        let picker_info = PyDict::new(py);
        picker_info.set_item("initialized", picker_initialized)?;
        if let Some(base_path) = picker_base_path {
            picker_info.set_item("base_path", base_path)?;
        }
        if let Some(is_scanning) = picker_is_scanning {
            picker_info.set_item("is_scanning", is_scanning)?;
        }
        if let Some(indexed_files) = picker_indexed_files {
            picker_info.set_item("indexed_files", indexed_files)?;
        }
        dict.set_item("file_picker", picker_info)?;

        let frecency_info = PyDict::new(py);
        frecency_info.set_item("initialized", frecency_initialized)?;
        dict.set_item("frecency", frecency_info)?;

        let query_info = PyDict::new(py);
        query_info.set_item("initialized", query_tracker_initialized)?;
        dict.set_item("query_tracker", query_info)?;

        Ok(dict.unbind())
    }
}
