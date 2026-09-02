use crate::types::FileItem;
use smallvec::SmallVec;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

pub use crate::constants::MAX_FFFILE_SIZE;

/// Controls how the grep pattern is interpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GrepMode {
    /// Literal plain text match: default path that doesn't require any regex machinery
    #[default]
    PlainText,
    /// Regex mode: uses the same exact matching engine as ripgrep
    Regex,
    /// Smart fuzzy mode, allows user to make either a couple of single char typos or long gaps
    /// e.g. shcema -> shcema, or UserController -> UserAuthController
    ///
    /// Significatnly slower than plain text, especially on unindexed FilePicker
    Fuzzy,
}

/// A single content match within a file
#[derive(Debug, Clone)]
pub struct GrepMatch {
    /// Index into the deduplicated `files` vec of the GrepResult.
    pub file_index: usize,
    /// 1-based line number.
    pub line_number: u64,
    /// 0-based byte column of first match start within the line.
    pub col: usize,
    /// Absolute byte offset of the matched line from the start of the file.
    /// Can be used by the preview to seek directly without scanning from the top.
    pub byte_offset: u64,
    /// The matched line text, truncated to `MAX_LINE_DISPLAY_LEN`.
    pub line_content: String,
    /// Byte offsets `(start, end)` within `line_content` for each match.
    /// Stack-allocated for the common case of ≤4 spans per line.
    pub match_byte_offsets: SmallVec<[(u32, u32); 4]>,
    /// Fuzzy match score from neo_frizbee (only set in Fuzzy grep mode).
    pub fuzzy_score: Option<u16>,
    /// Whether the matched line looks like a definition (struct, fn, class, etc.).
    /// Computed at match time so output formatters don't need to re-scan.
    pub is_definition: bool,
    /// Lines before the match (for context display). Empty when context is 0.
    pub context_before: Vec<String>,
    /// Lines after the match (for context display). Empty when context is 0.
    pub context_after: Vec<String>,
}

impl GrepMatch {
    /// Strip leading whitespace from `line_content` and all context lines,
    /// adjusting `col` and `match_byte_offsets` so highlights remain correct.
    pub fn trim_leading_whitespace(&mut self) {
        let strip_len = self.line_content.len() - self.line_content.trim_start().len();
        if strip_len > 0 {
            self.line_content.drain(..strip_len);
            let off = strip_len as u32;
            self.col = self.col.saturating_sub(strip_len);
            for range in &mut self.match_byte_offsets {
                range.0 = range.0.saturating_sub(off);
                range.1 = range.1.saturating_sub(off);
            }
        }
        for line in &mut self.context_before {
            let n = line.len() - line.trim_start().len();
            if n > 0 {
                line.drain(..n);
            }
        }
        for line in &mut self.context_after {
            let n = line.len() - line.trim_start().len();
            if n > 0 {
                line.drain(..n);
            }
        }
    }
}

/// Options for grep search.
#[derive(Debug, Clone)]
pub struct GrepSearchOptions {
    pub max_file_size: u64,
    pub max_matches_per_file: usize,
    pub smart_case: bool,
    /// File-based pagination offset: index into the sorted/filtered file list
    /// to start searching from. Pass 0 for the first page, then use
    /// `GrepResult::next_file_offset` for subsequent pages.
    pub file_offset: usize,
    /// Maximum number of matches to collect before stopping.
    pub page_limit: usize,
    /// How to interpret the search pattern. Defaults to `PlainText`.
    pub mode: GrepMode,
    /// Maximum time in milliseconds to spend searching before returning partial
    /// results. Prevents UI freezes on pathological queries. 0 = no limit.
    pub time_budget_ms: u64,
    /// Apply `time_budget_ms` even before anything matched. Off by default: plain/regex
    /// grep only starts counting once matches exist, so a zero-match query scans everything.
    pub enforce_time_budget: bool,
    /// Number of context lines to include before each match. 0 = disabled.
    pub before_context: usize,
    /// Number of context lines to include after each match. 0 = disabled.
    pub after_context: usize,
    /// Whether to classify each match as a definition line. Adds ~2% overhead
    /// on large repos; disable for interactive grep where it is not needed.
    pub classify_definitions: bool,
    /// Strip leading whitespace from matched lines and context lines, adjusting
    /// highlight byte offsets accordingly. Useful for AI/MCP consumers and UIs
    /// that don't need indentation. Default: false.
    pub trim_whitespace: bool,
    /// External abort signal. When provided, overrides the picker's internal
    /// cancellation flag. Set to `true` to stop the search early and return
    /// partial results. Omit (or use `..Default::default()`) to let the
    /// picker manage cancellation.
    pub abort_signal: Option<Arc<AtomicBool>>,
}

impl Default for GrepSearchOptions {
    fn default() -> Self {
        Self {
            max_file_size: MAX_FFFILE_SIZE,
            max_matches_per_file: 200,
            smart_case: true,
            file_offset: 0,
            page_limit: 50,
            mode: GrepMode::default(),
            time_budget_ms: 0,
            enforce_time_budget: false,
            before_context: 0,
            after_context: 0,
            classify_definitions: false,
            trim_whitespace: false,
            abort_signal: None,
        }
    }
}

/// Result of a grep search with a list of matches, list of matched files, and metadata.
#[derive(Debug, Clone, Default)]
pub struct GrepResult<'a> {
    pub matches: Vec<GrepMatch>,
    /// Deduplicated file references for the returned matches.
    pub files: Vec<&'a FileItem>,
    /// Number of files actually searched in this call.
    pub total_files_searched: usize,
    /// Total number of indexed files (before filtering).
    pub total_files: usize,
    /// Total number of searchable files (after filtering out binary, too-large, etc.).
    pub filtered_file_count: usize,
    /// Number of files that contained at least one match.
    pub files_with_matches: usize,
    /// The file offset to pass for the next page. `0` if there are no more files.
    /// Callers should store this and pass it as `file_offset` in the next call.
    pub next_file_offset: usize,
    /// When regex mode fails to compile the pattern, the search falls back to
    /// literal matching and this field contains the compilation error message.
    /// The UI can display this to inform the user their regex was invalid.
    pub regex_fallback_error: Option<String>,
    /// Set to `true` if the constrained query found nothing and the results come from
    /// retrying the whole raw query as literal text (ignored all the inferred constraints)
    pub literal_fallback: bool,
}

impl<'a> GrepResult<'a> {
    /// Empty result carrying only the file counts (empty query / prefilter miss)
    pub(crate) fn empty(total_files: usize, filtered_file_count: usize) -> Self {
        Self {
            total_files,
            filtered_file_count,
            ..Default::default()
        }
    }

    pub(crate) fn collect(
        per_file_results: Vec<(usize, &'a FileItem, Vec<GrepMatch>)>,
        files_to_search_len: usize,
        options: &GrepSearchOptions,
        total_files: usize,
        filtered_file_count: usize,
        // Lowest unsearched index when an abort stopped the search.
        abort_resume: Option<usize>,
    ) -> Self {
        let page_limit = options.page_limit;

        // Each match stores a `file_index` pointing into `result_files` so that
        // consumers (FFI JSON, Lua) can look up file metadata without duplicating
        // it across every match from the same file
        let mut result_files: Vec<&'a FileItem> = Vec::new();
        let mut all_matches: Vec<GrepMatch> = Vec::new();
        // files_consumed tracks how far into files_to_search we have advanced,
        // counting every file whose results were emitted (with or without matches).
        // We use the batch_idx of the last consumed file + 1, which is correct
        // because per_file_results only contains files that had matches, and
        // files between them that had no matches were still searched and can be
        // safely skipped on the next page
        let mut files_consumed: usize = 0;

        for (batch_idx, file, file_matches) in per_file_results {
            // Matches past the abort point are dropped; the next page re-searches them.
            if abort_resume.is_some_and(|at| batch_idx >= at) {
                continue;
            }

            // batch_idx is the 0-based position in files_to_search.
            // Advance files_consumed to include this file and all no-match files before it.
            files_consumed = batch_idx + 1;

            let file_result_idx = result_files.len();
            result_files.push(file);

            for mut m in file_matches {
                m.file_index = file_result_idx;
                if options.trim_whitespace {
                    m.trim_leading_whitespace();
                }
                all_matches.push(m);
            }

            // page_limit is a soft cap: we always finish the current file before
            // stopping, so no matches are dropped. A page may return up to
            // page_limit + max_matches_per_file - 1 matches in the worst case
            if all_matches.len() >= page_limit {
                break;
            }
        }

        // Abort stops at a known index; otherwise zero matches means a full scan.
        match abort_resume {
            Some(resume_at) => files_consumed = resume_at.min(files_to_search_len),
            None if result_files.is_empty() => files_consumed = files_to_search_len,
            None => {}
        }

        let has_more = files_consumed < files_to_search_len
            && (abort_resume.is_some() || all_matches.len() >= page_limit);

        let next_file_offset = if has_more {
            options.file_offset + files_consumed
        } else {
            0
        };

        Self {
            matches: all_matches,
            files_with_matches: result_files.len(),
            files: result_files,
            total_files_searched: files_consumed,
            total_files,
            filtered_file_count,
            next_file_offset,
            regex_fallback_error: None,
            literal_fallback: false,
        }
    }
}
