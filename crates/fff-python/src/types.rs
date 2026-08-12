use pyo3::prelude::*;

#[pyclass(skip_from_py_object)]
#[derive(Clone)]
pub struct Score {
    #[pyo3(get)]
    pub total: i32,
    #[pyo3(get)]
    pub base_score: i32,
    #[pyo3(get)]
    pub filename_bonus: i32,
    #[pyo3(get)]
    pub special_filename_bonus: i32,
    #[pyo3(get)]
    pub frecency_boost: i32,
    #[pyo3(get)]
    pub distance_penalty: i32,
    #[pyo3(get)]
    pub current_file_penalty: i32,
    #[pyo3(get)]
    pub combo_match_boost: i32,
    #[pyo3(get)]
    pub path_alignment_bonus: i32,
    #[pyo3(get)]
    pub exact_match: bool,
    #[pyo3(get)]
    pub match_type: String,
}

#[pymethods]
impl Score {
    fn __repr__(&self) -> String {
        format!(
            "Score(total={}, base_score={}, filename_bonus={}, match_type={:?})",
            self.total, self.base_score, self.filename_bonus, self.match_type
        )
    }
}

#[pyclass(skip_from_py_object)]
#[derive(Clone)]
pub struct FileItem {
    #[pyo3(get)]
    pub relative_path: String,
    #[pyo3(get)]
    pub file_name: String,
    #[pyo3(get)]
    pub git_status: String,
    #[pyo3(get)]
    pub size: u64,
    #[pyo3(get)]
    pub modified: u64,
    #[pyo3(get)]
    pub access_frecency_score: i64,
    #[pyo3(get)]
    pub modification_frecency_score: i64,
    #[pyo3(get)]
    pub total_frecency_score: i64,
    #[pyo3(get)]
    pub is_binary: bool,
}

#[pymethods]
impl FileItem {
    fn __repr__(&self) -> String {
        format!(
            "FileItem(relative_path={:?}, file_name={:?}, size={})",
            self.relative_path, self.file_name, self.size
        )
    }
}

#[pyclass(skip_from_py_object)]
#[derive(Clone)]
pub struct DirItem {
    #[pyo3(get)]
    pub relative_path: String,
    #[pyo3(get)]
    pub dir_name: String,
    #[pyo3(get)]
    pub max_access_frecency: i32,
}

#[pymethods]
impl DirItem {
    fn __repr__(&self) -> String {
        format!(
            "DirItem(relative_path={:?}, dir_name={:?})",
            self.relative_path, self.dir_name
        )
    }
}

#[pyclass(skip_from_py_object)]
#[derive(Clone)]
pub struct MixedFileItem {
    #[pyo3(get)]
    pub relative_path: String,
    #[pyo3(get)]
    pub file_name: String,
    #[pyo3(get)]
    pub git_status: String,
    #[pyo3(get)]
    pub size: u64,
    #[pyo3(get)]
    pub modified: u64,
    #[pyo3(get)]
    pub access_frecency_score: i64,
    #[pyo3(get)]
    pub modification_frecency_score: i64,
    #[pyo3(get)]
    pub total_frecency_score: i64,
    #[pyo3(get)]
    pub is_binary: bool,
}

#[pymethods]
impl MixedFileItem {
    fn __repr__(&self) -> String {
        format!(
            "MixedFileItem(relative_path={:?}, file_name={:?}, size={})",
            self.relative_path, self.file_name, self.size
        )
    }
}

#[pyclass(skip_from_py_object)]
#[derive(Clone)]
pub struct MixedDirItem {
    #[pyo3(get)]
    pub relative_path: String,
    #[pyo3(get)]
    pub dir_name: String,
    #[pyo3(get)]
    pub max_access_frecency: i32,
}

#[pymethods]
impl MixedDirItem {
    fn __repr__(&self) -> String {
        format!(
            "MixedDirItem(relative_path={:?}, dir_name={:?})",
            self.relative_path, self.dir_name
        )
    }
}

#[pyclass(skip_from_py_object)]
#[derive(Clone)]
pub struct MatchRange {
    #[pyo3(get)]
    pub start: u32,
    #[pyo3(get)]
    pub end: u32,
}

#[pymethods]
impl MatchRange {
    fn __repr__(&self) -> String {
        format!("MatchRange(start={}, end={})", self.start, self.end)
    }
}

#[pyclass(skip_from_py_object)]
#[derive(Clone)]
pub struct GrepMatch {
    #[pyo3(get)]
    pub relative_path: String,
    #[pyo3(get)]
    pub file_name: String,
    #[pyo3(get)]
    pub git_status: String,
    #[pyo3(get)]
    pub line_content: String,
    #[pyo3(get)]
    pub match_ranges: Vec<MatchRange>,
    #[pyo3(get)]
    pub context_before: Vec<String>,
    #[pyo3(get)]
    pub context_after: Vec<String>,
    #[pyo3(get)]
    pub size: u64,
    #[pyo3(get)]
    pub modified: u64,
    #[pyo3(get)]
    pub total_frecency_score: i64,
    #[pyo3(get)]
    pub access_frecency_score: i64,
    #[pyo3(get)]
    pub modification_frecency_score: i64,
    #[pyo3(get)]
    pub line_number: u64,
    #[pyo3(get)]
    pub byte_offset: u64,
    #[pyo3(get)]
    pub col: u32,
    #[pyo3(get)]
    pub fuzzy_score: Option<u16>,
    #[pyo3(get)]
    pub is_definition: bool,
    #[pyo3(get)]
    pub is_binary: bool,
}

#[pymethods]
impl GrepMatch {
    fn __repr__(&self) -> String {
        format!(
            "GrepMatch(relative_path={:?}, line_number={}, line_content={:?})",
            self.relative_path, self.line_number, self.line_content
        )
    }
}

#[pyclass(skip_from_py_object)]
#[derive(Clone)]
pub struct SearchResult {
    #[pyo3(get)]
    pub items: Vec<FileItem>,
    #[pyo3(get)]
    pub scores: Vec<Score>,
    #[pyo3(get)]
    pub total_matched: u32,
    #[pyo3(get)]
    pub total_files: u32,
}

#[pymethods]
impl SearchResult {
    fn __repr__(&self) -> String {
        format!(
            "SearchResult(items={}, total_matched={}, total_files={})",
            self.items.len(),
            self.total_matched,
            self.total_files
        )
    }

    fn __len__(&self) -> usize {
        self.items.len()
    }

    fn __bool__(&self) -> bool {
        !self.items.is_empty()
    }
}

#[pyclass(skip_from_py_object)]
#[derive(Clone)]
pub struct DirSearchResult {
    #[pyo3(get)]
    pub items: Vec<DirItem>,
    #[pyo3(get)]
    pub scores: Vec<Score>,
    #[pyo3(get)]
    pub total_matched: u32,
    #[pyo3(get)]
    pub total_dirs: u32,
}

#[pymethods]
impl DirSearchResult {
    fn __repr__(&self) -> String {
        format!(
            "DirSearchResult(items={}, total_matched={}, total_dirs={})",
            self.items.len(),
            self.total_matched,
            self.total_dirs
        )
    }

    fn __len__(&self) -> usize {
        self.items.len()
    }

    fn __bool__(&self) -> bool {
        !self.items.is_empty()
    }
}

#[pyclass]
pub struct MixedSearchResult {
    #[pyo3(get)]
    pub items: Vec<Py<PyAny>>,
    #[pyo3(get)]
    pub scores: Vec<Score>,
    #[pyo3(get)]
    pub total_matched: u32,
    #[pyo3(get)]
    pub total_files: u32,
    #[pyo3(get)]
    pub total_dirs: u32,
}

#[pymethods]
impl MixedSearchResult {
    fn __repr__(&self) -> String {
        format!(
            "MixedSearchResult(items={}, total_matched={}, total_files={}, total_dirs={})",
            self.items.len(),
            self.total_matched,
            self.total_files,
            self.total_dirs
        )
    }

    fn __len__(&self) -> usize {
        self.items.len()
    }

    fn __bool__(&self) -> bool {
        !self.items.is_empty()
    }
}

#[pyclass(skip_from_py_object)]
#[derive(Clone)]
pub struct GrepResult {
    #[pyo3(get)]
    pub items: Vec<GrepMatch>,
    #[pyo3(get)]
    pub total_matched: u32,
    #[pyo3(get)]
    pub total_files_searched: u32,
    #[pyo3(get)]
    pub total_files: u32,
    #[pyo3(get)]
    pub filtered_file_count: u32,
    #[pyo3(get)]
    pub next_file_offset: u32,
    #[pyo3(get)]
    pub regex_fallback_error: Option<String>,
}

#[pymethods]
impl GrepResult {
    fn __repr__(&self) -> String {
        format!(
            "GrepResult(items={}, total_matched={}, next_file_offset={})",
            self.items.len(),
            self.total_matched,
            self.next_file_offset
        )
    }

    fn __len__(&self) -> usize {
        self.items.len()
    }

    fn __bool__(&self) -> bool {
        !self.items.is_empty()
    }

    #[getter]
    fn has_more(&self) -> bool {
        self.next_file_offset > 0
    }

    fn next_cursor(&self, py: Python<'_>) -> PyResult<Option<Py<GrepCursor>>> {
        if self.next_file_offset > 0 {
            Ok(Some(Py::new(py, GrepCursor::new(self.next_file_offset))?))
        } else {
            Ok(None)
        }
    }
}

#[pyclass(skip_from_py_object)]
#[derive(Clone)]
pub struct ScanProgress {
    #[pyo3(get)]
    pub scanned_files_count: u64,
    #[pyo3(get)]
    pub is_scanning: bool,
    #[pyo3(get)]
    pub is_watcher_ready: bool,
    #[pyo3(get)]
    pub is_warmup_complete: bool,
}

#[pymethods]
impl ScanProgress {
    fn __repr__(&self) -> String {
        format!(
            "ScanProgress(scanned_files_count={}, is_scanning={})",
            self.scanned_files_count, self.is_scanning
        )
    }
}

#[pyclass(skip_from_py_object)]
#[derive(Clone)]
pub struct WatchEvent {
    #[pyo3(get)]
    pub path: String,
    #[pyo3(get)]
    pub kind: String,
    /// Path the file moved from. Only set when `kind == "renamed"`.
    #[pyo3(get)]
    pub from_path: Option<String>,
}

#[pymethods]
impl WatchEvent {
    fn __repr__(&self) -> String {
        match &self.from_path {
            Some(from) => format!(
                "WatchEvent(path={:?}, kind={:?}, from_path={:?})",
                self.path, self.kind, from
            ),
            None => format!("WatchEvent(path={:?}, kind={:?})", self.path, self.kind),
        }
    }
}

#[pyclass(skip_from_py_object)]
#[derive(Clone)]
pub struct GrepCursor {
    #[pyo3(get)]
    pub offset: u32,
}

#[pymethods]
impl GrepCursor {
    #[new]
    fn new(offset: u32) -> Self {
        Self { offset }
    }

    fn __repr__(&self) -> String {
        format!("GrepCursor(offset={})", self.offset)
    }
}
