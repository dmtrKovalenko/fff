use super::grep::{GrepContext, perform_grep};
use super::prefilter::prefilter_files;
use super::sink::{SinkState, debug_assert_newline_terminator};
use super::types::{GrepResult, GrepSearchOptions};
use crate::index::{BigramFilter, BigramOverlay, bigram_boundary, literal_candidates};
use crate::types::{ContentCacheBudget, FileItem, FileSliceExt};
use aho_corasick::AhoCorasick;
use fff_grep::{
    Searcher, SearcherBuilder, Sink, SinkMatch,
    matcher::{Match, Matcher, NoError},
};
use smallvec::SmallVec;
use std::path::Path;
use std::sync::atomic::AtomicBool;

/// A `grep_matcher::Matcher` backed by Aho-Corasick for multi-pattern search.
///
/// Finds the first occurrence of any pattern starting at the given offset.
/// Always reports `\n` as the line terminator for the fast candidate-line path.
struct AhoCorasickMatcher<'a> {
    ac: &'a AhoCorasick,
}

impl Matcher for AhoCorasickMatcher<'_> {
    type Error = NoError;

    #[inline]
    fn find_at(&self, haystack: &[u8], at: usize) -> std::result::Result<Option<Match>, NoError> {
        let hay = &haystack[at..];
        let found: Option<aho_corasick::Match> = self.ac.find(hay);
        Ok(found.map(|m| Match::new(at + m.start(), at + m.end())))
    }

    #[inline]
    fn line_terminator(&self) -> Option<fff_grep::LineTerminator> {
        Some(fff_grep::LineTerminator::byte(b'\n'))
    }
}

/// Sink for Aho-Corasick multi-pattern mode.
///
/// Collects all pattern match positions on each matched line for highlighting.
struct AhoCorasickSink<'a> {
    state: SinkState,
    ac: &'a AhoCorasick,
}

impl Sink for AhoCorasickSink<'_> {
    type Error = std::io::Error;

    fn matched(&mut self, searcher: &Searcher, mat: &SinkMatch<'_>) -> Result<bool, Self::Error> {
        debug_assert_newline_terminator(searcher);
        if self.state.max_matches != 0 && self.state.matches.len() >= self.state.max_matches {
            return Ok(false);
        }

        let line_bytes = mat.bytes();
        let (display_bytes, display_len, line_number, byte_offset) =
            SinkState::prepare_line(line_bytes, mat);

        let line_content = String::from_utf8_lossy(display_bytes).into_owned();
        let mut match_byte_offsets: SmallVec<[(u32, u32); 4]> = SmallVec::new();
        let mut col = 0usize;
        let mut first = true;

        for m in self.ac.find_iter(display_bytes as &[u8]) {
            let abs_start = m.start() as u32;
            let abs_end = (m.end() as u32).min(display_len);
            if first {
                col = abs_start as usize;
                first = false;
            }
            match_byte_offsets.push((abs_start, abs_end));
        }

        let (context_before, context_after) = self.state.extract_context(mat);
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

/// Multi-pattern OR search using Aho-Corasick.
///
/// Builds a single automaton from all patterns and searches each file in one
/// pass. This is significantly faster than regex alternation for literal text
/// searches because Aho-Corasick uses SIMD-accelerated multi-needle matching.
///
/// Returns the same `GrepResult` type as `grep_search`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn multi_grep_search<'a>(
    files: &'a [FileItem],
    patterns: &[&str],
    constraints: &[fff_query_parser::Constraint<'_>],
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

    if patterns.is_empty() || patterns.iter().all(|p| p.is_empty()) {
        return GrepResult::empty(total_files, total_files);
    }

    let bigram_candidates = literal_candidates(bigram_index, bigram_overlay, patterns);
    let base_file_count = bigram_boundary(bigram_overlay, files.len());

    // Constraints are separate from patterns, so a miss must not broaden the search.
    let (files_to_search, filtered_file_count) = prefilter_files(
        files,
        constraints,
        bigram_candidates.as_deref(),
        base_file_count,
        options,
        arena,
        overflow_arena,
    );

    if files_to_search.is_empty() {
        return GrepResult::empty(total_files, filtered_file_count);
    }

    let casing = options.effective_casing();
    let case_insensitive = patterns.iter().all(|p| casing.is_insensitive_for(p));

    let ac = aho_corasick::AhoCorasickBuilder::new()
        .ascii_case_insensitive(case_insensitive)
        .build(patterns)
        .expect("Aho-Corasick build should not fail for literal patterns");

    let searcher = {
        let mut b = SearcherBuilder::new();
        b.line_number(true);
        b
    }
    .build();

    let ac_matcher = AhoCorasickMatcher { ac: &ac };
    perform_grep(
        &files_to_search,
        options,
        &GrepContext {
            total_files,
            filtered_file_count,
            budget,
            base_path,
            arena,
            overflow_arena,
            prefilter: None, // no memmem prefilter for multi-pattern search
            abort_signal,
        },
        |file_bytes: &[u8], max_matches: usize| {
            let state = SinkState {
                file_index: 0,
                matches: Vec::with_capacity(4),
                max_matches,
                before_context: options.before_context,
                after_context: options.after_context,
                classify_definitions: options.classify_definitions,
            };

            let mut sink = AhoCorasickSink { state, ac: &ac };

            if let Err(e) = searcher.search_slice(&ac_matcher, file_bytes, &mut sink) {
                tracing::error!(error = %e, "Grep (aho-corasick multi) search failed");
            }

            sink.state.matches
        },
    )
}
