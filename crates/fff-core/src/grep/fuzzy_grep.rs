use crate::simd_path::ArenaPtr;
use crate::types::{ContentCacheBudget, FileItem, MmapSlot};
use fff_grep::lines::LineStep;
use rayon::prelude::*;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::sink::{
    char_indices_to_byte_offsets, classify_definition, strip_line_terminators,
    truncate_display_bytes,
};
use super::types::{GrepMatch, GrepResult, GrepSearchOptions};

#[allow(clippy::too_many_arguments)]
pub(super) fn fuzzy_grep_search<'a>(
    grep_text: &str,
    files_to_search: &[&'a FileItem],
    options: &GrepSearchOptions,
    total_files: usize,
    filtered_file_count: usize,
    case_insensitive: bool,
    budget: &ContentCacheBudget,
    abort_signal: &AtomicBool,
    base_path: &Path,
    arena: ArenaPtr,
    overflow_arena: ArenaPtr,
) -> GrepResult<'a> {
    // max_typos controls how many *needle* characters can be unmatched.
    // A transposition (e.g. "shcema" -> "schema") costs ~1 typo with
    // default gap penalties. We scale max_typos by needle length:
    //   1-2 chars -> 0 typos (exact subsequence only)
    //   3-5 chars -> 1 typo
    //   6+  chars -> 2 typos
    // Cap at 2: higher values (3+) let the SIMD prefilter pass lines
    // missing key characters entirely (e.g. query "flvencodeX" matching
    // lines without 'l' or 'v'). Quality comes from the post-match filters.
    let max_typos = (grep_text.len() / 3).min(2);
    let scoring = neo_frizbee::Scoring {
        // Use default gap penalties. Higher values (e.g. 20) cause
        // smith-waterman to prefer *dropping needle chars* over paying
        // gap costs, which inflates the typo count and breaks
        // transposition matching ("shcema" -> "schema" becomes 3 typos instead of 1)
        exact_match_bonus: 100,
        // gap_open_penalty: 4,
        // gap_extend_penalty: 2,
        prefix_bonus: 0,
        capitalization_bonus: if case_insensitive { 0 } else { 4 },
        ..neo_frizbee::Scoring::default()
    };

    let matcher = neo_frizbee::Matcher::new(
        grep_text,
        &neo_frizbee::Config {
            // Use the real max_typos so frizbee's SIMD prefilter actually rejects non-matching lines (~2 SIMD instructions per line vs full SW scoring).
            max_typos: Some(max_typos as u16),
            sort: false,
            scoring,
            ..Default::default()
        },
    );

    // Minimum score threshold: 50% of a perfect contiguous match.
    // With default scoring (match_score=12, matching_case_bonus=4 = 16/char),
    // a transposition costs ~5 from a gap, keeping the score well above 50%
    let perfect_score = (grep_text.len() as u16) * 16;
    let min_score = (perfect_score * 50) / 100;

    // Target identifiers are often longer than the query due to delimiters
    // (e.g. query "flvencodepicture" -> "ff_flv_encode_picture_header" from ffmpeg)
    // Allow 3x needle length to accommodate underscore/dot-separated names
    let max_match_span = grep_text.len() * 3;
    let needle_len = grep_text.len();

    // Each delimiter (_, .) in the target creates a gap. A typical C/Rust
    // identifier like "ff_flv_encode_picture_header" has 4-5 underscores.
    // Scale generously so delimiter gaps don't reject valid matches.
    let max_gaps = (needle_len / 3).max(2);

    // If a file doesn't contain enough distinct needle characters just skip it
    let needle_bytes = grep_text.as_bytes();
    let mut unique_needle_chars: Vec<u8> = Vec::new();
    for &b in needle_bytes {
        let lo = b.to_ascii_lowercase();
        let hi = b.to_ascii_uppercase();
        if !unique_needle_chars.contains(&lo) {
            unique_needle_chars.push(lo);
        }
        if lo != hi && !unique_needle_chars.contains(&hi) {
            unique_needle_chars.push(hi);
        }
    }

    // How many distinct needle chars must appear in the file.
    // With max_typos allowed, we need at least (unique_count - max_typos)
    let unique_count = {
        let mut seen = [false; 256];
        for &b in needle_bytes {
            seen[b.to_ascii_lowercase() as usize] = true;
        }
        seen.iter().filter(|&&v| v).count()
    };
    let min_chars_required = unique_count.saturating_sub(max_typos);

    let time_budget = if options.time_budget_ms > 0 {
        Some(std::time::Duration::from_millis(options.time_budget_ms))
    } else {
        None
    };
    let search_start = std::time::Instant::now();
    // Lowest index an abort skipped, i.e. the resume point; usize::MAX means no abort.
    let first_skipped = AtomicUsize::new(usize::MAX);
    let max_matches_per_file = options.max_matches_per_file;

    // for fuzzy match we need a bit smarter chunking as the amount of work we have to perform is
    // exponentially larger than the original grep (and the nature of work is heavier), so in short we have to
    // understand if the approximate index prefilter got us a lot of candidates or not
    //
    // if we have a few candidates -> likely we have a lot of matches, so verify the check faster
    // if we have a lot of candidates -> rely on a larger chunk pipelining more parallel lines at once
    let page_limit = options.page_limit;
    let base_chunk = rayon::current_num_threads() * 4;
    let prefilter_strong = total_files > 0 && files_to_search.len() * 2 < total_files;
    let max_chunk = if prefilter_strong {
        base_chunk
    } else {
        (base_chunk * 256).max(8 * 1024)
    };

    let growth = if prefilter_strong { 1 } else { 2 };
    let mut chunk_size = base_chunk;
    let mut chunk_start = 0;
    let mut running_matches = 0usize;
    let mut per_file_results: Vec<(usize, &'a FileItem, Vec<GrepMatch>)> = Vec::new();

    while chunk_start < files_to_search.len() {
        let chunk_end = (chunk_start + chunk_size).min(files_to_search.len());
        let chunk = &files_to_search[chunk_start..chunk_end];
        let chunk_offset = chunk_start;
        chunk_start = chunk_end;
        chunk_size = (chunk_size * growth).min(max_chunk);

        // Parallel phase with `map_init`: each rayon worker thread clones the
        // matcher once and gets a reusable read buffer + mmap slot. Buffer holds
        // small files, slot holds fresh mmap for cache-miss files ≥ FRESH_MMAP_THRESHOLD.
        let chunk_results: Vec<(usize, &'a FileItem, Vec<GrepMatch>)> = chunk
            .par_iter()
            .enumerate()
            .map_init(
                || {
                    (
                        matcher.clone(),
                        Vec::with_capacity(64 * 1024),
                        MmapSlot::default(),
                    )
                },
                |(matcher, buf, mmap_slot), (local_idx, file)| {
                    // File 0 is never skipped so a page always consumes at least one
                    // file (cursor 0 means "done").
                    let idx = chunk_offset + local_idx;
                    if idx > 0
                        && (abort_signal.load(Ordering::Relaxed)
                            || time_budget.is_some_and(|b| search_start.elapsed() > b))
                    {
                        first_skipped.fetch_min(idx, Ordering::Relaxed);
                        return None;
                    }

                    let file_arena = if file.is_overflow() {
                        overflow_arena
                    } else {
                        arena
                    };

                    let file_bytes =
                        file.get_content_for_search(buf, mmap_slot, file_arena, base_path, budget)?;

                    if min_chars_required > 0 {
                        let mut chars_found = 0usize;
                        for &ch in &unique_needle_chars {
                            if memchr::memchr(ch, file_bytes).is_some() {
                                chars_found += 1;
                                if chars_found >= min_chars_required {
                                    break;
                                }
                            }
                        }
                        if chars_found < min_chars_required {
                            return None;
                        }
                    }

                    // Validate the whole file as UTF-8 once upfront. Source code
                    // files are virtually always valid UTF-8; this single check
                    // replaces per-line from_utf8 calls (~8% of fuzzy grep time)
                    let file_is_utf8 = std::str::from_utf8(file_bytes).is_ok();

                    let mut stepper = LineStep::new(b'\n', 0, file_bytes.len());
                    let estimated_lines = (file_bytes.len() / 40).max(64);
                    let mut file_lines: Vec<&str> = Vec::with_capacity(estimated_lines);
                    let mut line_meta: Vec<(u64, u64)> = Vec::with_capacity(estimated_lines);

                    let mut line_number: u64 = 1;
                    while let Some(line_match) = stepper.next_match(file_bytes) {
                        let byte_offset = line_match.start() as u64;
                        let trimmed = strip_line_terminators(&file_bytes[line_match]);

                        if !trimmed.is_empty() {
                            // we know for sure that the file is UTF-8 at this point
                            let line_str = if file_is_utf8 {
                                unsafe { std::str::from_utf8_unchecked(trimmed) }
                            } else if let Ok(s) = std::str::from_utf8(trimmed) {
                                s
                            } else {
                                line_number += 1;
                                continue;
                            };
                            file_lines.push(line_str);
                            line_meta.push((line_number, byte_offset));
                        }

                        line_number += 1;
                    }

                    if file_lines.is_empty() {
                        return None;
                    }

                    // Single-pass: score + indices in one Smith-Waterman run per line (not parallel)
                    let matches_with_indices = matcher.match_list_indices(&file_lines);
                    let mut file_matches: Vec<GrepMatch> = Vec::new();

                    for mut match_indices in matches_with_indices {
                        if match_indices.score < min_score {
                            continue;
                        }

                        let idx = match_indices.index as usize;
                        let raw_line = file_lines[idx];

                        let truncated = truncate_display_bytes(raw_line.as_bytes());
                        let display_line = if truncated.len() < raw_line.len() {
                            // SAFETY: truncate_display_bytes preserves UTF-8 char boundaries
                            &raw_line[..truncated.len()]
                        } else {
                            raw_line
                        };

                        // If the line was truncated, re-compute indices on the shorter string.
                        if display_line.len() < raw_line.len() {
                            let Some(re_indices) = matcher
                                .match_list_indices(&[display_line])
                                .into_iter()
                                .next()
                            else {
                                continue;
                            };
                            match_indices = re_indices;
                        }

                        match_indices.indices.sort_unstable();

                        // Minimum matched chars: at least (needle_len - max_typos)
                        // characters must appear. This is consistent with the typo
                        // budget: each typo can drop one needle char from the alignment.
                        let min_matched = needle_len.saturating_sub(max_typos).max(1);
                        if match_indices.indices.len() < min_matched {
                            continue;
                        }

                        let indices = &match_indices.indices;

                        if let (Some(&first), Some(&last)) = (indices.first(), indices.last()) {
                            // reject widely scattered matches
                            let span = last - first + 1;
                            if span > max_match_span {
                                continue;
                            }

                            // Density check: matched chars / span must be dense enough.
                            // Relaxed for perfect subsequence matches (all needle chars
                            // present), slightly relaxed for typo matches to handle
                            // delimiter-heavy targets
                            // (e.g. "ff_flv_encode_picture_header" has span inflated by underscores w/ density ~68%)
                            let density = (indices.len() * 100) / span;
                            let min_density = if indices.len() >= needle_len {
                                45 // Perfect subsequence relaxed (delimiters inflate span)
                            } else {
                                65 // Has typos filter out a long string
                            };
                            if density < min_density {
                                continue;
                            }

                            // Gap count check: count discontinuities in the indices
                            let gap_count = indices.windows(2).filter(|w| w[1] != w[0] + 1).count();
                            if gap_count > max_gaps {
                                continue;
                            }
                        }

                        let (ln, bo) = line_meta[idx];
                        let match_byte_offsets =
                            char_indices_to_byte_offsets(display_line, &match_indices.indices);
                        let col = match_byte_offsets
                            .first()
                            .map(|r| r.0 as usize)
                            .unwrap_or(0);

                        file_matches.push(GrepMatch {
                            file_index: 0,
                            line_number: ln,
                            col,
                            byte_offset: bo,
                            is_definition: classify_definition(
                                options.classify_definitions,
                                display_line,
                            ),
                            line_content: display_line.to_string(),
                            match_byte_offsets,
                            fuzzy_score: Some(match_indices.score),
                            context_before: Vec::new(),
                            context_after: Vec::new(),
                        });

                        if max_matches_per_file != 0 && file_matches.len() >= max_matches_per_file {
                            break;
                        }
                    }

                    if file_matches.is_empty() {
                        return None;
                    }

                    Some((idx, *file, file_matches))
                },
            )
            .flatten()
            .collect();

        for result in chunk_results {
            running_matches += result.2.len();
            per_file_results.push(result);
        }

        if running_matches >= page_limit || first_skipped.load(Ordering::Relaxed) != usize::MAX {
            break;
        }
    }

    let abort_resume = Some(first_skipped.load(Ordering::Relaxed)).filter(|&i| i != usize::MAX);
    GrepResult::collect(
        per_file_results,
        files_to_search.len(),
        options,
        total_files,
        filtered_file_count,
        abort_resume,
    )
}
