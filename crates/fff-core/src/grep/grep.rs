use super::prefilter::prefilter_with_filepath_retry;
use super::regex::{RegexMatcher, RegexSink, build_regex};
use super::sink::{SinkState, debug_assert_newline_terminator};
use super::types::{GrepMatch, GrepMode, GrepResult, GrepSearchOptions};
use crate::index::{
    BigramFilter, BigramOverlay, bigram_boundary, fuzzy_candidates, literal_candidates,
    regex_candidates,
};
use crate::simd_string_utils::memmem;
use crate::types::{ContentCacheBudget, FileItem, FileSliceExt, MmapSlot};
use fff_grep::{
    Searcher, SearcherBuilder, Sink, SinkMatch,
    matcher::{Match, Matcher, NoError},
};
use fff_query_parser::{FFFQuery, GrepConfig, QueryParser};
use rayon::prelude::*;
use smallvec::SmallVec;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tracing::Level;

#[allow(clippy::large_enum_variant)]
pub(super) enum NeedleFinder<'a> {
    CaseSensitive(memchr::memmem::Finder<'a>),
    /// Pre-lowered needle bytes for the SIMD case-insensitive search.
    CaseInsensitive(&'a [u8]),
}

impl<'a> NeedleFinder<'a> {
    fn new(needle: &'a [u8], case_insensitive: bool) -> Self {
        if case_insensitive {
            Self::CaseInsensitive(needle)
        } else {
            Self::CaseSensitive(memchr::memmem::Finder::new(needle))
        }
    }

    #[inline]
    fn find(&self, haystack: &[u8]) -> Option<usize> {
        match self {
            Self::CaseSensitive(finder) => finder.find(haystack),
            Self::CaseInsensitive(needle_lower) => memmem::find(haystack, needle_lower),
        }
    }

    #[inline]
    fn needle(&self) -> &[u8] {
        match self {
            Self::CaseSensitive(finder) => finder.needle(),
            Self::CaseInsensitive(needle_lower) => needle_lower,
        }
    }

    /// Compare `haystack` against a slice of the needle with the same case
    /// semantics as `find`.
    #[inline]
    fn eq_fold(&self, haystack: &[u8], needle_seg: &[u8]) -> bool {
        match self {
            Self::CaseSensitive(_) => haystack == needle_seg,
            Self::CaseInsensitive(_) => {
                haystack.len() == needle_seg.len() && memmem::find(haystack, needle_seg) == Some(0)
            }
        }
    }

    /// Collect highlight spans for every needle occurrence within a line.
    /// The case branch is resolved once per line, not once per occurrence.
    #[inline]
    fn for_each_occurrence(&self, haystack: &[u8], mut on_match: impl FnMut(usize)) {
        match self {
            Self::CaseSensitive(finder) => {
                let mut start_pos = 0usize;
                while let Some(pos) = finder.find(&haystack[start_pos..]) {
                    on_match(start_pos + pos);
                    start_pos += pos + 1;
                }
            }
            Self::CaseInsensitive(needle_lower) => {
                let mut start_pos = 0usize;
                while let Some(pos) = memmem::find(&haystack[start_pos..], needle_lower) {
                    on_match(start_pos + pos);
                    start_pos += pos + 1;
                }
            }
        }
    }
}

struct PlainTextMatcher<'a> {
    finder: &'a NeedleFinder<'a>,
}

impl Matcher for PlainTextMatcher<'_> {
    type Error = NoError;

    #[inline]
    fn find_at(&self, haystack: &[u8], at: usize) -> Result<Option<Match>, NoError> {
        let hay = &haystack[at..];
        let needle_len = self.finder.needle().len();

        Ok(self
            .finder
            .find(hay)
            .map(|pos| Match::new(at + pos, at + pos + needle_len)))
    }

    #[inline]
    fn line_terminator(&self) -> Option<fff_grep::LineTerminator> {
        Some(fff_grep::LineTerminator::byte(b'\n'))
    }
}

struct PlainTextSink<'r> {
    state: SinkState,
    finder: &'r NeedleFinder<'r>,
    pattern_len: u32,
    multiline_segment_len: Option<usize>,
}

impl Sink for PlainTextSink<'_> {
    type Error = std::io::Error;

    fn matched(
        &mut self,
        searcher: &Searcher,
        sink_match: &SinkMatch<'_>,
    ) -> Result<bool, Self::Error> {
        debug_assert_newline_terminator(searcher);
        if self.state.max_matches != 0 && self.state.matches.len() >= self.state.max_matches {
            return Ok(false);
        }

        let line_bytes = sink_match.bytes();
        let (display_bytes, display_len, line_number, byte_offset) =
            SinkState::prepare_line(line_bytes, sink_match);

        let line_content = String::from_utf8_lossy(display_bytes).into_owned();
        let mut match_byte_offsets: SmallVec<[(u32, u32); 4]> = SmallVec::new();
        let mut col = 0usize;
        let mut first = true;

        if let Some(seg_len) = self.multiline_segment_len {
            // Multiline needle: the match starts on this line, so the needle's
            // first segment must be a suffix of the line. Highlight that suffix.
            let seg = &self.finder.needle()[..seg_len];
            if !seg.is_empty()
                && display_bytes.len() >= seg.len()
                && self
                    .finder
                    .eq_fold(&display_bytes[display_bytes.len() - seg.len()..], seg)
            {
                col = display_bytes.len() - seg.len();
                match_byte_offsets.push((col as u32, display_len));
            }
        } else {
            let pattern_len = self.pattern_len;
            self.finder.for_each_occurrence(display_bytes, |pos| {
                let abs_start = pos as u32;
                let abs_end = (abs_start + pattern_len).min(display_len);
                if first {
                    col = pos;
                    first = false;
                }
                match_byte_offsets.push((abs_start, abs_end));
            });
        }

        let (context_before, context_after) = self.state.extract_context(sink_match);
        self.state.push_match(
            line_number,
            col,
            byte_offset,
            line_content,
            match_byte_offsets,
            context_before,
            context_after,
        );
        Ok(true)
    }

    fn finish(&mut self, _: &Searcher, _: &fff_grep::SinkFinish) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// Perform a grep search across all indexed files.
///
/// When `query` is empty, returns git-modified/untracked files sorted by
/// frecency for the "welcome state" UI.
#[tracing::instrument(skip_all, fields(file_count = files.len()))]
#[allow(clippy::too_many_arguments)]
pub(crate) fn grep_search<'a>(
    files: &'a [FileItem],
    query: &FFFQuery<'_>,
    options: &GrepSearchOptions,
    budget: &ContentCacheBudget,
    bigram_index: Option<&BigramFilter>,
    bigram_overlay: Option<&BigramOverlay>,
    abort_signal: &AtomicBool,
    base_path: &Path,
    arena: crate::simd_path::ArenaPtr,
    overflow_arena: crate::simd_path::ArenaPtr,
) -> GrepResult<'a> {
    let result = grep_search_parsed(
        files,
        query,
        options,
        budget,
        bigram_index,
        bigram_overlay,
        abort_signal,
        base_path,
        arena,
        overflow_arena,
    );

    // Constraint parsing can swallow tokens the user meant literally (e.g. `!=`
    // becoming an exclusion). If the constrained search scanned everything and
    // found nothing, retry the whole raw query as literal text. This also holds
    // for later pages: an empty full scan at offset 0 stays empty at any offset,
    // so paging offsets consistently index the literal search's file list.
    let full_scan_empty = result.matches.is_empty() && result.next_file_offset == 0;
    if !full_scan_empty || query.constraints.is_empty() || abort_signal.load(Ordering::Relaxed) {
        return result;
    }

    let raw = query.raw_query.trim();
    if raw.is_empty() {
        return result;
    }

    // Keep any explicit FilePath scope (AI mode `path/to/file.ext` prefix) so the
    // fallback can't leak matches outside the file the user pinned. Only the
    // swallowed operator/glob tokens are dropped. See issue #756.
    let scoped_constraints: fff_query_parser::ConstraintVec<'_> = query
        .constraints
        .iter()
        .filter(|c| matches!(c, fff_query_parser::Constraint::FilePath(_)))
        .cloned()
        .collect();

    let literal_query = FFFQuery {
        raw_query: query.raw_query,
        constraints: scoped_constraints,
        fuzzy_query: fff_query_parser::FuzzyQuery::Text(raw),
        location: None,
    };

    let mut fallback = grep_search_parsed(
        files,
        &literal_query,
        options,
        budget,
        bigram_index,
        bigram_overlay,
        abort_signal,
        base_path,
        arena,
        overflow_arena,
    );

    if fallback.matches.is_empty() {
        result
    } else {
        fallback.literal_fallback = true;
        fallback
    }
}

#[allow(clippy::too_many_arguments)]
fn grep_search_parsed<'a>(
    files: &'a [FileItem],
    query: &FFFQuery<'_>,
    options: &GrepSearchOptions,
    budget: &ContentCacheBudget,
    bigram_index: Option<&BigramFilter>,
    bigram_overlay: Option<&BigramOverlay>,
    abort_signal: &AtomicBool,
    base_path: &Path,
    arena: crate::simd_path::ArenaPtr,
    overflow_arena: crate::simd_path::ArenaPtr,
) -> GrepResult<'a> {
    let total_files = files.live_count();
    let constraints_from_query = &query.constraints[..];

    let grep_text = extract_grep_text(query);
    if grep_text.is_empty() {
        return GrepResult::empty(total_files, total_files);
    }

    let case_insensitive = if options.smart_case {
        !grep_text.chars().any(|c| c.is_uppercase())
    } else {
        false
    };

    let base_count = bigram_boundary(bigram_overlay, files.len());

    let mut regex_fallback_error: Option<String> = None;
    let regex = match options.mode {
        GrepMode::PlainText => None,
        GrepMode::Fuzzy => {
            let bigram_candidates = fuzzy_candidates(bigram_index, bigram_overlay, &grep_text);

            let (files_to_search, filtered_file_count) = prefilter_with_filepath_retry(
                files,
                constraints_from_query,
                bigram_candidates.as_deref(),
                base_count,
                options,
                arena,
                overflow_arena,
            );

            if files_to_search.is_empty() {
                return GrepResult::empty(total_files, filtered_file_count);
            }

            return super::fuzzy_grep::fuzzy_grep_search(
                &grep_text,
                &files_to_search,
                options,
                total_files,
                filtered_file_count,
                case_insensitive,
                budget,
                abort_signal,
                base_path,
                arena,
                overflow_arena,
            );
        }
        GrepMode::Regex => build_regex(&grep_text, options.smart_case)
            .inspect_err(|err| {
                tracing::warn!("Regex compilation failed for {}. Error {}", grep_text, err);

                regex_fallback_error = Some(err.to_string());
            })
            .ok(),
    };

    let (multiline_segment_len, effective_pattern) = match replace_newline_escapes(&grep_text) {
        Some((replaced, first_newline_pos)) => (Some(first_newline_pos), replaced),
        None => (None, grep_text),
    };

    let is_multiline = multiline_segment_len.is_some();

    // when there is multiple line requested automatically expand the context to include all the lines
    let after_context = if is_multiline && regex.is_none() && options.after_context == 0 {
        effective_pattern.bytes().filter(|&b| b == b'\n').count()
    } else {
        options.after_context
    };

    let finder_pattern: Vec<u8> = if case_insensitive {
        effective_pattern.as_bytes().to_ascii_lowercase()
    } else {
        effective_pattern.as_bytes().to_vec()
    };
    let finder = NeedleFinder::new(&finder_pattern, case_insensitive);
    let pattern_len = finder_pattern.len() as u32;

    // PlainText (or regex-fallback-to-plain): literal bigram query.
    // Regex: decompose the pattern HIR into an AND/OR bigram query tree.
    let bigram_candidates = if regex.is_none() {
        literal_candidates(bigram_index, bigram_overlay, &[&effective_pattern])
    } else {
        regex_candidates(bigram_index, bigram_overlay, &effective_pattern)
    };

    let (files_to_search, filtered_file_count) = prefilter_with_filepath_retry(
        files,
        constraints_from_query,
        bigram_candidates.as_deref(),
        base_count,
        options,
        arena,
        overflow_arena,
    );

    if files_to_search.is_empty() {
        return GrepResult::empty(total_files, filtered_file_count);
    }

    // `PlainTextMatcher` is used by the grep-searcher engine for line detection.
    // `PlainTextSink` / `RegexSink` handle highlight extraction independently via ripgrep create
    let plain_matcher = PlainTextMatcher { finder: &finder };

    let searcher = {
        let mut b = SearcherBuilder::new();
        b.line_number(true).multi_line(is_multiline);
        b
    }
    .build();

    let should_prefilter = regex.is_none();
    let mut result = perform_grep(
        &files_to_search,
        options,
        &GrepContext {
            total_files,
            filtered_file_count,
            budget,
            base_path,
            arena,
            overflow_arena,
            prefilter: should_prefilter.then_some(&finder),
            abort_signal,
        },
        // The single sink-selection point: every mode's matcher/sink pairing
        // is decided here based on the compiled pattern.
        |file_bytes: &[u8], max_matches: usize| {
            let state = SinkState {
                file_index: 0,
                matches: Vec::with_capacity(4),
                max_matches,
                before_context: options.before_context,
                after_context,
                classify_definitions: options.classify_definitions,
            };

            match regex {
                Some(ref re) => {
                    let regex_matcher = RegexMatcher {
                        regex: re,
                        is_multiline,
                    };
                    let mut sink = RegexSink { state, re };
                    if let Err(e) = searcher.search_slice(&regex_matcher, file_bytes, &mut sink) {
                        tracing::error!(error = %e, "Grep (regex) search failed");
                    }
                    sink.state.matches
                }
                None => {
                    let mut sink = PlainTextSink {
                        state,
                        finder: &finder,
                        pattern_len,
                        multiline_segment_len,
                    };
                    if let Err(e) = searcher.search_slice(&plain_matcher, file_bytes, &mut sink) {
                        tracing::error!(error = %e, "Grep (plain text) search failed");
                    }
                    sink.state.matches
                }
            }
        },
    );
    result.regex_fallback_error = regex_fallback_error;
    result
}

/// Replace unescaped `\n` escapes with real newlines in a single pass.
///
/// Returns `Some((replaced, first_newline_pos))` when the pattern contained at
/// least one real `\n` escape (the user wants multiline search), where
/// `first_newline_pos` is the byte offset of the first inserted newline in the
/// replaced string. Returns `None` when nothing had to be replaced: `\\n` is
/// preserved as-is (escaped backslash + literal `n`, e.g. `\\nvim-data`).
pub(super) fn replace_newline_escapes(text: &str) -> Option<(String, usize)> {
    let bytes = text.as_bytes();
    let mut result = Vec::with_capacity(bytes.len());
    let mut first_newline_pos: Option<usize> = None;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            if bytes[i + 1] == b'n' {
                // Odd number of consecutive backslashes before 'n' -> real \n escape
                let mut backslash_count = 1;
                while backslash_count <= i && bytes[i - backslash_count] == b'\\' {
                    backslash_count += 1;
                }
                if backslash_count % 2 == 1 {
                    first_newline_pos.get_or_insert(result.len());
                    result.push(b'\n');
                    i += 2;
                    continue;
                }
            }
            result.push(bytes[i]);
            i += 1;
        } else {
            result.push(bytes[i]);
            i += 1;
        }
    }

    let first_newline_pos = first_newline_pos?;
    let replaced = String::from_utf8(result).unwrap_or_else(|_| text.to_string());
    Some((replaced, first_newline_pos))
}

pub fn parse_grep_query(query: &str) -> FFFQuery<'_> {
    let parser = QueryParser::new(GrepConfig);
    parser.parse(query)
}

/// Extract the grep pattern text from the parsed query: all non-constraint
/// tokens joined with spaces, e.g. `"name = *.rs someth"` -> `"name = someth"`
/// with constraint `Extension("rs")`.
fn extract_grep_text(query: &FFFQuery<'_>) -> String {
    if !matches!(query.fuzzy_query, fff_query_parser::FuzzyQuery::Empty) {
        return query.grep_text();
    }

    // if constraint-only or empty query we use raw_query for backslash-escape handling
    let t = query.raw_query.trim();
    if t.starts_with('\\') && t.len() > 1 {
        let suffix = &t[1..];
        let parser = QueryParser::new(GrepConfig);
        if !parser.parse(suffix).constraints.is_empty() {
            return suffix.to_string();
        }
    }
    t.to_string()
}

#[derive(Clone, Copy)]
pub(super) struct GrepContext<'a, 'b> {
    pub(super) total_files: usize,
    pub(super) filtered_file_count: usize,
    pub(super) budget: &'a ContentCacheBudget,
    pub(super) base_path: &'a Path,
    pub(super) arena: crate::simd_path::ArenaPtr,
    pub(super) overflow_arena: crate::simd_path::ArenaPtr,
    pub(super) prefilter: Option<&'a NeedleFinder<'b>>,
    pub(super) abort_signal: &'a AtomicBool,
}

impl GrepContext<'_, '_> {
    #[inline]
    fn arena_for_file(&self, file: &FileItem) -> crate::simd_path::ArenaPtr {
        if file.is_overflow() {
            self.overflow_arena
        } else {
            self.arena
        }
    }
}

#[tracing::instrument(
    skip_all,
    level = Level::DEBUG,
    fields(prefiltered_count = files_to_search.len())
)]
pub(super) fn perform_grep<'a, F>(
    files_to_search: &[&'a FileItem],
    options: &GrepSearchOptions,
    ctx: &GrepContext<'_, '_>,
    search_file: F,
) -> GrepResult<'a>
where
    F: Fn(&[u8], usize) -> Vec<GrepMatch> + Sync,
{
    let time_budget = if options.time_budget_ms > 0 {
        Some(std::time::Duration::from_millis(options.time_budget_ms))
    } else {
        None
    };

    let search_start = std::time::Instant::now();
    let page_limit = options.page_limit;
    // Lowest index an abort skipped; everything below it was searched, so it is
    // the resume point for the next page. usize::MAX means no abort happened.
    let first_skipped = AtomicUsize::new(usize::MAX);

    let mut result_files: Vec<&'a FileItem> = Vec::new();
    let mut all_matches: Vec<GrepMatch> = Vec::new();
    let mut files_consumed: usize = 0;
    let mut page_filled = false;
    let mut aborted = false;

    // Each chunk is a rayon barrier. A flat small chunk over 500k files = ~7800
    // barriers; x2 growth makes it logarithmic. But a too-aggressive growth
    // over-scans: when a page fills mid-chunk, the whole submitted chunk still
    // runs.
    //
    // So only grow when the prefilter is weak (large candidate set);
    // when bigram cut the set in half, keep fixed small chunks for cheap page-fill termination.
    let base_chunk = rayon::current_num_threads() * 4;
    let prefilter_strong = ctx.total_files > 0 && files_to_search.len() * 2 < ctx.total_files;
    let max_chunk = if prefilter_strong {
        base_chunk
    } else {
        (base_chunk * 256).max(8 * 1024)
    };
    let growth = if prefilter_strong { 1 } else { 2 };
    let mut chunk_size = base_chunk;
    let mut chunk_start = 0;

    while chunk_start < files_to_search.len() {
        let chunk_end = (chunk_start + chunk_size).min(files_to_search.len());
        let chunk = &files_to_search[chunk_start..chunk_end];
        chunk_start = chunk_end;
        chunk_size = (chunk_size * growth).min(max_chunk);
        let chunk_offset = files_consumed;

        // Unless enforced, the budget stays dormant until something matched.
        let budget = time_budget.filter(|_| options.enforce_time_budget || all_matches.len() > 1);

        let chunk_results: Vec<(usize, &'a FileItem, Vec<GrepMatch>)> = chunk
            .par_iter()
            .enumerate()
            .map_init(
                // tested it out a few times, this is just fine for rayon worker in this specific
                // case it doesn't reallocate this many times and it is actually faster than using
                // scoped threads with a predefined local scratch buffers because of spawn cost
                || (Vec::with_capacity(64 * 1024), MmapSlot::default(), false),
                |(buf, mmap_slot, aborted), (local_idx, file)| {
                    // check the clock on every 8th file only; `aborted` latches for the
                    // rest of this worker's range. File 0 is never skipped so a page
                    // always consumes at least one file (cursor 0 means "done").
                    let idx = chunk_offset + local_idx;
                    if !*aborted && local_idx % 8 == 0 && idx > 0 {
                        *aborted = ctx.abort_signal.load(Ordering::Relaxed)
                            || budget.is_some_and(|b| search_start.elapsed() > b);
                    }
                    if *aborted {
                        first_skipped.fetch_min(idx, Ordering::Relaxed);
                        return None;
                    }

                    let content = file.get_content_for_search(
                        buf,
                        mmap_slot,
                        ctx.arena_for_file(file),
                        ctx.base_path,
                        ctx.budget,
                    )?;

                    // Fast whole-file memmem check before entering the
                    // grep-searcher machinery. Skips Vec alloc, Searcher
                    // setup, and line-splitting for files that can't match.
                    if let Some(pf) = ctx.prefilter
                        && pf.find(content).is_none()
                    {
                        return None;
                    }

                    let file_matches = search_file(content, options.max_matches_per_file);

                    if file_matches.is_empty() {
                        return None;
                    }

                    Some((idx, *file, file_matches))
                },
            )
            .flatten()
            .collect();

        // Every file in the chunk was visited unless an abort cut it short.
        let resume_at = first_skipped.load(Ordering::Relaxed);
        aborted = resume_at != usize::MAX;
        files_consumed = resume_at.min(chunk_offset + chunk.len());

        for (batch_idx, file, file_matches) in chunk_results {
            // Matches past the abort point are dropped; the next page re-searches them.
            if batch_idx >= resume_at {
                continue;
            }

            let file_result_idx = result_files.len();
            result_files.push(file);

            for mut m in file_matches {
                m.file_index = file_result_idx;
                if options.trim_whitespace {
                    m.trim_leading_whitespace();
                }
                all_matches.push(m);
            }

            if all_matches.len() >= page_limit {
                // Tighten files_consumed to the file that tipped us over so
                // the next page resumes right after it.
                files_consumed = batch_idx + 1;
                page_filled = true;
                break;
            }
        }

        if page_filled || aborted {
            break;
        }
    }

    // No match and no abort means the whole slice was searched.
    if result_files.is_empty() && !aborted {
        files_consumed = files_to_search.len();
    }

    let has_more = (aborted || page_filled) && files_consumed < files_to_search.len();

    let next_file_offset = if has_more {
        options.file_offset + files_consumed
    } else {
        0
    };

    GrepResult {
        matches: all_matches,
        files_with_matches: result_files.len(),
        files: result_files,
        total_files_searched: files_consumed,
        total_files: ctx.total_files,
        filtered_file_count: ctx.filtered_file_count,
        next_file_offset,
        regex_fallback_error: None,
        literal_fallback: false,
    }
}
