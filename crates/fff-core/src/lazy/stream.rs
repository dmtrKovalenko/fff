use super::segment::{FileRef, Segment, StreamedFiles};
use crate::file_picker::{FFFMode, FuzzySearchOptions};
use crate::git::GitDiscovery;
use crate::grep::{GrepMatch, GrepResult, GrepSearchOptions};
use crate::query_tracker::QueryTracker;
use crate::score::fuzzy_match_byte_offsets_for_page;
use crate::shared::SharedFrecency;
use crate::types::{ContentCacheBudget, PaginationArgs, Score, ScoringContext, SearchResult};
use crate::walk::{WalkCursor, WalkPull};
use fff_query_parser::FFFQuery;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

// Bounds the copy done under the walker lock and the time to the first results.
const MAX_SEGMENT_FILES: usize = 32 * 1024;

// A lazy search's view of the running initial walk.
pub(crate) struct LazyStream {
    pub(crate) cursor: WalkCursor,
    pub(crate) base_path: PathBuf,
    pub(crate) mode: FFFMode,
    pub(crate) cancelled: Arc<AtomicBool>,
    pub(crate) frecency: SharedFrecency,
}

pub(super) struct StreamedGrep {
    pub(super) store: StreamedFiles,
    pub(super) page: GrepPage,
    pub(super) complete: bool,
}

#[derive(Default)]
pub(super) struct GrepPage {
    matches: Vec<GrepMatch>,
    files: Vec<FileRef>,
    total_files: usize,
    filtered_file_count: usize,
    total_files_searched: usize,
    regex_fallback_error: Option<String>,
}

pub(super) struct StreamedFuzzy {
    pub(super) store: StreamedFiles,
    pub(super) page: FuzzyPage,
    pub(super) complete: bool,
}

pub(super) struct FuzzyPage {
    ranked: Vec<Candidate>,
    total_matched: usize,
    total_files: usize,
    max_typos: u16,
    pagination: PaginationArgs,
}

struct Candidate {
    file: FileRef,
    modified: u64,
    score: Score,
}

impl LazyStream {
    // Blocks until the scan either committed its index or gave up.
    pub(super) fn wait_settled(&self) {
        self.cursor.wait_settled();
    }

    // Greps segments as the walker produces them until the page is full.
    // `None` when the index has to answer instead.
    pub(super) fn grep(
        &mut self,
        query: &FFFQuery<'_>,
        options: &GrepSearchOptions,
    ) -> Option<StreamedGrep> {
        // Segments are dropped after the call, so nothing is worth caching.
        let budget = ContentCacheBudget::with_max_files(0);
        let cancelled = Arc::clone(&self.cancelled);
        let abort_signal = options.abort_signal.as_deref().unwrap_or(&cancelled);
        let time_budget =
            (options.time_budget_ms > 0).then(|| Duration::from_millis(options.time_budget_ms));
        let started = Instant::now();

        let mut store = StreamedFiles::default();
        let mut page = GrepPage::default();
        let mut complete = false;

        loop {
            // Same rule as the indexed grep: the budget sleeps until something matched.
            let budget_active = options.enforce_time_budget || page.matches.len() > 1;
            let deadline = time_budget.filter(|_| budget_active).map(|b| started + b);

            let batch = match self.cursor.next(MAX_SEGMENT_FILES, deadline) {
                WalkPull::Files(batch) => batch,
                WalkPull::Complete => {
                    complete = true;
                    break;
                }
                WalkPull::Idle => break,
                WalkPull::Lost => return None,
            };

            let segment = Segment::new(batch);
            let segment_options = GrepSearchOptions {
                file_offset: 0,
                page_limit: options.page_limit.saturating_sub(page.matches.len()),
                time_budget_ms: match deadline {
                    Some(deadline) => (deadline
                        .saturating_duration_since(Instant::now())
                        .as_millis() as u64)
                        .max(1),
                    None => options.time_budget_ms,
                },
                enforce_time_budget: budget_active,
                ..options.clone()
            };

            let result = segment.scope(&self.base_path, &budget).grep_parsed(
                query,
                &segment_options,
                abort_signal,
            );
            let stopped_early = result.next_file_offset != 0;
            page.total_files += segment.len();
            page.filtered_file_count += result.filtered_file_count;
            page.total_files_searched += result.total_files_searched;
            if page.regex_fallback_error.is_none() {
                page.regex_fallback_error = result.regex_fallback_error;
            }

            if !result.matches.is_empty() {
                let first_file = page.files.len();
                page.matches.extend(result.matches.into_iter().map(|mut m| {
                    m.file_index += first_file;
                    m
                }));
                let matched = segment.indices_of(&result.files);
                page.files.extend(store.push(segment, matched));
            }

            if stopped_early
                || page.matches.len() >= options.page_limit
                || abort_signal.load(Ordering::Relaxed)
                || deadline.is_some_and(|deadline| Instant::now() >= deadline)
            {
                break;
            }
        }

        // A constrained miss may need the literal retry, which only the index can run.
        if complete && page.matches.is_empty() && !query.constraints.is_empty() {
            return None;
        }

        self.apply_result_frecency(&mut store, &page.files);
        Some(StreamedGrep {
            store,
            page,
            complete,
        })
    }

    // Ranks segments as the walker produces them and keeps the best page.
    // `None` when the index has to answer instead.
    pub(super) fn fuzzy_search<'q>(
        &mut self,
        query: &'q FFFQuery<'q>,
        query_tracker: Option<&QueryTracker>,
        options: FuzzySearchOptions<'q>,
        time_budget: Option<Duration>,
    ) -> Option<StreamedFuzzy> {
        let context = ScoringContext::for_search(query, query_tracker, &options);
        let pagination = options.pagination;
        // A zero limit asks for every match.
        let keep = match pagination.limit {
            0 => usize::MAX,
            limit => pagination.offset.saturating_add(limit),
        };
        let segment_context = ScoringContext {
            pagination: PaginationArgs {
                offset: 0,
                limit: if keep == usize::MAX { 0 } else { keep },
            },
            ..context.clone()
        };

        let budget = ContentCacheBudget::with_max_files(0);
        let deadline = time_budget.map(|budget| Instant::now() + budget);
        let shared_base = self.shared_base();

        let mut store = StreamedFiles::default();
        let mut page = FuzzyPage {
            ranked: Vec::new(),
            total_matched: 0,
            total_files: 0,
            max_typos: context.max_typos,
            pagination,
        };
        let mut complete = false;

        loop {
            let batch = match self.cursor.next(MAX_SEGMENT_FILES, deadline) {
                WalkPull::Files(batch) => batch,
                WalkPull::Complete => {
                    complete = true;
                    break;
                }
                WalkPull::Idle => break,
                WalkPull::Lost => return None,
            };

            let mut segment = Segment::new(batch);
            if let Ok(guard) = self.frecency.read()
                && let Some(tracker) = guard.as_ref()
            {
                segment.apply_frecency(tracker, &self.base_path, shared_base.as_deref(), self.mode);
            }

            let (items, scores, matched) = segment
                .scope(&self.base_path, &budget)
                .fuzzy_rank(&segment_context);
            page.total_matched += matched;
            page.total_files += segment.len();

            if !items.is_empty() {
                let modified: Vec<u64> = items.iter().map(|file| file.modified).collect();
                let ranked = segment.indices_of(&items);
                let files = store.push(segment, ranked);
                page.ranked
                    .extend(files.into_iter().zip(modified).zip(scores).map(
                        |((file, modified), score)| Candidate {
                            file,
                            modified,
                            score,
                        },
                    ));
                if page.ranked.len() > keep {
                    page.ranked.sort_unstable_by(Candidate::rank);
                    page.ranked.truncate(keep);
                    store.retain(page.ranked.iter().map(|candidate| candidate.file));
                }
            }

            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                break;
            }
        }

        page.ranked.sort_unstable_by(Candidate::rank);
        Some(StreamedFuzzy {
            store,
            page,
            complete,
        })
    }

    // Walked files carry no frecency yet, only the returned ones are worth a lookup.
    fn apply_result_frecency(&self, store: &mut StreamedFiles, files: &[FileRef]) {
        if files.is_empty() {
            return;
        }
        if let Ok(guard) = self.frecency.read()
            && let Some(tracker) = guard.as_ref()
        {
            let shared_base = self.shared_base();
            store.update_frecency(
                files,
                tracker,
                &self.base_path,
                shared_base.as_deref(),
                self.mode,
            );
        }
    }

    // Linked worktrees key their frecency by the main checkout.
    fn shared_base(&self) -> Option<PathBuf> {
        let has_tracker = self.frecency.read().is_ok_and(|guard| guard.is_some());
        if !has_tracker {
            return None;
        }
        GitDiscovery::discover(&self.base_path)?.shared_base(&self.base_path)
    }
}

impl GrepPage {
    pub(super) fn into_result(self, store: &StreamedFiles) -> GrepResult<'_> {
        let files: Vec<_> = self
            .files
            .iter()
            .filter_map(|&file| store.file(file))
            .collect();

        GrepResult {
            matches: self.matches,
            files_with_matches: files.len(),
            files,
            total_files_searched: self.total_files_searched,
            total_files: self.total_files,
            filtered_file_count: self.filtered_file_count,
            // The stream order is not the index order, so the cursor can not carry over.
            next_file_offset: 0,
            regex_fallback_error: self.regex_fallback_error,
            literal_fallback: false,
        }
    }
}

impl FuzzyPage {
    pub(super) fn into_result<'a, 'q>(
        self,
        store: &'a StreamedFiles,
        query: &'q FFFQuery<'q>,
    ) -> SearchResult<'a> {
        let limit = match self.pagination.limit {
            0 => usize::MAX,
            limit => limit,
        };

        let mut result = SearchResult {
            total_matched: self.total_matched,
            total_files: self.total_files,
            location: query.location,
            ..Default::default()
        };
        for candidate in self
            .ranked
            .into_iter()
            .skip(self.pagination.offset)
            .take(limit)
        {
            let Some(file) = store.file(candidate.file) else {
                continue;
            };
            let arena = store.arena(candidate.file);
            let offsets =
                fuzzy_match_byte_offsets_for_page(query, &[file], self.max_typos, arena, arena);
            result.items.push(file);
            result.scores.push(candidate.score);
            result
                .match_byte_offsets
                .push(offsets.into_iter().next().unwrap_or_default());
        }
        result
    }
}

impl Candidate {
    // Same order as the indexed search: score, then recency, then a stable tie break.
    fn rank(a: &Self, b: &Self) -> std::cmp::Ordering {
        b.score
            .total
            .cmp(&a.score.total)
            .then_with(|| b.modified.cmp(&a.modified))
            .then_with(|| a.file.cmp(&b.file))
    }
}
