use fff::GrepMatch;
use fff::file_picker::FilePicker;
use fff::git::format_git_status_opt;
use fff::grep::is_import_line;
use fff::types::FileItem;

use crate::cursor::{CursorStore, GrepCursor};

fn frecency_word(score: i32) -> Option<&'static str> {
    if score >= 100 {
        Some("hot")
    } else if score >= 50 {
        Some("warm")
    } else if score >= 10 {
        Some("frequent")
    } else {
        None
    }
}

pub fn file_suffix(git_status: Option<git2::Status>, frecency_score: i32) -> String {
    // `clean` is the default state of nearly every file: annotating it burns
    // context without adding signal. Only exceptional git state is surfaced.
    match (
        frecency_word(frecency_score),
        format_git_status_opt(git_status).filter(|status| *status != "clean"),
    ) {
        (Some(f), Some(g)) => format!(" - {f} git:{g}"),
        (Some(f), None) => format!(" - {f}"),
        (None, Some(g)) => format!(" git:{g}"),
        (None, None) => String::new(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    Content,
    FilesWithMatches,
    Count,
    Usage,
}

impl OutputMode {
    pub fn new(s: Option<&str>) -> Self {
        match s {
            Some("files_with_matches") => Self::FilesWithMatches,
            Some("count") => Self::Count,
            Some("usage") => Self::Usage,
            _ => Self::Content,
        }
    }
}

const LARGE_FILE_BYTES: u64 = 20_000;

fn size_tag(bytes: u64) -> String {
    if bytes < LARGE_FILE_BYTES {
        String::new()
    } else {
        let kb = (bytes + 512) / 1024; // round
        format!(" ({}KB - use offset to read relevant section)", kb)
    }
}

const MAX_PREVIEW: usize = 120;
const MAX_LINE_LEN: usize = 180;
const MAX_DEF_EXPAND_FIRST: usize = 16;
const MAX_DEF_EXPAND: usize = 5;
const MAX_FIRST_MATCH_EXPAND: usize = 8;
const AUTO_CONTEXT_CHARS: usize = 5000;
const EXPLICIT_CONTEXT_CHARS: usize = 8000;

fn trauncate_line_for_ai(
    line: &str,
    match_ranges: Option<&[(u32, u32)]>,
    max_len: usize,
) -> String {
    let trimmed = line.trim_end();
    if trimmed.is_empty() {
        return String::new();
    }

    if trimmed.len() <= max_len {
        return trimmed.to_string();
    }

    // Use first match range to center the window
    if let Some(ranges) = match_ranges
        && let Some(&(match_start, match_end)) = ranges.first()
    {
        let match_start = match_start as usize;
        let match_end = match_end as usize;
        let match_len = match_end.saturating_sub(match_start);

        let budget = max_len.saturating_sub(match_len);
        let before = budget / 3;
        let after = budget - before;

        let win_start = match_start.saturating_sub(before);
        let win_end = (match_end + after).min(trimmed.len());

        // Clamp to char boundaries
        let win_start = floor_char_boundary(trimmed, win_start);
        let win_end = ceil_char_boundary(trimmed, win_end);

        let mut result = trimmed[win_start..win_end].to_string();
        if win_start > 0 {
            result.insert(0, '…');
        }
        if win_end < trimmed.len() {
            result.push('…');
        }
        return result;
    }

    // No match ranges — truncate from start
    let end = ceil_char_boundary(trimmed, max_len);
    format!("{}…", &trimmed[..end])
}

fn floor_char_boundary(s: &str, index: usize) -> usize {
    if index >= s.len() {
        return s.len();
    }
    let mut i = index;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_char_boundary(s: &str, index: usize) -> usize {
    if index >= s.len() {
        return s.len();
    }
    let mut i = index;
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

struct FileMeta<'a> {
    file: &'a FileItem,
    line_number: u64,
    line_content: String,
    is_definition: bool,
    match_ranges: Vec<(u32, u32)>,
    context_after: Vec<String>,
}

pub struct GrepFormatter<'a> {
    pub matches: &'a [GrepMatch],
    pub files: &'a [&'a FileItem],
    pub total_matched: usize,
    pub cursor: GrepCursor,
    pub output_mode: OutputMode,
    pub max_results: usize,
    pub show_context: bool,
    pub auto_expand_defs: bool,
    pub picker: &'a FilePicker,
}

impl GrepFormatter<'_> {
    pub fn format(&self, cursor_store: &mut CursorStore) -> String {
        let GrepFormatter {
            matches,
            files,
            total_matched,
            cursor,
            output_mode,
            max_results,
            show_context,
            auto_expand_defs,
            picker,
        } = *self;

        let items = if matches.len() > max_results {
            &matches[..max_results]
        } else {
            matches
        };

        if output_mode == OutputMode::FilesWithMatches {
            return format_files_with_matches(
                items,
                files,
                cursor,
                auto_expand_defs,
                cursor_store,
                picker,
            );
        }

        if output_mode == OutputMode::Count {
            return format_count(items, files, cursor, cursor_store, picker);
        }
        if show_context
            && let Some(text) =
                self.format_with_context(items, cursor_store, EXPLICIT_CONTEXT_CHARS, false)
        {
            return text;
        }
        if auto_expand_defs
            && items.len() <= 8
            && !items.iter().any(|m| m.is_definition)
            && let Some(text) =
                self.format_with_context(items, cursor_store, AUTO_CONTEXT_CHARS, true)
        {
            return text;
        }

        // output_mode == usage
        let mut lines: Vec<String> = Vec::new();
        let unique_files = {
            let mut seen = std::collections::HashSet::new();
            for m in items {
                seen.insert(m.file_index);
            }
            seen.len()
        };

        let max_output_chars: usize = if output_mode == OutputMode::Usage || unique_files <= 3 {
            5000
        } else if unique_files <= 8 {
            3500
        } else {
            2500
        };

        if total_matched > items.len() {
            lines.push(format!("{}/{} matches shown", items.len(), total_matched));
        }

        // Track which files already had a definition expanded
        let mut def_expanded_files = std::collections::HashSet::new();

        // Detailed content (subject to budget)
        let mut char_count = 0usize;
        let mut current_file = String::new();

        // Definitions first, each file's exact declaration leading its body; then usages, then imports.
        let sorted_items: Vec<usize> = if auto_expand_defs {
            let mut indices: Vec<usize> = (0..items.len()).collect();
            indices.sort_by_cached_key(|&i| {
                let m = &items[i];
                if m.is_definition {
                    (
                        0,
                        m.file_index,
                        !declares_whole_word(m, files[m.file_index]),
                    )
                } else if is_import_line(&m.line_content) {
                    (2, 0, false)
                } else {
                    (1, 0, false)
                }
            });

            indices
        } else {
            (0..items.len()).collect()
        };

        let mut compacted = 0usize;
        let mut body_lines = std::collections::HashSet::new();
        let matching_lines: std::collections::HashMap<_, _> = if auto_expand_defs {
            items
                .iter()
                .map(|m| ((m.file_index, m.line_number), m.match_byte_offsets.as_ref()))
                .collect()
        } else {
            std::collections::HashMap::new()
        };
        for (shown_count, &idx) in sorted_items.iter().enumerate() {
            let m = &items[idx];
            // Already printed inside an expanded definition body.
            if body_lines.contains(&(m.file_index, m.line_number)) {
                continue;
            }
            let file = files[m.file_index];
            let mut match_lines: Vec<String> = Vec::new();
            let over_budget = shown_count > 0 && char_count >= max_output_chars;

            let file_rel_path = file.relative_path(picker);
            if file_rel_path != current_file {
                current_file = file_rel_path;
                match_lines.push(current_file.to_string());
            }

            // Match line
            match_lines.push(format!(
                " {}: {}",
                m.line_number,
                trauncate_line_for_ai(
                    &m.line_content,
                    Some(m.match_byte_offsets.as_ref()),
                    MAX_LINE_LEN
                )
            ));

            // Auto-expand definitions with body context
            let file_rel_for_expand = file.relative_path(picker);
            let expands = auto_expand_defs
                && m.is_definition
                && !m.context_after.is_empty()
                && !def_expanded_files.contains(&file_rel_for_expand);
            if expands && over_budget {
                compacted += 1;
            } else if expands {
                let expand_limit = if def_expanded_files.is_empty() {
                    MAX_DEF_EXPAND_FIRST
                } else {
                    MAX_DEF_EXPAND
                };
                def_expanded_files.insert(file_rel_for_expand);
                let start_line = m.line_number + 1;
                for (i, ctx) in m.context_after.iter().take(expand_limit).enumerate() {
                    if ctx.trim().is_empty() {
                        continue;
                    }
                    body_lines.insert((m.file_index, start_line + i as u64));
                    let ranges = matching_lines
                        .get(&(m.file_index, start_line + i as u64))
                        .copied();
                    match_lines.push(format!(
                        "  {}| {}",
                        start_line + i as u64,
                        trauncate_line_for_ai(ctx, ranges, MAX_LINE_LEN)
                    ));
                }
            }

            let chunk = match_lines.join("\n");
            char_count += chunk.len();
            lines.push(chunk);
        }
        if compacted > 0 {
            let note =
                format!("Definition bodies omitted for {compacted} matches to fit the output.");
            lines.insert(usize::from(total_matched > items.len()), note);
        }

        if cursor.has_more() {
            let cursor_id = cursor_store.store_grep(cursor);
            lines.push(format!("\ncursor: {}", cursor_id));
        }

        lines.join("\n")
    }

    fn format_with_context(
        &self,
        items: &[GrepMatch],
        cursor_store: &mut CursorStore,
        max_chars: usize,
        all_or_nothing: bool,
    ) -> Option<String> {
        let mut rows = std::collections::BTreeMap::new();
        let mut chars = 0;
        let mut with_context = 0;
        for m in items {
            let line =
                trauncate_line_for_ai(&m.line_content, Some(&m.match_byte_offsets), MAX_LINE_LEN);
            // Explicit context budgets only context rows; matching lines are always shown.
            if all_or_nothing {
                chars += line.len() + 8;
            }
            rows.insert((m.file_index, m.line_number), (line, true));
        }
        for m in items {
            let start = m.line_number.saturating_sub(m.context_before.len() as u64);
            let context = m
                .context_before
                .iter()
                .enumerate()
                .map(|(i, text)| (start + i as u64, text))
                .chain(
                    m.context_after
                        .iter()
                        .enumerate()
                        .map(|(i, text)| (m.line_number + 1 + i as u64, text)),
                )
                .filter(|(line, _)| !rows.contains_key(&(m.file_index, *line)))
                .map(|(line, text)| (line, trauncate_line_for_ai(text, None, MAX_LINE_LEN)))
                .collect::<Vec<_>>();
            let cost: usize = context.iter().map(|(_, text)| text.len() + 8).sum();
            if chars + cost > max_chars {
                if all_or_nothing {
                    return None;
                }
                break;
            }
            chars += cost;
            with_context += 1;
            for (line, text) in context {
                rows.insert((m.file_index, line), (text, false));
            }
        }
        let mut lines = Vec::new();
        if with_context < items.len() {
            lines.push(format!(
                "Context shown for {with_context}/{} matches to fit the output.",
                items.len()
            ));
        }
        let mut previous = None;
        for ((file_index, line), (text, matched)) in rows {
            if previous.is_none_or(|(file, _)| file != file_index) {
                lines.push(self.files[file_index].relative_path(self.picker));
            } else if previous.is_some_and(|(_, last)| line > last + 1) {
                lines.push("--".into());
            }
            lines.push(format!(" {line}{} {text}", if matched { ':' } else { '-' }));
            previous = Some((file_index, line));
        }
        if self.cursor.has_more() {
            let id = cursor_store.store_grep(self.cursor);
            lines.push(format!("\ncursor: {id}"));
        }
        Some(lines.join("\n"))
    }
}

fn format_files_with_matches(
    items: &[GrepMatch],
    files: &[&FileItem],
    cursor: GrepCursor,
    auto_expand_defs: bool,
    cursor_store: &mut CursorStore,
    picker: &FilePicker,
) -> String {
    let file_map = collect_file_preview(items, files, picker);

    let mut lines: Vec<String> = Vec::new();
    let file_count = file_map.len();

    let is_small_set = file_count <= 5;
    let mut def_expanded_count = 0usize;

    for (file_idx, fm) in file_map.iter().enumerate() {
        let is_def = fm.is_definition;
        let def_tag = if is_def { " [def]" } else { "" };
        lines.push(format!(
            "{}{}{}",
            fm.file.relative_path(picker),
            def_tag,
            size_tag(fm.file.size)
        ));

        // Show preview
        if !fm.line_content.is_empty() && (is_def || file_idx == 0 || is_small_set) {
            let ranges_ref: Option<&[(u32, u32)]> = if fm.match_ranges.is_empty() {
                None
            } else {
                Some(&fm.match_ranges)
            };
            lines.push(format!(
                "  {}: {}",
                fm.line_number,
                trauncate_line_for_ai(&fm.line_content, ranges_ref, MAX_PREVIEW)
            ));

            // Auto-expand body context
            if auto_expand_defs && !fm.context_after.is_empty() {
                let expand_limit = if is_def {
                    let limit = if def_expanded_count == 0 {
                        MAX_DEF_EXPAND_FIRST
                    } else {
                        MAX_DEF_EXPAND
                    };
                    def_expanded_count += 1;
                    limit
                } else if is_small_set && file_idx == 0 {
                    MAX_FIRST_MATCH_EXPAND
                } else if is_small_set {
                    MAX_DEF_EXPAND
                } else {
                    0
                };

                if expand_limit > 0 {
                    let start_line = fm.line_number + 1;
                    for (i, ctx) in fm.context_after.iter().take(expand_limit).enumerate() {
                        if ctx.trim().is_empty() {
                            continue;
                        }
                        lines.push(format!(
                            "  {}| {}",
                            start_line + i as u64,
                            trauncate_line_for_ai(ctx, None, MAX_PREVIEW)
                        ));
                    }
                }
            }
        }
    }

    if cursor.has_more() {
        let cursor_id = cursor_store.store_grep(cursor);
        lines.push(format!("\ncursor: {}", cursor_id));
    }

    lines.join("\n")
}

fn format_count(
    items: &[GrepMatch],
    files: &[&FileItem],
    cursor: GrepCursor,
    cursor_store: &mut CursorStore,
    picker: &FilePicker,
) -> String {
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for m in items {
        let file = files[m.file_index];
        let path = file.relative_path(picker);
        let count = counts.entry(path.to_string()).or_insert_with(|| {
            order.push(path.to_string());
            0
        });
        *count += 1;
    }

    let mut lines: Vec<String> = Vec::new();
    for path in &order {
        lines.push(format!("{}: {}", path, counts[path.as_str()]));
    }
    if cursor.has_more() {
        let cursor_id = cursor_store.store_grep(cursor);
        lines.push(format!("\ncursor: {}", cursor_id));
    }
    lines.join("\n")
}

fn collect_file_preview<'a>(
    items: &[GrepMatch],
    files: &[&'a FileItem],
    picker: &FilePicker,
) -> Vec<FileMeta<'a>> {
    let mut file_preview: Vec<FileMeta<'a>> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for m in items {
        let file = files[m.file_index];
        if seen.insert(file.relative_path(picker)) {
            file_preview.push(FileMeta {
                file,
                line_number: m.line_number,
                line_content: m.line_content.clone(),
                is_definition: m.is_definition,
                match_ranges: m.match_byte_offsets.iter().copied().collect(),
                context_after: m.context_after.clone(),
            });
        }
    }
    file_preview
}

// Files whose declaration is named by a whole matched word move ahead; the cursor is already set.
pub(crate) fn declarations_first(result: &mut fff::GrepResult<'_>) {
    let mut exact = vec![false; result.files.len()];
    for m in &result.matches {
        exact[m.file_index] |=
            m.is_definition && declares_whole_word(m, result.files[m.file_index]);
    }
    if !exact.contains(&true) {
        return;
    }
    let mut order: Vec<usize> = (0..result.files.len()).collect();
    order.sort_by_key(|&i| !exact[i]);
    let mut position = vec![0; order.len()];
    for (new, &old) in order.iter().enumerate() {
        position[old] = new;
    }
    result.files = order.iter().map(|&old| result.files[old]).collect();
    for m in &mut result.matches {
        m.file_index = position[m.file_index];
    }
    result.matches.sort_by_key(|m| !exact[order[m.file_index]]);
}

// The classifier confirms that the whole-word range is the declared name, not a parameter type.
fn declares_whole_word(m: &GrepMatch, file: &FileItem) -> bool {
    let line = m.line_content.as_bytes();
    let word = |i: usize| {
        line.get(i)
            .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
    };
    m.match_byte_offsets.iter().any(|&(start, end)| {
        let (s, e) = (start as usize, end as usize);
        s < e
            && (s == 0 || !word(s - 1))
            && !word(e)
            && file.is_definition_match(&m.line_content, &[(start, end)])
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suffix_omits_clean_git_status() {
        assert_eq!(file_suffix(None, 0), "");
        assert_eq!(file_suffix(Some(git2::Status::CURRENT), 0), "");
        assert_eq!(file_suffix(Some(git2::Status::empty()), 0), "");
        assert_eq!(file_suffix(None, 120), " - hot");
    }

    #[test]
    fn suffix_keeps_dirty_git_status() {
        assert_eq!(
            file_suffix(Some(git2::Status::WT_MODIFIED), 0),
            " git:modified"
        );
        assert_eq!(file_suffix(Some(git2::Status::WT_NEW), 0), " git:untracked");
        assert_eq!(
            file_suffix(Some(git2::Status::INDEX_MODIFIED), 60),
            " - warm git:staged_modified"
        );
    }

    #[test]
    fn trunc_strips_trailing_whitespace() {
        // Leading whitespace is now stripped by core's trim_whitespace option.
        // This function only strips trailing whitespace.
        assert_eq!(trauncate_line_for_ai("foo()", None, 180), "foo()");
        assert_eq!(trauncate_line_for_ai("bar  ", None, 180), "bar");
        assert_eq!(trauncate_line_for_ai("   ", None, 180), "");
    }

    #[test]
    fn trunc_preserves_pre_trimmed_match_ranges() {
        // Core already stripped leading whitespace and adjusted offsets,
        // so "hello" arrives with match at bytes 0..5.
        let line = "hello";
        let ranges = [(0, 5)];
        let result = trauncate_line_for_ai(line, Some(&ranges), 180);
        assert_eq!(result, "hello");
    }

    #[test]
    fn trunc_long_line_centered() {
        // Core already stripped leading whitespace; offsets are pre-adjusted.
        let line = format!("match_here{}", "x".repeat(200));
        let ranges = [(0u32, 10u32)];
        let result = trauncate_line_for_ai(&line, Some(&ranges), 50);
        assert!(result.contains("match_here"));
        assert!(result.len() <= 55); // budget + ellipsis chars
    }
}
