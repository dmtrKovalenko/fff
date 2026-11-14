use crate::file_picker::FuzzySearchOptions;
use crate::grep::{GrepResult, GrepSearchOptions, grep_search, multi_grep_search};
use crate::index::{BigramFilter, BigramOverlay};
use crate::parallelism::SEARCH_THREAD_POOL;
use crate::query_tracker::QueryTracker;
use crate::score::{fuzzy_match_and_score_files, fuzzy_match_byte_offsets_for_page};
use crate::simd_path::ArenaPtr;
use crate::types::{ContentCacheBudget, FileItem, Score, ScoringContext, SearchResult};
use fff_query_parser::{Constraint, FFFQuery, FuzzyQuery};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use tracing::{debug, info};

// Everything a search needs to run over one set of files. The picker hands
// out its whole index, a lazy search hands out a segment of the running walk.
#[derive(Clone, Copy)]
pub(crate) struct SearchScope<'a> {
    pub(crate) files: &'a [FileItem],
    pub(crate) base_count: usize,
    pub(crate) live_count: usize,
    pub(crate) arena: ArenaPtr,
    pub(crate) overflow_arena: ArenaPtr,
    pub(crate) base_path: &'a Path,
    pub(crate) budget: &'a ContentCacheBudget,
    pub(crate) bigram_index: Option<&'a BigramFilter>,
}

impl<'a> SearchScope<'a> {
    pub(crate) fn fuzzy_search<'q>(
        &self,
        query: &'q FFFQuery<'q>,
        query_tracker: Option<&QueryTracker>,
        options: FuzzySearchOptions<'q>,
    ) -> SearchResult<'a> {
        let context = ScoringContext::for_search(query, query_tracker, &options);
        debug!(
            raw_query = ?query.raw_query,
            pagination = ?options.pagination,
            max_threads = context.max_threads,
            current_file = ?options.current_file,
            "Fuzzy search",
        );

        let time = std::time::Instant::now();
        let (items, scores, total_matched) = self.fuzzy_rank(&context);
        let match_byte_offsets = fuzzy_match_byte_offsets_for_page(
            query,
            &items,
            context.max_typos,
            self.arena,
            self.overflow_arena,
        );

        info!(
            ?query,
            completed_in = ?time.elapsed(),
            total_matched,
            returned_count = items.len(),
            pagination = ?options.pagination,
            "Fuzzy search completed",
        );

        SearchResult {
            items,
            scores,
            match_byte_offsets,
            total_matched,
            total_files: self.live_count,
            location: query.location,
        }
    }

    // Ranked page of the scope without highlight offsets.
    pub(crate) fn fuzzy_rank(
        &self,
        context: &ScoringContext,
    ) -> (Vec<&'a FileItem>, Vec<Score>, usize) {
        fuzzy_match_and_score_files(
            self.files,
            context,
            self.base_count,
            self.arena,
            self.overflow_arena,
        )
    }

    pub(crate) fn grep(
        &self,
        query: &FFFQuery<'_>,
        options: &GrepSearchOptions,
        bigram_overlay: Option<&BigramOverlay>,
        abort_signal: &AtomicBool,
    ) -> GrepResult<'a> {
        SEARCH_THREAD_POOL.install(|| {
            grep_search(
                self.files,
                self.live_count,
                query,
                options,
                self.budget,
                self.bigram_index,
                bigram_overlay,
                abort_signal,
                self.base_path,
                self.arena,
                self.overflow_arena,
            )
        })
    }

    pub(crate) fn multi_grep(
        &self,
        patterns: &[&str],
        constraints: &[Constraint<'_>],
        options: &GrepSearchOptions,
        bigram_overlay: Option<&BigramOverlay>,
        abort_signal: &AtomicBool,
    ) -> GrepResult<'a> {
        SEARCH_THREAD_POOL.install(|| {
            multi_grep_search(
                self.files,
                patterns,
                constraints,
                options,
                self.budget,
                self.bigram_index,
                bigram_overlay,
                abort_signal,
                self.base_path,
                self.arena,
                self.overflow_arena,
            )
        })
    }
}

impl<'q> ScoringContext<'q> {
    pub(crate) fn for_search(
        query: &'q FFFQuery<'q>,
        query_tracker: Option<&QueryTracker>,
        options: &FuzzySearchOptions<'q>,
    ) -> Self {
        let max_threads = if options.max_threads == 0 {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
        } else {
            options.max_threads
        };

        // Get effective query for max_typos calculation (without location suffix)
        let effective_query = match &query.fuzzy_query {
            FuzzyQuery::Text(t) => *t,
            FuzzyQuery::Parts(parts) if !parts.is_empty() => parts[0],
            _ => query.raw_query.trim(),
        };

        // Look up the last file selected for this query (combo-boost scoring)
        let last_same_query_match =
            query_tracker
                .zip(options.project_path)
                .and_then(|(tracker, project_path)| {
                    tracker
                        .get_last_query_entry(
                            query.raw_query,
                            project_path,
                            options.min_combo_count,
                        )
                        .ok()
                        .flatten()
                });

        Self {
            query,
            // small queries with a large number of results can match absolutely everything
            max_typos: (effective_query.len() as u16 / 4).clamp(2, 6),
            max_threads,
            project_path: options.project_path,
            current_file: options.current_file,
            last_same_query_match,
            combo_boost_score_multiplier: options.combo_boost_score_multiplier,
            min_combo_count: options.min_combo_count,
            pagination: options.pagination,
        }
    }
}
