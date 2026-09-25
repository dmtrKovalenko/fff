use crate::{
    git::is_modified_status,
    index::constraints::apply_constraints,
    match_offsets::char_indices_to_byte_offsets,
    path_utils::DirectoryDistance,
    simd_path::{ArenaPtr, MAX_PATH_CHUNKS},
    sort_buffer::sort_with_buffer,
    types::{DirItem, FileItem, Score, ScoringContext},
};
use fff_query_parser::{FFFQuery, FuzzyQuery};
use neo_frizbee::Scoring;
use rayon::prelude::*;
use smallvec::SmallVec;
use std::{borrow::Cow, path::MAIN_SEPARATOR};

enum FileItems<'a> {
    All(&'a [FileItem]),
    Filtered(Vec<&'a FileItem>),
}

impl<'a> FileItems<'a> {
    #[inline]
    fn len(&self) -> usize {
        match self {
            FileItems::All(s) => s.len(),
            FileItems::Filtered(v) => v.len(),
        }
    }

    #[inline]
    fn index(&self, index: usize) -> &'a FileItem {
        match self {
            FileItems::All(s) => &s[index],
            FileItems::Filtered(v) => v[index],
        }
    }
}

/// Resolve a FileItem's chunked path into frizbee's pointer buffer.
/// Returns `Some((chunk_count, byte_len))` or `None` for deleted files.
#[inline]
fn resolve_file_chunks(
    file: &FileItem,
    arena: ArenaPtr,
    buf: &mut [*const u8; MAX_PATH_CHUNKS],
) -> Option<(usize, u16)> {
    if file.is_deleted() {
        return None;
    }
    let ptrs = file.path.resolve_ptrs(arena, buf);
    Some((ptrs.len(), file.path.byte_len))
}

#[inline]
fn match_fuzzy_parts(
    fuzzy_parts: &[&str],
    working_files: &FileItems<'_>,
    options: &neo_frizbee::Config,
    max_threads: usize,
    arena: ArenaPtr,
) -> Vec<neo_frizbee::Match> {
    let valid_parts: SmallVec<[&str; 4]> = fuzzy_parts
        .iter()
        .copied()
        .filter(|p| p.len() >= 2)
        .collect();

    if valid_parts.is_empty() {
        tracing::debug!("match_fuzzy_parts: no valid parts after filtering, returning empty");
        return vec![];
    }

    // Index-based resolver: no `Vec<&FileItem>` materialization for either the
    // full list or the per-part subsets, and a single monomorphized hot loop.
    let first_part_matches = neo_frizbee::match_range_parallel_resolved(
        valid_parts[0],
        working_files.len(),
        &|index, buf: &mut [*const u8; MAX_PATH_CHUNKS]| {
            resolve_file_chunks(working_files.index(index as usize), arena, buf)
        },
        options,
        max_threads,
    );

    if valid_parts.len() == 1 {
        return first_part_matches;
    }

    let total_parts = valid_parts.len() as u32;
    let mut matches = first_part_matches;
    for part in valid_parts[1..].iter() {
        let mut part_options = *options;
        part_options.max_typos = options.max_typos.map(|t| t.min(part.len() as u16));

        // Match only the files that survived the previous round, addressed
        // through the previous matches without collecting a subset.
        let survivors = &matches;
        let sub_matches = neo_frizbee::match_range_parallel_resolved(
            part,
            survivors.len(),
            &|index, buf: &mut [*const u8; MAX_PATH_CHUNKS]| {
                let file = working_files.index(survivors[index as usize].index as usize);
                resolve_file_chunks(file, arena, buf)
            },
            &part_options,
            max_threads,
        );

        if sub_matches.is_empty() {
            return vec![]; // break early! 
        }

        // Map sub_matches back to original indices, average scores across all parts.
        matches = sub_matches
            .into_iter()
            .map(|sm| {
                let prev = &matches[sm.index as usize];
                let sum = (prev.score as u32).saturating_add(sm.score as u32);
                let avg = sum / total_parts;

                neo_frizbee::Match {
                    index: prev.index,
                    score: avg.min(u16::MAX as u32) as u16,
                    end_col: prev.end_col, // keep first part's position for filename bonus
                    exact: prev.exact && sm.exact,
                }
            })
            .collect();
    }

    matches
}

/// Match + score across base and overflow files, each with their own arena.
#[tracing::instrument(skip_all, level = tracing::Level::DEBUG)]
pub(crate) fn fuzzy_match_and_score_files<'a>(
    files: &'a [FileItem],
    context: &ScoringContext,
    base_count: usize,
    base_arena: ArenaPtr,
    overflow_arena: ArenaPtr,
) -> (Vec<&'a FileItem>, Vec<Score>, usize) {
    if fuzzy_parts(context.query).is_none() {
        return rank_by_frecency(files, context, base_count, base_arena, overflow_arena);
    }

    // Process overflow files first: newly added files (created after the
    // initial scan) live in the overflow arena and are more likely to be
    // relevant to the current search.
    //
    // putting them first in the list makes sorting more efficient and gives
    // them tiebreaker advantage in case sorting is the same
    let results = if files.len() > base_count {
        let mut results = match_and_score_in_arena(&files[base_count..], context, overflow_arena);

        results.extend(match_and_score_in_arena(
            &files[..base_count],
            context,
            base_arena,
        ));

        results
    } else {
        match_and_score_in_arena(files, context, base_arena)
    };

    sort_and_paginate(results, context, |score| score.total)
}

// Ranks on a 16-byte (file, total) pair and builds the full Score only for the page.
fn rank_by_frecency<'a>(
    files: &'a [FileItem],
    context: &ScoringContext,
    base_count: usize,
    base_arena: ArenaPtr,
    overflow_arena: ArenaPtr,
) -> (Vec<&'a FileItem>, Vec<Score>, usize) {
    let base_count = base_count.min(files.len());
    let mut totals: Vec<(&FileItem, i32)> = Vec::new();
    for (slice, arena) in [
        (&files[base_count..], overflow_arena),
        (&files[..base_count], base_arena),
    ] {
        if slice.is_empty() {
            continue;
        }
        let Some(working_files) = filter_by_constraints(slice, context, arena) else {
            continue;
        };
        frecency_totals(&working_files, context, arena, &mut totals);
    }

    let (items, _, total_matched) = sort_and_paginate(totals, context, |total| *total);
    let scores = items
        .iter()
        .map(|file| {
            let arena = if file.is_overflow() {
                overflow_arena
            } else {
                base_arena
            };
            frecency_score(file, context, arena)
        })
        .collect();
    (items, scores, total_matched)
}

fn fuzzy_parts<'q>(query: &'q FFFQuery<'q>) -> Option<&'q [&'q str]> {
    match &query.fuzzy_query {
        FuzzyQuery::Text(t) if t.len() >= 2 => Some(std::slice::from_ref(t)),
        FuzzyQuery::Parts(parts) if !parts.is_empty() => Some(parts.as_slice()),
        _ => None,
    }
}

// None when constraints exclude every file.
fn filter_by_constraints<'a>(
    files: &'a [FileItem],
    context: &ScoringContext,
    arena: ArenaPtr,
) -> Option<FileItems<'a>> {
    let constraints = &context.query.constraints;
    if constraints.is_empty() {
        return Some(FileItems::All(files));
    }
    match apply_constraints(files, constraints, arena, arena) {
        Some(filtered) if !filtered.is_empty() => Some(FileItems::Filtered(filtered)),
        Some(_) => None,
        None => Some(FileItems::All(files)),
    }
}

pub(crate) fn fuzzy_match_byte_offsets_for_page<'q>(
    query: &'q FFFQuery<'q>,
    items: &[&FileItem],
    max_typos: u16,
    base_arena: ArenaPtr,
    overflow_arena: ArenaPtr,
) -> Vec<SmallVec<[(u32, u32); 4]>> {
    let parts: SmallVec<[&str; 4]> = match &query.fuzzy_query {
        FuzzyQuery::Text(text) if text.len() >= 2 => smallvec::smallvec![*text],
        FuzzyQuery::Parts(parts) => parts.iter().copied().filter(|p| p.len() >= 2).collect(),
        _ => SmallVec::new(),
    };

    let mut ranges_by_item = vec![SmallVec::new(); items.len()];
    if parts.is_empty() || items.is_empty() {
        return ranges_by_item;
    }

    let mut paths = String::with_capacity(items.iter().map(|item| item.relative_path_len()).sum());
    for item in items {
        let arena = if item.is_overflow() {
            overflow_arena
        } else {
            base_arena
        };
        item.path.append_to_string(arena, &mut paths);
    }

    let mut start = 0;
    let path_strs: Vec<&str> = items
        .iter()
        .map(|item| {
            let end = start + item.relative_path_len();
            let path = &paths[start..end];
            start = end;
            path
        })
        .collect();

    let has_uppercase = parts
        .iter()
        .any(|part| part.chars().any(|ch| ch.is_uppercase()));
    let config = neo_frizbee::Config {
        max_typos: Some(max_typos),
        sort: neo_frizbee::SortStrategy::Unsorted,
        scoring: Scoring {
            capitalization_bonus: if has_uppercase { 8 } else { 0 },
            matching_case_bonus: if has_uppercase { 4 } else { 0 },
            ..Default::default()
        },
        ..Default::default()
    };

    for (idx, part) in parts.iter().copied().enumerate() {
        let mut part_config = config;
        if idx > 0 {
            part_config.max_typos = config.max_typos.map(|t| t.min(part.len() as u16));
        }

        let mut matcher = neo_frizbee::Matcher::new(part, &part_config);
        for mut matched in matcher.match_list_indices(&path_strs) {
            let item_idx = matched.index as usize;
            let Some(path) = path_strs.get(item_idx) else {
                continue;
            };

            matched.indices.sort_unstable();
            ranges_by_item[item_idx].extend(char_indices_to_byte_offsets(path, &matched.indices));
        }
    }

    for ranges in &mut ranges_by_item {
        *ranges = merge_byte_offsets(std::mem::take(ranges));
    }

    ranges_by_item
}

fn merge_byte_offsets(mut ranges: SmallVec<[(u32, u32); 4]>) -> SmallVec<[(u32, u32); 4]> {
    if ranges.len() <= 1 {
        return ranges;
    }

    ranges.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    let mut merged = 0;
    for index in 0..ranges.len() {
        let (start, end) = ranges[index];
        if end <= start {
            continue;
        }

        if merged > 0 && start <= ranges[merged - 1].1 {
            let last = &mut ranges[merged - 1];
            last.1 = last.1.max(end);
            continue;
        }

        ranges[merged] = (start, end);
        merged += 1;
    }

    ranges.truncate(merged);
    ranges
}

/// Resolve a DirItem's chunked path into frizbee's pointer buffer.
#[inline]
fn resolve_dir_chunks(
    dir: &DirItem,
    arena: ArenaPtr,
    overflow_arena: ArenaPtr,
    buf: &mut [*const u8; MAX_PATH_CHUNKS],
) -> Option<(usize, u16)> {
    let arena = if dir.is_overflow() {
        overflow_arena
    } else {
        arena
    };
    let ptrs = dir.path.resolve_ptrs(arena, buf);
    Some((ptrs.len(), dir.path.byte_len))
}

/// Run SIMD fuzzy match across directory items, narrowing the candidate set
/// through each query part — same pipeline as file matching.
fn match_fuzzy_parts_dirs(
    fuzzy_parts: &[&str],
    working_dirs: &[&DirItem],
    options: &neo_frizbee::Config,
    max_threads: usize,
    arena: ArenaPtr,
    overflow_arena: ArenaPtr,
) -> Vec<neo_frizbee::Match> {
    let valid_parts: SmallVec<[&str; 4]> = fuzzy_parts
        .iter()
        .copied()
        .filter(|p| p.len() >= 2)
        .collect();

    if valid_parts.is_empty() {
        return vec![];
    }

    let first_part_matches = neo_frizbee::match_range_parallel_resolved(
        valid_parts[0],
        working_dirs.len(),
        &|index, buf: &mut [*const u8; MAX_PATH_CHUNKS]| {
            resolve_dir_chunks(working_dirs[index as usize], arena, overflow_arena, buf)
        },
        options,
        max_threads,
    );

    if valid_parts.len() == 1 {
        return first_part_matches;
    }

    let mut matches = first_part_matches;
    let total_parts = valid_parts.len() as u32;
    for part in valid_parts[1..].iter() {
        let mut part_options = *options;
        part_options.max_typos = options.max_typos.map(|t| t.min(part.len() as u16));

        // Match only the dirs that survived the previous round, addressed
        // through the previous matches without collecting a subset.
        let survivors = &matches;
        let sub_matches = neo_frizbee::match_range_parallel_resolved(
            part,
            survivors.len(),
            &|index, buf: &mut [*const u8; MAX_PATH_CHUNKS]| {
                let dir = working_dirs[survivors[index as usize].index as usize];
                resolve_dir_chunks(dir, arena, overflow_arena, buf)
            },
            &part_options,
            max_threads,
        );

        if sub_matches.is_empty() {
            return vec![];
        }

        // Map sub_matches back to original indices, average scores across all parts.
        matches = sub_matches
            .into_iter()
            .map(|sm| {
                let prev = &matches[sm.index as usize];
                let sum = (prev.score as u32).saturating_add(sm.score as u32);
                let avg = sum / total_parts;

                neo_frizbee::Match {
                    index: prev.index,
                    score: avg.min(u16::MAX as u32) as u16,
                    end_col: prev.end_col, // keep first part's position for dirname bonus
                    exact: prev.exact && sm.exact,
                }
            })
            .collect();
    }

    matches
}

/// Match + score directories against a fuzzy query.
/// Scoring: base_score, frecency_boost, distance_penalty, dirname_bonus.
#[tracing::instrument(skip_all, level = tracing::Level::DEBUG)]
pub(crate) fn fuzzy_match_and_score_dirs<'a>(
    dirs: &'a [DirItem],
    context: &ScoringContext,
    arena: ArenaPtr,
    overflow_arena: ArenaPtr,
) -> (Vec<&'a DirItem>, Vec<Score>, usize) {
    if dirs.is_empty() {
        return (vec![], vec![], 0);
    }

    let parsed_query = context.query;
    // Ghost dirs (all files tombstoned) never surface in search results.
    let working_dirs: Vec<&DirItem> = if parsed_query.constraints.is_empty() {
        dirs.iter().filter(|d| !d.is_deleted()).collect()
    } else {
        match apply_constraints(dirs, &parsed_query.constraints, arena, overflow_arena) {
            Some(filtered) if !filtered.is_empty() => {
                filtered.into_iter().filter(|d| !d.is_deleted()).collect()
            }
            Some(_) => return (vec![], vec![], 0),
            None => dirs.iter().filter(|d| !d.is_deleted()).collect(),
        }
    };

    let fuzzy_parts: &[&str] = match &parsed_query.fuzzy_query {
        FuzzyQuery::Text(t) if t.len() >= 2 => std::slice::from_ref(t),
        FuzzyQuery::Parts(parts) if !parts.is_empty() => parts.as_slice(),
        _ => {
            return score_dirs_by_frecency(&working_dirs, context);
        }
    };

    let valid_parts: SmallVec<[&str; 4]> = fuzzy_parts
        .iter()
        .copied()
        .filter(|p| p.len() >= 2)
        .collect();

    if valid_parts.is_empty() {
        return score_dirs_by_frecency(&working_dirs, context);
    }

    let has_uppercase = valid_parts
        .iter()
        .any(|p| p.chars().any(|c| c.is_uppercase()));

    let options = neo_frizbee::Config {
        max_typos: Some(context.max_typos),
        sort: neo_frizbee::SortStrategy::Unsorted,
        scoring: Scoring {
            capitalization_bonus: if has_uppercase { 8 } else { 0 },
            matching_case_bonus: if has_uppercase { 4 } else { 0 },
            ..Default::default()
        },
        ..Default::default()
    };

    let path_matches = match_fuzzy_parts_dirs(
        fuzzy_parts,
        &working_dirs,
        &options,
        context.max_threads,
        arena,
        overflow_arena,
    );

    let main_needle = valid_parts[0].as_bytes();
    let main_needle_len = main_needle.len() as u16;

    let mut dir_buf = String::with_capacity(64);
    let mut dirname_buf = String::with_capacity(32);
    let distance = context.current_file.map(DirectoryDistance::new);

    let results: Vec<(&DirItem, Score)> = path_matches
        .into_iter()
        .map(|path_match| {
            let dir = working_dirs[path_match.index as usize];
            let dir_arena = if dir.is_overflow() {
                overflow_arena
            } else {
                arena
            };
            let base_score = path_match.score as i32;
            let frecency_boost = base_score.saturating_mul(dir.max_access_frecency()) / 100;

            // Distance penalty from current file's directory.
            let distance_penalty = if let Some(distance) = &distance {
                dir.path.write_to_string(dir_arena, &mut dir_buf);
                distance.penalty(&dir_buf)
            } else {
                0
            };

            // Dirname bonus: if the match is in the last path segment.
            let last_seg_offset = dir.last_segment_offset();
            let match_start_approx = path_match.end_col.saturating_sub(main_needle_len - 1);
            let is_dirname_match = match_start_approx >= last_seg_offset;

            dir.write_dir_name(dir_arena, &mut dirname_buf);
            let dirname_len = dirname_buf.len();
            let is_exact_dirname = is_dirname_match
                && main_needle_len as usize == dirname_len
                && main_needle.eq_ignore_ascii_case(dirname_buf.as_bytes());

            let filename_bonus = if is_exact_dirname {
                base_score / 5 * 2 // 40% bonus for exact dirname match
            } else if is_dirname_match {
                base_score / 6 // ~16% bonus for fuzzy dirname match
            } else {
                0
            };

            let total = base_score
                .saturating_add(frecency_boost)
                .saturating_add(distance_penalty)
                .saturating_add(filename_bonus);

            let score = Score {
                total,
                base_score,
                filename_bonus,
                special_filename_bonus: 0,
                frecency_boost,
                git_status_boost: 0,
                git_recency_boost: 0,
                distance_penalty,
                current_file_penalty: 0,
                combo_match_boost: 0,
                path_alignment_bonus: 0,
                exact_match: is_exact_dirname || path_match.exact,
                match_type: if is_exact_dirname {
                    "exact_dirname"
                } else if is_dirname_match {
                    "fuzzy_dirname"
                } else if path_match.exact {
                    "exact_path"
                } else {
                    "fuzzy_path"
                },
            };

            (dir, score)
        })
        .collect();

    sort_and_paginate_dirs(results, context)
}

fn score_dirs_by_frecency<'a>(
    dirs: &[&'a DirItem],
    context: &ScoringContext,
) -> (Vec<&'a DirItem>, Vec<Score>, usize) {
    let results: Vec<(&DirItem, Score)> = dirs
        .iter()
        .map(|&dir| {
            let score = Score {
                total: dir.max_access_frecency(),
                frecency_boost: dir.max_access_frecency(),
                match_type: "frecency",
                ..Default::default()
            };

            (dir, score)
        })
        .collect();

    sort_and_paginate_dirs(results, context)
}

/// Sort dir results by total score (descending) and apply pagination.
fn sort_and_paginate_dirs<'a>(
    mut results: Vec<(&'a DirItem, Score)>,
    context: &ScoringContext,
) -> (Vec<&'a DirItem>, Vec<Score>, usize) {
    let total_matched = results.len();
    if total_matched == 0 {
        return (vec![], vec![], 0);
    }

    let offset = context.pagination.offset;
    let limit = if context.pagination.limit > 0 {
        context.pagination.limit
    } else {
        total_matched
    };

    if offset >= total_matched {
        return (vec![], vec![], total_matched);
    }

    let items_needed = offset.saturating_add(limit).min(total_matched);
    let use_partial_sort = items_needed < total_matched / 2 && total_matched > 100;

    if use_partial_sort {
        results.select_nth_unstable_by(items_needed - 1, |a, b| b.1.total.cmp(&a.1.total));
        results.truncate(items_needed);
    }

    sort_with_buffer(&mut results, |a, b| b.1.total.cmp(&a.1.total));

    let (items, scores): (Vec<&DirItem>, Vec<Score>) =
        results.into_iter().skip(offset).take(limit).unzip();
    (items, scores, total_matched)
}

fn match_and_score_in_arena<'a>(
    files: &'a [FileItem],
    context: &ScoringContext,
    arena: ArenaPtr,
) -> Vec<(&'a FileItem, Score)> {
    if context.current_file.is_some() {
        match_and_score_in_arena_inner::<true>(files, context, arena)
    } else {
        match_and_score_in_arena_inner::<false>(files, context, arena)
    }
}

fn match_and_score_in_arena_inner<'a, const WITH_CURRENT_FILE: bool>(
    files: &'a [FileItem],
    context: &ScoringContext,
    arena: ArenaPtr,
) -> Vec<(&'a FileItem, Score)> {
    if files.is_empty() {
        return vec![];
    }

    let Some(working_files) = filter_by_constraints(files, context, arena) else {
        return vec![];
    };
    let Some(fuzzy_parts) = fuzzy_parts(context.query) else {
        // Frecency-only queries are ranked by `rank_by_frecency` before reaching here.
        return frecency_scores(&working_files, context, arena);
    };
    let has_uppercase = fuzzy_parts
        .iter()
        .any(|p| p.chars().any(|c| c.is_uppercase()));
    // Users type `/` regardless of platform. Checking the OS separator alone
    // would miss forward-slash queries on Windows.
    let query_contains_path_separator = fuzzy_parts
        .iter()
        .any(|p| p.contains('/') || p.contains(MAIN_SEPARATOR));

    let options = neo_frizbee::Config {
        max_typos: Some(context.max_typos),
        sort: neo_frizbee::SortStrategy::Unsorted,
        scoring: Scoring {
            capitalization_bonus: if has_uppercase { 8 } else { 0 },
            matching_case_bonus: if has_uppercase { 4 } else { 0 },
            ..Default::default()
        },
        ..Default::default()
    };

    let path_matches = match_fuzzy_parts(
        fuzzy_parts,
        &working_files,
        &options,
        context.max_threads,
        arena,
    );

    let main_needle = fuzzy_parts[0].as_bytes(); // safe
    let main_needle_len = main_needle.len() as u16;

    let mut fallback_indices: Vec<u32> = Vec::new();
    let filename_fallback_matches = if query_contains_path_separator || path_matches.len() > 15_000
    {
        vec![]
    } else {
        let mut fallback_filenames: Vec<Cow<'_, str>> = Vec::new();

        for (i, path_match) in path_matches.iter().enumerate() {
            let file = working_files.index(path_match.index as usize);
            let filename_start = file.filename_offset_in_relative_path() as u16;
            let match_start_approx = path_match.end_col.saturating_sub(main_needle_len - 1);

            if match_start_approx < filename_start {
                fallback_indices.push(i as u32);
                fallback_filenames.push(file.path.filename_cow(arena));
            }
        }

        if fallback_filenames.is_empty() {
            vec![]
        } else {
            // Match on `&str` so frizbee reuses the instantiation its index
            // resolver already emits instead of a separate `Cow<str>` copy.
            let filename_strs: Vec<&str> = fallback_filenames.iter().map(Cow::as_ref).collect();
            neo_frizbee::Matcher::new(fuzzy_parts[0], &options).match_list_parallel(
                &filename_strs,
                if path_matches.len() > 4096 {
                    context.max_threads.div_ceil(2048)
                } else {
                    1
                },
            )
        }
    };

    // path-match index -> position in filename_fallback_matches (u32::MAX = none)
    let mut fallback_by_match: Vec<u32> = Vec::new();
    if !filename_fallback_matches.is_empty() {
        fallback_by_match.resize(path_matches.len(), u32::MAX);
        for (position, m) in filename_fallback_matches.iter().enumerate() {
            fallback_by_match[fallback_indices[m.index as usize] as usize] = position as u32;
        }
    }

    let distance = context
        .current_file
        .filter(|_| WITH_CURRENT_FILE)
        .map(DirectoryDistance::new);
    let combo_path = context
        .last_same_query_match
        .as_ref()
        .map(|entry| entry.file_path.to_string_lossy());

    let score_match =
        |bufs: &mut ScoreBuffers, (match_idx, path_match): (usize, &neo_frizbee::Match)| {
            let ScoreBuffers {
                dir_buf,
                fname_buf,
                path_buf,
                last_dir_penalty,
            } = bufs;
            let file_idx = path_match.index as usize;
            let file = working_files.index(file_idx);

            let base_score = path_match.score as i32;
            let frecency_boost = base_score.saturating_mul(file.total_frecency_score()) / 100;

            let git_status_boost = if file.git_status.is_some_and(is_modified_status) {
                base_score * 15 / 100
            } else {
                0
            };
            let git_recency_boost = file.git_recency_score as i32;

            let distance_penalty = if WITH_CURRENT_FILE && let Some(distance) = &distance {
                if let Some((parent, penalty)) = *last_dir_penalty
                    && parent == file.parent_dir_index
                    && parent != u32::MAX
                {
                    penalty
                } else {
                    file.write_dir_str(arena, dir_buf);
                    let penalty = distance.penalty(dir_buf);
                    *last_dir_penalty = Some((file.parent_dir_index, penalty));
                    penalty
                }
            } else {
                0
            };

            let filename_start = file.filename_offset_in_relative_path() as u16;
            let match_start_approx = path_match.end_col.saturating_sub(main_needle_len - 1);

            let end_col_filename_match = match_start_approx >= filename_start;
            let simd_filename_match = if !end_col_filename_match {
                fallback_by_match
                    .get(match_idx)
                    .and_then(|&position| filename_fallback_matches.get(position as usize))
            } else {
                None
            };

            let is_filename_match = end_col_filename_match || simd_filename_match.is_some();
            let fname_len = file.path.byte_len as usize - file.path.filename_offset as usize;

            let is_exact_filename = simd_filename_match.is_some_and(|m| m.exact)
                || (end_col_filename_match && main_needle_len as usize == fname_len && {
                    file.write_file_name_from_arena(arena, fname_buf);
                    main_needle.eq_ignore_ascii_case(fname_buf.as_bytes())
                });

            let mut has_special_filename_bonus = false;
            let filename_bonus = if is_exact_filename {
                base_score / 5 * 2 // 40% bonus for exact filename match
            } else if is_filename_match {
                // 16% bonus for fuzzy filename match that landed in the filename region.
                // For fallback matches (where the path match landed in a directory segment),
                // scale the bonus by the quality of the filename match — a contiguous match
                // like "rename" in "rename.ts" gets the full bonus, while a scattered
                // subsequence like r-e-n-a-m-e in "generateSessionName.ts" gets much less.
                let max_bonus = (base_score / 6).min(30);
                if let Some(fm) = simd_filename_match {
                    let max_possible = main_needle_len as i32 * 16;
                    let quality = (fm.score as i32).min(max_possible);
                    max_bonus * quality / max_possible
                } else {
                    max_bonus
                }
            } else if !is_filename_match && (5..=11).contains(&fname_len) {
                file.write_file_name_from_arena(arena, fname_buf);
                // 5% bonus for special file but not as much as file name to avoid situations
                // when you have /user_service/server.rs and /user_service/server/mod.rs
                if is_special_entry_point_file(fname_buf) {
                    has_special_filename_bonus = true;
                    base_score * 5 / 100
                } else {
                    0
                }
            } else {
                0
            };

            let current_file_penalty = if WITH_CURRENT_FILE {
                calculate_current_file_penalty(file, base_score / 4, context, arena)
            } else {
                0
            };
            let combo_match_boost = {
                let last_same_query_match = context.last_same_query_match.as_ref().filter(|_| {
                    combo_path
                        .as_ref()
                        .and_then(|path| {
                            path.len()
                                .checked_sub(file.relative_path_len())
                                .and_then(|start| path.get(start..))
                        })
                        .is_some_and(|suffix| file.relative_path_eq(arena, suffix))
                });

                match last_same_query_match {
                    Some(_) if context.min_combo_count == 0 => 1000,
                    Some(combo_match) if combo_match.open_count >= context.min_combo_count => {
                        combo_match.open_count as i32 * context.combo_boost_score_multiplier
                    }
                    Some(combo_match) => combo_match.open_count as i32 * 5,
                    _ => 0,
                }
            };

            // Path alignment bonus: when the query looks like a file path,
            // reward candidates whose path closely matches the typed query.
            // Uses suffix overlap — bytes matching from the end. A full prefix
            // match is just the 100% coverage case, so no separate branch needed.
            let path_alignment_bonus = if query_contains_path_separator {
                let rel_path = file.path.read_to_buf(arena, path_buf);
                let path_bytes = rel_path.as_bytes();
                let common_suffix = main_needle
                    .iter()
                    .rev()
                    .zip(path_bytes.iter().rev())
                    .take_while(|(n, p): &(&u8, &u8)| n.eq_ignore_ascii_case(p))
                    .count();

                let needle_len = main_needle.len();
                if common_suffix > 10 && needle_len > 0 {
                    let coverage = common_suffix * 100 / needle_len;
                    if coverage >= 30 {
                        base_score * coverage as i32 / 100
                    } else {
                        0
                    }
                } else {
                    0
                }
            } else {
                0
            };

            let total = base_score
                .saturating_add(frecency_boost)
                .saturating_add(git_status_boost)
                .saturating_add(git_recency_boost)
                .saturating_add(distance_penalty)
                .saturating_add(filename_bonus)
                .saturating_add(current_file_penalty)
                .saturating_add(combo_match_boost)
                .saturating_add(path_alignment_bonus);

            let score = Score {
                total,
                base_score,
                current_file_penalty,
                filename_bonus,
                special_filename_bonus: if has_special_filename_bonus {
                    filename_bonus
                } else {
                    0
                },
                frecency_boost,
                git_status_boost,
                git_recency_boost,
                distance_penalty,
                combo_match_boost,
                path_alignment_bonus,
                exact_match: is_exact_filename || path_match.exact,
                match_type: if is_exact_filename {
                    "exact_filename"
                } else if is_filename_match {
                    "fuzzy_filename"
                } else if path_match.exact {
                    "exact_path"
                } else {
                    "fuzzy_path"
                },
            };

            (file, score)
        };

    // Scoring is a pure per-match function; only the scratch buffers are per-thread.
    let results: Vec<_> =
        if path_matches.len() >= PARALLEL_SCORING_MIN_MATCHES && context.max_threads > 1 {
            path_matches
                .par_iter()
                .enumerate()
                .with_min_len((path_matches.len() / context.max_threads).max(4096))
                .map_init(ScoreBuffers::new, score_match)
                .collect()
        } else {
            let mut bufs = ScoreBuffers::new();
            path_matches
                .iter()
                .enumerate()
                .map(|item| score_match(&mut bufs, item))
                .collect()
        };

    results
}

const PARALLEL_SCORING_MIN_MATCHES: usize = 8192;

struct ScoreBuffers {
    dir_buf: String,
    fname_buf: String,
    path_buf: [u8; crate::simd_path::PATH_BUF_SIZE],
    last_dir_penalty: Option<(u32, i32)>,
}

impl ScoreBuffers {
    fn new() -> Self {
        Self {
            dir_buf: String::with_capacity(64),
            fname_buf: String::with_capacity(32),
            path_buf: [0u8; crate::simd_path::PATH_BUF_SIZE],
            last_dir_penalty: None,
        }
    }
}

fn is_special_entry_point_file(filename: &str) -> bool {
    matches!(
        filename,
        "mod.rs"
            | "lib.rs"
            | "main.rs"
            | "index.js"
            | "index.jsx"
            | "index.ts"
            | "index.tsx"
            | "index.mjs"
            | "index.cjs"
            | "index.vue"
            | "__init__.py"
            | "__main__.py"
            | "main.go"
            | "main.c"
            | "index.php"
            | "main.rb"
            | "index.rb"
    )
}

fn frecency_total(file: &FileItem, context: &ScoringContext, arena: ArenaPtr) -> i32 {
    frecency_score(file, context, arena).total
}

fn frecency_score(file: &FileItem, context: &ScoringContext, arena: ArenaPtr) -> Score {
    let frecency_boost = file.access_frecency_score as i32
        + (file.modification_frecency_score as i32).saturating_mul(4);

    // Give modified/dirty files a boost even in frecency-only mode
    let git_status_boost = if file.git_status.is_some_and(is_modified_status) {
        frecency_boost * 15 / 100
    } else {
        0
    };
    let git_recency_boost = file.git_recency_score as i32;
    let current_file_penalty = calculate_current_file_penalty(file, frecency_boost, context, arena);
    let total = frecency_boost
        .saturating_add(git_status_boost)
        .saturating_add(git_recency_boost)
        .saturating_add(current_file_penalty);

    Score {
        total,
        base_score: 0,
        filename_bonus: 0,
        distance_penalty: 0,
        special_filename_bonus: 0,
        combo_match_boost: 0,
        path_alignment_bonus: 0,
        current_file_penalty,
        frecency_boost,
        git_status_boost,
        git_recency_boost,
        exact_match: false,
        match_type: "frecency",
    }
}

fn frecency_totals<'a>(
    files: &FileItems<'a>,
    context: &ScoringContext,
    arena: ArenaPtr,
    out: &mut Vec<(&'a FileItem, i32)>,
) {
    let total = |file: &'a FileItem| (file, frecency_total(file, context, arena));
    match files {
        // Small indexes cannot amortize Rayon scheduling for this cheap scoring pass.
        FileItems::All(s) if s.len() >= 32_768 && context.max_threads > 1 => out.par_extend(
            s.par_iter()
                .with_min_len(4096)
                .filter(|f| !f.is_deleted())
                .map(total),
        ),
        FileItems::All(s) => out.extend(s.iter().filter(|f| !f.is_deleted()).map(total)),
        FileItems::Filtered(v) => {
            out.extend(v.iter().copied().filter(|f| !f.is_deleted()).map(total))
        }
    }
}

fn frecency_scores<'a>(
    files: &FileItems<'a>,
    context: &ScoringContext,
    arena: ArenaPtr,
) -> Vec<(&'a FileItem, Score)> {
    let mut totals = Vec::new();
    frecency_totals(files, context, arena, &mut totals);
    totals
        .into_iter()
        .map(|(file, _)| (file, frecency_score(file, context, arena)))
        .collect()
}

#[inline]
fn calculate_current_file_penalty(
    file: &FileItem,
    base_score: i32,
    context: &ScoringContext,
    arena: ArenaPtr,
) -> i32 {
    let mut penalty = 0i32;

    if let Some(current) = context.current_file
        && file.relative_path_eq(arena, current)
    {
        penalty -= base_score;
    }

    penalty
}

/// Sorts elements by total score (descending) and returns the requested page.
/// Always returns results in descending order (best scores first).
/// The UI layer handles rendering order based on prompt position.
#[tracing::instrument(skip_all, level = tracing::Level::DEBUG)]
fn sort_and_paginate<'a, S>(
    mut results: Vec<(&'a FileItem, S)>,
    context: &ScoringContext,
    total: impl Fn(&S) -> i32,
) -> (Vec<&'a FileItem>, Vec<S>, usize) {
    let total_matched = results.len();

    if total_matched == 0 {
        return (vec![], vec![], 0);
    }

    let offset = context.pagination.offset;
    let limit = if context.pagination.limit > 0 {
        context.pagination.limit
    } else {
        total_matched
    };

    // Check if offset is out of bounds
    if offset >= total_matched {
        tracing::warn!(
            offset = offset,
            total_matched = total_matched,
            "Pagination: offset >= total_matched, returning empty"
        );

        return (vec![], vec![], total_matched);
    }

    let items_needed = offset.saturating_add(limit).min(total_matched);
    let compare = |a: &(&FileItem, S), b: &(&FileItem, S)| {
        total(&b.1)
            .cmp(&total(&a.1))
            .then_with(|| b.0.modified.cmp(&a.0.modified))
    };
    // Use partial sort if we need less than half the results and dataset is large
    if items_needed < total_matched / 2 && total_matched > 100 {
        results.select_nth_unstable_by(items_needed - 1, compare);
        results.truncate(items_needed);
    }

    // select nth does not sort the results, we have to sort accordingly anyway
    sort_with_buffer(&mut results, compare);

    let (items, scores): (Vec<&FileItem>, Vec<S>) =
        results.into_iter().skip(offset).take(limit).unzip();
    (items, scores, total_matched)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::PaginationArgs;
    use fff_query_parser::QueryParser;

    #[test]
    fn frecency_parallel_and_sequential_scores_match() {
        let files: Vec<_> = (0..33_000)
            .map(|index| {
                let mut file =
                    FileItem::new_raw(0, 0, index, Some(git2::Status::WT_MODIFIED), false);
                file.access_frecency_score = (index % 97) as i16;
                file.modification_frecency_score = (index % 29) as i16;
                file.git_recency_score = (index % 13) as i16;
                file.set_deleted(index % 7 == 0);
                file
            })
            .collect();
        let parser = QueryParser::default();
        let query = parser.parse("");
        let mut context = ScoringContext {
            query: &query,
            max_threads: 4,
            max_typos: 0,
            project_path: None,
            current_file: None,
            last_same_query_match: None,
            combo_boost_score_multiplier: 0,
            min_combo_count: 0,
            pagination: PaginationArgs {
                offset: 0,
                limit: 50,
            },
        };
        for count in [0, 1, 1000, 33_000] {
            let items = FileItems::All(&files[..count]);
            context.max_threads = 4;
            let parallel = frecency_scores(&items, &context, ArenaPtr::null());
            context.max_threads = 1;
            let sequential = frecency_scores(&items, &context, ArenaPtr::null());
            assert_eq!(parallel.len(), count - count.div_ceil(7));
            assert_eq!(parallel.len(), sequential.len());
            for ((a, a_score), (b, b_score)) in parallel.iter().zip(&sequential) {
                assert!(std::ptr::eq(*a, *b));
                assert!(!a.is_deleted());
                assert_eq!(a_score.total, b_score.total);
                assert_eq!(a_score.frecency_boost, b_score.frecency_boost);
                assert_eq!(a_score.git_status_boost, b_score.git_status_boost);
                assert_eq!(a_score.git_recency_boost, b_score.git_recency_boost);
            }

            // Keyed ranking must produce the same page as ranking full scores.
            context.max_threads = 4;
            let (expected_items, expected_scores, expected_total) =
                sort_and_paginate(parallel, &context, |score| score.total);
            let (items, scores, total) = rank_by_frecency(
                &files[..count],
                &context,
                count / 2,
                ArenaPtr::null(),
                ArenaPtr::null(),
            );
            assert_eq!(total, expected_total);
            assert_eq!(items.len(), expected_items.len());
            for ((a, a_score), (b, b_score)) in items
                .iter()
                .zip(&scores)
                .zip(expected_items.iter().zip(&expected_scores))
            {
                assert!(std::ptr::eq(*a, *b));
                assert_eq!(a_score.total, b_score.total);
                assert_eq!(a_score.match_type, "frecency");
            }
        }
    }

    #[test]
    fn pagination_matches_full_ranking() {
        let files: Vec<_> = (0..257)
            .map(|i| FileItem::new_raw(0, 0, i as u64, None, false))
            .collect();
        let dirs: Vec<_> = (0..257)
            .map(|_| DirItem::new(crate::simd_path::ChunkedString::empty(), 0))
            .collect();
        let parser = QueryParser::default();
        let query = parser.parse("");
        for count in [0, 1, 3, 100, 101, 257] {
            let results: Vec<_> = files[..count]
                .iter()
                .enumerate()
                .map(|(index, file)| {
                    (
                        file,
                        Score {
                            total: (index * 73 % 257) as i32,
                            ..Default::default()
                        },
                    )
                })
                .collect();
            let mut expected = results.clone();
            expected.sort_by(|a, b| {
                b.1.total
                    .cmp(&a.1.total)
                    .then_with(|| b.0.modified.cmp(&a.0.modified))
            });
            for offset in [0, 1, count / 2, count, usize::MAX] {
                for limit in [0, 1, 2, 50, count, usize::MAX] {
                    let context = ScoringContext {
                        query: &query,
                        max_threads: 1,
                        max_typos: 0,
                        project_path: None,
                        current_file: None,
                        last_same_query_match: None,
                        combo_boost_score_multiplier: 0,
                        min_combo_count: 0,
                        pagination: PaginationArgs { offset, limit },
                    };
                    let expected_page: Vec<_> = expected
                        .iter()
                        .skip(offset)
                        .take(if limit == 0 { count } else { limit })
                        .map(|(_, score)| score.total)
                        .collect();
                    let (items, scores, total) =
                        sort_and_paginate(results.clone(), &context, |s| s.total);
                    assert_eq!(total, count);
                    assert_eq!(items.len(), expected_page.len());
                    assert_eq!(
                        scores.iter().map(|score| score.total).collect::<Vec<_>>(),
                        expected_page
                    );
                    let dir_results = results
                        .iter()
                        .enumerate()
                        .map(|(index, (_, score))| (&dirs[index], score.clone()))
                        .collect();
                    let (items, scores, total) = sort_and_paginate_dirs(dir_results, &context);
                    assert_eq!(total, count);
                    assert_eq!(items.len(), expected_page.len());
                    assert_eq!(
                        scores.iter().map(|score| score.total).collect::<Vec<_>>(),
                        expected_page
                    );
                }
            }
        }
    }

    #[test]
    fn merge_offsets_reuses_storage() {
        let mut ranges: SmallVec<[(u32, u32); 4]> = smallvec::smallvec![
            (20, 25),
            (2, 6),
            (0, 3),
            (6, 10),
            (11, 11),
            (24, 30),
            (40, 45),
        ];
        let storage = ranges.as_mut_ptr();
        let merged = merge_byte_offsets(ranges);
        assert_eq!(merged.as_slice(), &[(0, 10), (20, 30), (40, 45)]);
        assert_eq!(merged.as_ptr(), storage);
    }

    fn make_test_files(specs: &[(&str, i32, u64)]) -> (Vec<(FileItem, Score)>, ArenaPtr) {
        let path_strings: Vec<String> = specs.iter().map(|(p, _, _)| p.to_string()).collect();
        let items: Vec<FileItem> = specs
            .iter()
            .map(|(p, _, _)| {
                let fname = p.rfind(std::path::is_separator).map(|i| i + 1).unwrap_or(0) as u16;
                FileItem::new_raw(fname, 0, 0, None, false)
            })
            .collect();
        let (store, strings) =
            crate::simd_path::build_chunked_path_store_from_strings(&path_strings, &items);
        let arena = store.as_arena_ptr();
        let result: Vec<(FileItem, Score)> = specs
            .iter()
            .enumerate()
            .map(|(i, &(_, score, modified))| {
                let filename_start = path_strings[i]
                    .rfind(std::path::is_separator)
                    .map(|j| j + 1)
                    .unwrap_or(0) as u16;
                let mut file = FileItem::new_raw(filename_start, 0, modified, None, false);
                file.set_path(strings[i].clone());
                let score_obj = Score {
                    total: score,
                    base_score: score,
                    filename_bonus: 0,
                    distance_penalty: 0,
                    special_filename_bonus: 0,
                    current_file_penalty: 0,
                    frecency_boost: 0,
                    git_status_boost: 0,
                    git_recency_boost: 0,
                    exact_match: false,
                    match_type: "test",
                    combo_match_boost: 0,
                    path_alignment_bonus: 0,
                };
                (file, score_obj)
            })
            .collect();
        std::mem::forget(store);
        (result, arena)
    }

    #[test]
    fn test_partial_sort_descending() {
        // Create test data with known scores
        let (test_data, arena) = make_test_files(&[
            ("file1.rs", 100, 1000),
            ("file2.rs", 200, 2000),
            ("file3.rs", 50, 3000),
            ("file4.rs", 300, 4000),
            ("file5.rs", 150, 5000),
            ("file6.rs", 250, 6000),
            ("file7.rs", 80, 7000),
            ("file8.rs", 180, 8000),
            ("file9.rs", 120, 9000),
            ("file10.rs", 90, 10000),
        ]);

        // Convert to references like the actual function uses
        let results: Vec<(&FileItem, Score)> = test_data
            .iter()
            .map(|(file, score)| (file, score.clone()))
            .collect();

        let query_str = "test";
        let parser = QueryParser::default();
        let query = parser.parse(query_str);
        let context = ScoringContext {
            query: &query,
            max_threads: 1,
            max_typos: 2,
            current_file: None,
            last_same_query_match: None,
            project_path: None,
            combo_boost_score_multiplier: 100,
            min_combo_count: 3,

            pagination: PaginationArgs {
                offset: 0,
                limit: 0,
            },
        };

        // Test with full sort - returns all results sorted descending
        let (items, scores, total) = sort_and_paginate(results.clone(), &context, |s| s.total);

        // Should return all 10 items sorted by score descending
        assert_eq!(total, 10);
        assert_eq!(scores.len(), 10);
        assert_eq!(scores[0].total, 300, "First should be highest score");
        assert_eq!(scores[1].total, 250, "Second should be second highest");
        assert_eq!(scores[2].total, 200, "Third should be third highest");

        // Verify the files match
        assert_eq!(items[0].relative_path(arena), "file4.rs");
        assert_eq!(items[1].relative_path(arena), "file6.rs");
        assert_eq!(items[2].relative_path(arena), "file2.rs");
    }

    #[test]
    fn test_partial_sort_with_same_scores() {
        // Test tiebreaker with modified time
        let (test_data, _arena) = make_test_files(&[
            ("file1.rs", 100, 5000), // Same score, older
            ("file2.rs", 100, 8000), // Same score, newer
            ("file3.rs", 100, 3000), // Same score, oldest
            ("file4.rs", 200, 1000),
            ("file5.rs", 200, 9000), // Higher score, newest
        ]);

        let results: Vec<(&FileItem, Score)> = test_data
            .iter()
            .map(|(file, score)| (file, score.clone()))
            .collect();

        let query_str = "test";
        let parser = QueryParser::default();
        let query = parser.parse(query_str);
        let context = ScoringContext {
            query: &query,
            max_threads: 1,
            max_typos: 2,
            current_file: None,
            last_same_query_match: None,
            project_path: None,
            combo_boost_score_multiplier: 100,
            min_combo_count: 3,

            pagination: PaginationArgs {
                offset: 0,
                limit: 0,
            },
        };

        let (items, scores, _) = sort_and_paginate(results, &context, |s| s.total);

        // Should return all 5 items sorted: 200(9000), 200(1000), 100(8000), 100(5000), 100(3000)
        assert_eq!(scores.len(), 5);
        assert_eq!(scores[0].total, 200);
        assert_eq!(items[0].modified, 9000, "First 200 should be newest");
        assert_eq!(scores[1].total, 200);
        assert_eq!(items[1].modified, 1000, "Second 200 should be older");
        assert_eq!(scores[2].total, 100);
        assert_eq!(items[2].modified, 8000, "First 100 should be newest");
        assert_eq!(scores[3].total, 100);
        assert_eq!(items[3].modified, 5000);
        assert_eq!(scores[4].total, 100);
        assert_eq!(items[4].modified, 3000, "Last 100 should be oldest");
    }

    #[test]
    fn test_no_partial_sort_for_small_results() {
        // When results.len() <= threshold, should use regular sort
        let (test_data, arena) = make_test_files(&[
            ("file1.rs", 100, 1000),
            ("file2.rs", 200, 2000),
            ("file3.rs", 50, 3000),
        ]);

        let results: Vec<(&FileItem, Score)> = test_data
            .iter()
            .map(|(file, score)| (file, score.clone()))
            .collect();

        let query_str = "test";
        let parser = QueryParser::default();
        let query = parser.parse(query_str);
        let context = ScoringContext {
            query: &query,
            max_threads: 1,
            max_typos: 2,
            current_file: None,
            last_same_query_match: None,
            project_path: None,
            combo_boost_score_multiplier: 100,
            min_combo_count: 3,

            pagination: PaginationArgs {
                offset: 0,
                limit: 0,
            },
        };

        // Returns all results sorted descending
        let (items, scores, _) = sort_and_paginate(results, &context, |s| s.total);

        assert_eq!(scores.len(), 3);
        assert_eq!(scores[0].total, 200);
        assert_eq!(scores[1].total, 100);
        assert_eq!(scores[2].total, 50);
        assert_eq!(items[0].relative_path(arena), "file2.rs");
        assert_eq!(items[1].relative_path(arena), "file1.rs");
        assert_eq!(items[2].relative_path(arena), "file3.rs");
    }
}

#[cfg(test)]
mod filename_bonus_tests {
    use super::*;
    use crate::types::PaginationArgs;
    use fff_query_parser::QueryParser;

    #[test]
    fn page_highlights_use_each_files_arena() {
        let base_path = "src/ui_controller.rs";
        let overflow_path = "other/test_controller.rs";
        let (base, base_arena) = make_files(&[base_path]);
        let (overflow, overflow_arena) = make_files(&[overflow_path]);
        overflow[0].set_overflow(true);
        let parser = QueryParser::default();
        let query = parser.parse("controller");
        let ranges = fuzzy_match_byte_offsets_for_page(
            &query,
            &[&base[0], &overflow[0]],
            0,
            base_arena,
            overflow_arena,
        );
        for (path, ranges) in [base_path, overflow_path].into_iter().zip(ranges) {
            let start = path.find("controller").unwrap() as u32;
            assert_eq!(ranges.as_slice(), &[(start, start + 10)]);
        }
    }

    #[test]
    fn distance_scoring_handles_repeated_and_unknown_parents() {
        let paths = [
            "src/widgets/file_a.rs",
            "src/widgets/file_b.rs",
            "src/file_c.rs",
            "tests/file_d.rs",
        ];
        let (mut files, arena) = make_files(&paths);
        files[0].parent_dir_index = 0;
        files[1].parent_dir_index = 0;
        let parser = QueryParser::default();
        let query = parser.parse("file");
        let current_file = Some("src/widgets/current.rs");
        let context = ScoringContext {
            query: &query,
            current_file,
            max_threads: 1,
            max_typos: 0,
            project_path: None,
            last_same_query_match: None,
            combo_boost_score_multiplier: 0,
            min_combo_count: 0,
            pagination: PaginationArgs {
                offset: 0,
                limit: 50,
            },
        };
        let results = match_and_score_in_arena(&files, &context, arena);
        assert_eq!(results.len(), files.len());
        for (file, score) in results {
            assert_eq!(
                score.distance_penalty,
                crate::path_utils::calculate_distance_penalty(current_file, &file.dir_str(arena))
            );
        }
    }

    fn make_files(paths: &[&str]) -> (Vec<FileItem>, ArenaPtr) {
        let path_strings: Vec<String> = paths.iter().map(|p| p.to_string()).collect();
        let items: Vec<FileItem> = paths
            .iter()
            .map(|p| {
                let fname = p.rfind(std::path::is_separator).map(|i| i + 1).unwrap_or(0) as u16;
                FileItem::new_raw(fname, 0, 0, None, false)
            })
            .collect();
        let (store, strings) =
            crate::simd_path::build_chunked_path_store_from_strings(&path_strings, &items);
        let arena = store.as_arena_ptr();
        let mut result: Vec<FileItem> = items;
        for (i, file) in result.iter_mut().enumerate() {
            file.set_path(strings[i].clone());
        }
        std::mem::forget(store);
        (result, arena)
    }

    fn make_files_with_frecency(specs: &[(&str, i16)]) -> (Vec<FileItem>, ArenaPtr) {
        let path_strings: Vec<String> = specs.iter().map(|(p, _)| p.to_string()).collect();
        let items: Vec<FileItem> = specs
            .iter()
            .map(|(p, _)| {
                let fname = p.rfind(std::path::is_separator).map(|i| i + 1).unwrap_or(0) as u16;
                FileItem::new_raw(fname, 0, 0, None, false)
            })
            .collect();
        let (store, strings) =
            crate::simd_path::build_chunked_path_store_from_strings(&path_strings, &items);
        let arena = store.as_arena_ptr();
        let mut result: Vec<FileItem> = items;
        for (i, file) in result.iter_mut().enumerate() {
            file.set_path(strings[i].clone());
            file.access_frecency_score = specs[i].1;
        }
        std::mem::forget(store);
        (result, arena)
    }

    /// Run `fuzzy_match_and_score_files` with production-like max_typos scaling.
    fn search(files: &[FileItem], query: &str, arena: ArenaPtr) -> Vec<(String, Score)> {
        let parser = QueryParser::default();
        let parsed = parser.parse(query);

        let effective_query = match &parsed.fuzzy_query {
            FuzzyQuery::Text(t) => *t,
            FuzzyQuery::Parts(parts) if !parts.is_empty() => parts[0],
            _ => query,
        };
        let max_typos = (effective_query.len() as u16 / 4).clamp(2, 6);

        let ctx = ScoringContext {
            query: &parsed,
            max_threads: 1,
            max_typos,
            current_file: None,
            last_same_query_match: None,
            project_path: None,
            combo_boost_score_multiplier: 100,
            min_combo_count: 3,

            pagination: PaginationArgs {
                offset: 0,
                limit: 100,
            },
        };
        let (items, scores, _) =
            fuzzy_match_and_score_files(files, &ctx, files.len(), arena, arena);
        items
            .iter()
            .zip(scores.iter())
            .map(|(f, s)| (f.relative_path(arena).to_string(), s.clone()))
            .collect()
    }

    #[test]
    fn test_filename_match_ranks_above_path_only_match() {
        let (files, arena) = make_files(&["src/username/handler.rs", "src/username/username.rs"]);

        let results = search(&files, "usrnmea", arena);

        assert!(
            results.len() >= 2,
            "both files should match, got {}",
            results.len()
        );
        assert_eq!(
            results[0].0, "src/username/username.rs",
            "filename match should rank first"
        );
        assert!(
            results[0].1.filename_bonus > 0,
            "username.rs should have filename bonus"
        );
        assert_eq!(
            results[1].1.filename_bonus, 0,
            "handler.rs should have no filename bonus"
        );
    }

    #[test]
    fn test_exact_filename_beats_fuzzy_filename() {
        let (files, arena) = make_files(&["src/user_name_handler.rs", "src/username.rs"]);

        let results = search(&files, "username.rs", arena);

        assert!(results.len() >= 2);
        assert_eq!(
            results[0].0, "src/username.rs",
            "exact filename should rank first"
        );
        assert_eq!(results[0].1.match_type, "exact_filename");
        assert!(results[0].1.filename_bonus > results[1].1.filename_bonus);
    }

    #[test]
    fn test_same_length_filename_no_false_exact() {
        let (files, arena) = make_files(&["src/item_sync/file.rs", "src/models/item.rs"]);

        let results = search(&files, "item.rs", arena);

        assert!(results.len() >= 2);
        assert_eq!(results[0].0, "src/models/item.rs");
        assert_eq!(results[0].1.match_type, "exact_filename");
        assert_ne!(
            results[1].1.match_type, "exact_filename",
            "file.rs should not get exact_filename"
        );
    }

    #[test]
    fn test_path_separator_disables_filename_bonus() {
        let (files, arena) = make_files(&["src/controllers/user.rs"]);

        let results = search(&files, "src/user", arena);

        assert!(!results.is_empty());
        assert_eq!(
            results[0].1.filename_bonus, 0,
            "path-like query should not get filename bonus"
        );
    }

    /// Regression: full-path query should rank the near-exact path match first.
    /// https://x.com/mbarneyjr/status/2043474268390817861
    #[test]
    fn test_full_path_query_prefers_closer_filename_match() {
        let (files, arena) = make_files_with_frecency(&[
            (
                "test-utils/completion/condition-key/yaml_partial-svc-colon.yml",
                0,
            ),
            (
                "test-utils/test-cases/completion/condition-key/yaml_partial-svc.yml",
                0,
            ),
            (
                "test-utils/action-value/yaml_inline_partial-svc-colon.yml",
                0,
            ),
            (
                "test-utils/completion/action-value/yaml_array_partial-svc-colon.yml",
                0,
            ),
            (
                "test-utils/completion/action-value/yaml_array_partial-svc.yml",
                0,
            ),
            (
                "test-utils/completion/condition-key/yaml_global-tag-keys.yml",
                0,
            ),
            (
                "test-utils/test-cases/completion/condition-key/yaml_partial.yml",
                10,
            ),
        ]);

        let results = search(
            &files,
            "t-utils/test-cases/completion/condition-key/yaml_partial-svc.yml",
            arena,
        );

        assert!(!results.is_empty(), "query should match at least one file");

        assert_eq!(
            results[0].0, "test-utils/test-cases/completion/condition-key/yaml_partial-svc.yml",
            "near-exact full-path match should rank first, but got: {} \
             (total={}, base={}, frecency={})",
            results[0].0, results[0].1.total, results[0].1.base_score, results[0].1.frecency_boost,
        );
    }

    /// Regression: PR #652 / field panic in pi-fff v0.9.6.
    /// A path >512 bytes (but within PATH_MAX) overflows the fixed
    /// `[*const u8; 32]` chunk-pointer buffer during scoring and panics with
    /// "index out of bounds: the len is 32 but the index is 32".
    #[test]
    fn test_path_longer_than_512_bytes_does_not_panic_and_matches() {
        let mut long_path = String::new();
        while long_path.len() < 600 {
            long_path.push_str("deeply_nested_directory_segment/");
        }
        long_path.push_str("needle_file.rs");
        assert!(long_path.len() > 512 && long_path.len() < crate::simd_path::PATH_BUF_SIZE);

        let (files, arena) = make_files(&[long_path.as_str(), "src/other.rs"]);

        // Panics here on unfixed code: frizbee resolves chunk ptrs per file.
        let results = search(&files, "needle", arena);

        assert!(
            results.iter().any(|(p, _)| p == &long_path),
            "filename at the tail of a >512-byte path must still match, got: {:?}",
            results.iter().map(|(p, _)| p).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_single_path_matching() {
        let path = "core_workflow_service/kafka_event_consumer/src/ai_part_extraction_request/ai_part_extraction_request_handler.rs";

        let options = neo_frizbee::Config {
            max_typos: Some(2),
            sort: neo_frizbee::SortStrategy::Unsorted,
            ..Default::default()
        };

        let matches = neo_frizbee::Matcher::new("aipart", &options).match_list(&[path]);
        assert!(!matches.is_empty(), "'aipart' should match the path");

        let matches = neo_frizbee::Matcher::new("core", &options).match_list(&[path]);
        assert!(!matches.is_empty(), "'core' should match the path");

        let co_options = neo_frizbee::Config {
            max_typos: Some(2),
            ..options
        };
        let matches = neo_frizbee::Matcher::new("co", &co_options).match_list(&[path]);
        assert!(!matches.is_empty(), "'co' should match the path");
    }

    #[test]
    fn test_lowercase_path_matching() {
        let path = "core_workflow_service/kafka_event_consumer/src/ai_part_extraction_request/ai_part_extraction_request_handler.rs".to_lowercase();

        let options = neo_frizbee::Config {
            max_typos: Some(2),
            sort: neo_frizbee::SortStrategy::Unsorted,
            ..Default::default()
        };

        let matches = neo_frizbee::Matcher::new("co", &options).match_list(&[path.as_str()]);
        assert!(!matches.is_empty(), "'co' should match the lowercase path");

        let matches = neo_frizbee::Matcher::new("core", &options).match_list(&[path.as_str()]);
        assert!(
            !matches.is_empty(),
            "'core' should match the lowercase path"
        );
    }
}

#[cfg(test)]
mod git_recency_scoring_tests {
    use super::*;
    use crate::types::PaginationArgs;
    use fff_query_parser::QueryParser;

    fn make_files(specs: &[(&str, i16)]) -> (Vec<FileItem>, ArenaPtr) {
        let path_strings: Vec<String> = specs.iter().map(|(p, _)| p.to_string()).collect();
        let items: Vec<FileItem> = specs
            .iter()
            .map(|(p, _)| {
                let fname = p.rfind(std::path::is_separator).map(|i| i + 1).unwrap_or(0) as u16;
                FileItem::new_raw(fname, 0, 0, None, false)
            })
            .collect();
        let (store, strings) =
            crate::simd_path::build_chunked_path_store_from_strings(&path_strings, &items);
        let arena = store.as_arena_ptr();
        let mut result: Vec<FileItem> = items;
        for (i, file) in result.iter_mut().enumerate() {
            file.set_path(strings[i].clone());
            file.git_recency_score = specs[i].1;
        }
        std::mem::forget(store);
        (result, arena)
    }

    fn search(files: &[FileItem], query: &str, arena: ArenaPtr) -> Vec<(String, Score)> {
        let parser = QueryParser::default();
        let parsed = parser.parse(query);
        let ctx = ScoringContext {
            query: &parsed,
            max_threads: 1,
            max_typos: 2,
            current_file: None,
            last_same_query_match: None,
            project_path: None,
            combo_boost_score_multiplier: 100,
            min_combo_count: 3,
            pagination: PaginationArgs {
                offset: 0,
                limit: 100,
            },
        };
        let (items, scores, _) =
            fuzzy_match_and_score_files(files, &ctx, files.len(), arena, arena);
        items
            .iter()
            .zip(scores)
            .map(|(f, s)| (f.relative_path(arena), s))
            .collect()
    }

    #[test]
    fn recency_boost_breaks_ties_between_equal_fuzzy_matches() {
        // Dir names share no letters with the query so both paths fuzzy-match
        // identically and only the recency boost separates them.
        let (files, arena) = make_files(&[("src/xxx/handler.rs", 0), ("src/yyy/handler.rs", 5)]);

        let results = search(&files, "handler", arena);

        assert!(results.len() >= 2);
        assert_eq!(results[0].0, "src/yyy/handler.rs");
        assert_eq!(results[0].1.git_recency_boost, 5);
        assert_eq!(results[1].1.git_recency_boost, 0);
        assert_eq!(
            results[0].1.total - results[1].1.total,
            5,
            "boost is additive: exactly +1 point per participating commit"
        );
    }

    #[test]
    fn recency_boost_ranks_files_in_frecency_only_mode() {
        let (files, arena) = make_files(&[("cold.rs", 0), ("committed.rs", 7)]);

        let results = search(&files, "", arena);

        assert_eq!(results[0].0, "committed.rs");
        assert_eq!(results[0].1.git_recency_boost, 7);
        assert_eq!(results[0].1.total, 7);
        assert_eq!(results[0].1.match_type, "frecency");
    }
}

#[cfg(test)]
mod typo_resistance_tests {
    use super::*;
    use crate::types::PaginationArgs;
    use fff_query_parser::QueryParser;

    fn make_files(paths: &[&str]) -> (Vec<FileItem>, ArenaPtr) {
        let path_strings: Vec<String> = paths.iter().map(|p| p.to_string()).collect();
        let items: Vec<FileItem> = paths
            .iter()
            .map(|p| {
                let fname = p.rfind(std::path::is_separator).map(|i| i + 1).unwrap_or(0) as u16;
                FileItem::new_raw(fname, 0, 0, None, false)
            })
            .collect();
        let (store, strings) =
            crate::simd_path::build_chunked_path_store_from_strings(&path_strings, &items);
        let arena = store.as_arena_ptr();
        let mut result: Vec<FileItem> = items;
        for (i, file) in result.iter_mut().enumerate() {
            file.set_path(strings[i].clone());
        }
        std::mem::forget(store);
        (result, arena)
    }

    fn search_with_typos(
        files: &[FileItem],
        query: &str,
        arena: ArenaPtr,
        max_typos: u16,
    ) -> Vec<String> {
        let parser = QueryParser::default();
        let parsed = parser.parse(query);
        let ctx = ScoringContext {
            query: &parsed,
            max_threads: 1,
            max_typos,
            current_file: None,
            last_same_query_match: None,
            project_path: None,
            combo_boost_score_multiplier: 100,
            min_combo_count: 3,
            pagination: PaginationArgs {
                offset: 0,
                limit: 100,
            },
        };
        let (items, _, _) = fuzzy_match_and_score_files(files, &ctx, files.len(), arena, arena);
        items.iter().map(|f| f.relative_path(arena)).collect()
    }

    #[test]
    fn test_typo_resistant_long_query() {
        let (files, arena) = make_files(&[
            "src/pricing/bid_comparison_supplier_part_cost_modifiers.rs",
            "src/pricing/bid_evaluation_handler.rs",
            "src/pricing/supplier_contract_terms.rs",
            "src/models/purchase_order.rs",
            "src/models/inventory_item.rs",
            "src/controllers/user_controller.rs",
            "src/controllers/admin_controller.rs",
            "src/services/notification_service.rs",
            "src/services/email_dispatcher.rs",
            "src/utils/string_helpers.rs",
            "src/utils/date_formatter.rs",
            "src/db/migration_runner.rs",
            "src/db/connection_pool.rs",
            "src/config/app_settings.rs",
            "src/config/feature_flags.rs",
        ]);

        // Exact substring — must always match
        let results = search_with_typos(&files, "bid_comparison", arena, 6);
        assert!(
            results
                .iter()
                .any(|p| p.contains("bid_comparison_supplier_part_cost_modifiers")),
            "exact substring 'bid_comparison' should match, got: {results:?}"
        );

        // Concatenated query with typos — the real-world scenario.
        // "bidcomparsionsupplierpartcostmodfiers" is a smushed version of
        // "bid_comparison_supplier_part_cost_modifiers" with 2 typos
        // (comparison → comparison, modifiers → modifiers).
        let results = search_with_typos(&files, "bidcomparsionsupplierpartcostmodfiers", arena, 6);
        assert!(
            results
                .iter()
                .any(|p| p.contains("bid_comparison_supplier_part_cost_modifiers")),
            "typo query 'bidcomparsionsupplierpartcostmodfiers' should match, got: {results:?}"
        );

        // Shorter typo query
        let results = search_with_typos(&files, "bidcomp", arena, 4);
        assert!(
            results.iter().any(|p| p.contains("bid_comparison")),
            "'bidcomp' should match bid_comparison file, got: {results:?}"
        );

        // Query with missing underscores
        let results = search_with_typos(&files, "supplierpartcost", arena, 6);
        assert!(
            results
                .iter()
                .any(|p| p.contains("bid_comparison_supplier_part_cost_modifiers")),
            "'supplierpartcost' should match, got: {results:?}"
        );
    }
}

#[cfg(test)]
mod constraint_only_query_tests {
    use super::*;
    use crate::types::PaginationArgs;
    use fff_query_parser::QueryParser;

    #[test]
    fn constraint_only_git_status_query_returns_filtered_files() {
        let paths = ["modified.rs", "clean.rs"];
        let path_strings: Vec<String> = paths.iter().map(|p| p.to_string()).collect();
        let items: Vec<FileItem> = paths
            .iter()
            .map(|p| {
                let fname = p.rfind(std::path::is_separator).map(|i| i + 1).unwrap_or(0) as u16;
                FileItem::new_raw(fname, 0, 0, None, false)
            })
            .collect();
        let (store, strings) =
            crate::simd_path::build_chunked_path_store_from_strings(&path_strings, &items);
        let arena = store.as_arena_ptr();
        let mut files: Vec<FileItem> = items;
        for (i, file) in files.iter_mut().enumerate() {
            file.set_path(strings[i].clone());
        }
        files[0].git_status = Some(git2::Status::WT_MODIFIED);
        files[1].git_status = Some(git2::Status::CURRENT);
        std::mem::forget(store);

        let parser = QueryParser::default();
        let parsed = parser.parse("git:modified");

        let ctx = ScoringContext {
            query: &parsed,
            max_threads: 1,
            max_typos: 2,
            current_file: None,
            last_same_query_match: None,
            project_path: None,
            combo_boost_score_multiplier: 100,
            min_combo_count: 3,
            pagination: PaginationArgs {
                offset: 0,
                limit: 50,
            },
        };

        let (items, _scores, total_matched) =
            fuzzy_match_and_score_files(&files, &ctx, files.len(), arena, arena);

        assert_eq!(
            total_matched, 1,
            "git:modified constraint-only query should match exactly 1 modified file"
        );
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].relative_path(arena), "modified.rs");
    }

    #[test]
    fn constraint_only_status_modified_alias_returns_filtered_files() {
        let paths = ["a.rs", "b.rs"];
        let path_strings: Vec<String> = paths.iter().map(|p| p.to_string()).collect();
        let items: Vec<FileItem> = paths
            .iter()
            .map(|p| {
                let fname = p.rfind(std::path::is_separator).map(|i| i + 1).unwrap_or(0) as u16;
                FileItem::new_raw(fname, 0, 0, None, false)
            })
            .collect();
        let (store, strings) =
            crate::simd_path::build_chunked_path_store_from_strings(&path_strings, &items);
        let arena = store.as_arena_ptr();
        let mut files: Vec<FileItem> = items;
        for (i, file) in files.iter_mut().enumerate() {
            file.set_path(strings[i].clone());
        }
        files[0].git_status = Some(git2::Status::WT_MODIFIED);
        files[1].git_status = Some(git2::Status::CURRENT);
        std::mem::forget(store);

        let parser = QueryParser::default();
        let parsed = parser.parse("status:modified");

        let ctx = ScoringContext {
            query: &parsed,
            max_threads: 1,
            max_typos: 2,
            current_file: None,
            last_same_query_match: None,
            project_path: None,
            combo_boost_score_multiplier: 100,
            min_combo_count: 3,
            pagination: PaginationArgs {
                offset: 0,
                limit: 50,
            },
        };

        let (items, _scores, total_matched) =
            fuzzy_match_and_score_files(&files, &ctx, files.len(), arena, arena);

        assert_eq!(total_matched, 1);
        assert_eq!(items[0].relative_path(arena), "a.rs");
    }
}
