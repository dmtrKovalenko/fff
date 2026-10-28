use super::types::GrepSearchOptions;
use crate::index::BigramFilter;
use crate::index::constraints::{ConstraintPlan, ConstraintsBuffers};
use crate::sort_buffer::sort_with_buffer;
use crate::types::FileItem;
use fff_query_parser::Constraint;

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
    arenas: crate::index::layers::LayerArenas,
) -> (Vec<&'a FileItem>, usize) {
    let (files_to_search, filtered_file_count) = prefilter_files(
        files,
        constraints,
        bigram_candidates,
        base_count,
        options,
        arenas,
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
        arenas,
    )
}

/// Single pass prefilter that doesn't involve file reading
/// allocates only amount of memory required for storing references of the FileItems have to be
/// opened for grepping unaviodably, in the worst case allocates N * <word> memory if no prefilter needed
pub(crate) fn prefilter_files<'a>(
    files: &'a [FileItem],
    constraints: &[Constraint<'_>],
    bigram_candidates: Option<&[u64]>,
    base_count: usize,
    options: &GrepSearchOptions,
    arenas: crate::index::layers::LayerArenas,
) -> (Vec<&'a FileItem>, usize) {
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
            arenas,
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
    let mut prefiltered: Vec<&FileItem> = match bigram_candidates {
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
                            && !plan.matches(f, file_idx, arenas, &mut scratch)
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
                    && !p.matches(f, boundary + offset, arenas, &mut scratch)
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
                    && !p.matches(f, idx, arenas, &mut scratch)
                {
                    continue;
                }
                out.push(f);
            }
            out
        }
    };

    let total_count = prefiltered.len();

    sort_with_buffer(&mut prefiltered, |a, b| {
        b.total_frecency_score()
            .cmp(&a.total_frecency_score())
            .then(b.modified.cmp(&a.modified))
    });

    if options.file_offset > 0 && options.file_offset < total_count {
        let paginated = prefiltered.split_off(options.file_offset);
        (paginated, total_count)
    } else if options.file_offset >= total_count {
        (Vec::new(), total_count)
    } else {
        (prefiltered, total_count)
    }
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
                    let (result, count) = prefilter_files(
                        &files,
                        &[],
                        candidates,
                        65,
                        &options,
                        crate::LayerArenas::uniform(ArenaPtr::null()),
                    );
                    assert_eq!(count, expected.len());
                    assert_eq!(
                        result.iter().map(|file| file.modified).collect::<Vec<_>>(),
                        expected[offset.min(expected.len())..],
                    );
                }
            }
        }
    }
}
