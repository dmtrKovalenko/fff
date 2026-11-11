use super::types::GrepSearchOptions;
use crate::index::BigramFilter;
use crate::index::constraints::{ConstraintPlan, ConstraintsBuffers};
use crate::sort_buffer::sort_with_buffer;
use crate::types::FileItem;
use fff_query_parser::Constraint;
use std::cmp::Ordering;
use std::ops::Range;

const FIRST_RANKED: usize = 8192;

pub(super) struct RankedFiles<'a> {
    files: Vec<&'a FileItem>,
    ranked: usize,
    offset: usize,
}

fn rank(a: &&FileItem, b: &&FileItem) -> Ordering {
    b.total_frecency_score()
        .cmp(&a.total_frecency_score())
        .then_with(|| b.modified.cmp(&a.modified))
}

fn rank_with_ties(a: &&FileItem, b: &&FileItem) -> Ordering {
    // All references point into one input slice, whose order breaks ties.
    rank(a, b).then_with(|| std::ptr::from_ref(*a).cmp(&std::ptr::from_ref(*b)))
}

impl<'a> RankedFiles<'a> {
    fn new(mut files: Vec<&'a FileItem>, options: &GrepSearchOptions) -> Self {
        let offset = options.file_offset.min(files.len());
        let prefix = offset.saturating_add(FIRST_RANKED);
        let ranked = if offset == files.len() {
            files.len()
        } else if prefix <= files.len() / 8 && options.page_limit <= FIRST_RANKED {
            files.select_nth_unstable_by(prefix, rank_with_ties);
            files[..prefix].sort_unstable_by(rank_with_ties);
            prefix
        } else {
            sort_with_buffer(&mut files, rank);
            files.len()
        };
        Self {
            files,
            ranked,
            offset,
        }
    }

    pub(super) fn len(&self) -> usize {
        self.files.len() - self.offset
    }

    pub(super) fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub(super) fn batch(&mut self, range: Range<usize>) -> &[&'a FileItem] {
        let range = self.offset + range.start..self.offset + range.end;
        if range.end > self.ranked {
            self.files[self.ranked..].sort_unstable_by(rank_with_ties);
            self.ranked = self.files.len();
        }
        &self.files[range]
    }
}

/// Prefilter with a FilePath-constraint fallback: when constraints yield 0
/// files and the query had FilePath constraints, retry without them (the path
/// token was likely part of the search text).
#[allow(clippy::too_many_arguments)]
pub(super) fn prefilter_with_filepath_retry<'a>(
    files: &'a [FileItem],
    constraints: &[Constraint<'_>],
    bigram_candidates: Option<&[u64]>,
    base_count: usize,
    options: &GrepSearchOptions,
    arena: crate::simd_path::ArenaPtr,
    overflow_arena: crate::simd_path::ArenaPtr,
) -> (RankedFiles<'a>, usize) {
    let (files_to_search, filtered_file_count) = prefilter_files(
        files,
        constraints,
        bigram_candidates,
        base_count,
        options,
        arena,
        overflow_arena,
    );

    if !files_to_search.is_empty()
        || options.classify_definitions == super::DefinitionMode::DefinitionOnly
    {
        return (files_to_search, filtered_file_count);
    }

    let Some(stripped) = strip_file_path_constraint_if_present(constraints) else {
        return (files_to_search, filtered_file_count);
    };

    prefilter_files(
        files,
        &stripped,
        bigram_candidates,
        base_count,
        options,
        arena,
        overflow_arena,
    )
}

/// Single pass prefilter that doesn't involve file reading
/// allocates only amount of memory required for storing references of the FileItems have to be
/// opened for grepping unaviodably, in the worst case allocates N * <word> memory if no prefilter needed
pub(super) fn prefilter_files<'a>(
    files: &'a [FileItem],
    constraints: &[Constraint<'_>],
    bigram_candidates: Option<&[u64]>,
    base_count: usize,
    options: &GrepSearchOptions,
    arena: crate::simd_path::ArenaPtr,
    overflow_arena: crate::simd_path::ArenaPtr,
) -> (RankedFiles<'a>, usize) {
    let max_file_size = options.max_file_size;
    let definition_only = options.classify_definitions == super::DefinitionMode::DefinitionOnly;
    let candidate_count = bigram_candidates.map(BigramFilter::count_candidates);
    let plan = if constraints.is_empty() {
        None
    } else {
        // Sparse content candidates need only inline glob checks, not a full-index path pass.
        let sparse = candidate_count.is_some_and(|count| count < files.len() / 2);
        Some(ConstraintPlan::build_with_prefilter(
            constraints,
            files,
            arena,
            overflow_arena,
            sparse,
        ))
    };

    let mut scratch = ConstraintsBuffers::new();

    #[inline(always)]
    fn basic_prefilter(file: &FileItem, max: u64, definition_only: bool) -> bool {
        !file.is_deleted()
            && !file.is_binary()
            && file.size > 0
            && file.size <= max
            && (!definition_only || file.supports_definitions())
    }

    // squeeze as much prefilters into a single loop as possible
    let prefiltered: Vec<&FileItem> = match bigram_candidates {
        Some(candidates) => {
            let boundary = base_count.min(files.len());
            let (indexed, tail) = files.split_at(boundary);

            let cap = candidate_count.unwrap_or(0) + tail.len();
            let mut out: Vec<&FileItem> = Vec::with_capacity(cap);

            let full_words = boundary / 64;
            let last_word_bits = boundary % 64;

            // we need this because we already had a regression of the wrong bit
            // has been set for the very last word based on the overlay, it's pretty cheap
            macro_rules! evaluate_bigram_match_word {
                ($word:expr, $base:expr) => {{
                    let mut bits: u64 = $word;
                    while bits != 0 {
                        let bit = bits.trailing_zeros() as usize;
                        let file_idx = $base + bit;
                        bits &= bits - 1;

                        let f = unsafe { indexed.get_unchecked(file_idx) };
                        if !basic_prefilter(f, max_file_size, definition_only) {
                            continue;
                        }
                        if let Some(plan) = plan.as_ref()
                            && !plan.matches(f, file_idx, arena, overflow_arena, &mut scratch)
                        {
                            continue;
                        }
                        out.push(f);
                    }
                }};
            }

            // Full words: every set bit guaranteed `< boundary`.
            for (word_idx, &word) in candidates.iter().take(full_words).enumerate() {
                if word != 0 {
                    evaluate_bigram_match_word!(word, word_idx * 64);
                }
            }

            // Last partial word: mask bits past `boundary` once at word load.
            if last_word_bits != 0 {
                // this will get only (mod 64) bits from the last word guarantee that it's 0 padded
                let last_mask: u64 = (1u64 << last_word_bits) - 1;
                let word = candidates[full_words] & last_mask;
                if word != 0 {
                    evaluate_bigram_match_word!(word, full_words * 64);
                }
            }

            // Sequential processing for non-bigrammable files: they are always in the end
            for (offset, f) in tail.iter().enumerate() {
                if !basic_prefilter(f, max_file_size, definition_only) {
                    continue;
                }
                if let Some(ref p) = plan
                    && !p.matches(f, boundary + offset, arena, overflow_arena, &mut scratch)
                {
                    continue;
                }
                out.push(f);
            }

            out
        }
        // this will be executed if there is no bigram, in the worst case it will allocate
        // whole array of files but probability in the real repo of NO preflter working is so
        // low that we just ignore that, usually there would be at least a few files excluded
        None => {
            let mut out: Vec<&FileItem> = Vec::new();
            for (idx, f) in files.iter().enumerate() {
                if !basic_prefilter(f, max_file_size, definition_only) {
                    continue;
                }
                if let Some(ref p) = plan
                    && !p.matches(f, idx, arena, overflow_arena, &mut scratch)
                {
                    continue;
                }
                out.push(f);
            }
            out
        }
    };

    let total_count = prefiltered.len();

    (RankedFiles::new(prefiltered, options), total_count)
}

fn strip_file_path_constraint_if_present<'a>(
    constraints: &[Constraint<'a>],
) -> Option<fff_query_parser::ConstraintVec<'a>> {
    if !constraints
        .iter()
        .any(|c| matches!(c, Constraint::FilePath(_)))
    {
        return None;
    }

    let filtered: fff_query_parser::ConstraintVec<'a> = constraints
        .iter()
        .filter(|c| !matches!(c, Constraint::FilePath(_)))
        .cloned()
        .collect();

    Some(filtered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grep::DefinitionMode;
    use crate::simd_path::ArenaPtr;

    #[test]
    fn partial_ranking_matches_stable_sort_across_batch_boundaries() {
        let files: Vec<_> = (0..100_000)
            .map(|i| {
                let mut file = FileItem::new_raw(0, 1, (i * 17 % 31) as u64, None, false);
                file.access_frecency_score = (i * 13 % 7) as i16;
                file
            })
            .collect();
        let candidates: Vec<_> = files.iter().filter(|file| file.modified != 8).collect();
        let mut expected = candidates.clone();
        expected.sort_by(rank);
        for (offset, limit) in [
            (0, 50),
            (971, 50),
            (50_000, 50),
            (0, 10_000),
            (usize::MAX, 50),
        ] {
            let mut list = RankedFiles::new(
                candidates.clone(),
                &GrepSearchOptions {
                    file_offset: offset,
                    page_limit: limit,
                    ..Default::default()
                },
            );
            let expected = &expected[offset.min(expected.len())..];
            assert_eq!(list.len(), expected.len());
            let mut start = 0;
            for end in [1, FIRST_RANKED - 1, FIRST_RANKED + 3, list.len()] {
                let end = end.min(list.len());
                assert!(
                    list.batch(start..end)
                        .iter()
                        .zip(&expected[start..end])
                        .all(|(a, b)| std::ptr::eq(*a, *b))
                );
                start = end;
            }
        }
    }

    #[test]
    fn definition_only_prefilters_before_sorting_and_offsets() {
        let files: Vec<_> = (0..70)
            .map(|idx| {
                let size = match idx {
                    9 => 0,
                    12 => 101,
                    _ => 10,
                };
                let mut file = FileItem::new_raw(0, size, 1000 - idx, None, idx == 6);
                file.mark_definition_language(["file.rs", "file.txt", "file.py"][idx as usize % 3]);
                file.set_deleted(idx == 8);
                file.set_overflow(idx >= 65);
                file
            })
            .collect();
        let all = [u64::MAX, 1];
        let even = [0x5555_5555_5555_5555, 1];
        for candidates in [None, Some(all.as_slice()), Some(even.as_slice())] {
            for mode in [
                DefinitionMode::Off,
                DefinitionMode::Classify,
                DefinitionMode::DefinitionOnly,
                DefinitionMode::UsageOnly,
            ] {
                let expected: Vec<_> = (0..70)
                    .filter(|idx| ![6, 8, 9, 12].contains(idx))
                    .filter(|idx| mode != DefinitionMode::DefinitionOnly || idx % 3 != 1)
                    .filter(|idx| {
                        *idx >= 65
                            || candidates.is_none_or(|bits| bits[idx / 64] & (1 << (idx % 64)) != 0)
                    })
                    .map(|idx| 1000 - idx as u64)
                    .collect();
                for offset in [0, 2, expected.len(), expected.len() + 1] {
                    let options = GrepSearchOptions {
                        max_file_size: 100,
                        classify_definitions: mode,
                        file_offset: offset,
                        ..Default::default()
                    };
                    let (mut result, count) = prefilter_files(
                        &files,
                        &[],
                        candidates,
                        65,
                        &options,
                        ArenaPtr::null(),
                        ArenaPtr::null(),
                    );
                    assert_eq!(count, expected.len());
                    assert_eq!(
                        result
                            .batch(0..result.len())
                            .iter()
                            .map(|file| file.modified)
                            .collect::<Vec<_>>(),
                        expected[offset.min(expected.len())..],
                    );
                }
            }
        }
    }
}
