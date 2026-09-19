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

    if !files_to_search.is_empty() {
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
    let plan = if constraints.is_empty() {
        None
    } else {
        Some(ConstraintPlan::build(constraints, files, arenas))
    };

    let mut scratch = ConstraintsBuffers::new();

    #[inline(always)]
    fn basic_prefilter(file: &FileItem, max: u64) -> bool {
        !file.is_deleted() && !file.is_binary() && file.size > 0 && file.size <= max
    }

    // squeeze as much prefilters into a single loop as possible
    let mut prefiltered: Vec<&FileItem> = match bigram_candidates {
        Some(candidates) => {
            let boundary = base_count.min(files.len());
            let (indexed, tail) = files.split_at(boundary);

            let cap = BigramFilter::count_candidates(candidates) + tail.len();
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
                        if !basic_prefilter(f, max_file_size) {
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
                if !basic_prefilter(f, max_file_size) {
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
                if !basic_prefilter(f, max_file_size) {
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
