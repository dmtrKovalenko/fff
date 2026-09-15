use super::types::GrepMatch;
use fff_grep::{Searcher, SinkMatch};
use smallvec::SmallVec;

/// Maximum bytes of a matched line to keep for display. Prevents minified
/// JS or huge single-line files from blowing up memory.
pub(super) const MAX_LINE_DISPLAY_LEN: usize = 512;

#[cfg(feature = "definitions")]
#[inline]
pub(super) fn classify_definition(enabled: bool, line: &str) -> bool {
    enabled && super::classify::is_definition_line(line)
}

#[cfg(not(feature = "definitions"))]
#[inline]
pub(super) fn classify_definition(_enabled: bool, _line: &str) -> bool {
    false
}

#[inline]
pub(super) fn debug_assert_newline_terminator(searcher: &Searcher) {
    debug_assert_eq!(
        searcher.line_terminator(),
        fff_grep::LineTerminator::byte(b'\n'),
        "sink helpers assume \\n line terminators (see module invariant)"
    );
}

#[inline]
pub(super) fn strip_line_terminators(bytes: &[u8]) -> &[u8] {
    let mut len = bytes.len();
    while len > 0 && matches!(bytes[len - 1], b'\n' | b'\r') {
        len -= 1;
    }
    &bytes[..len]
}

pub(super) struct SinkState {
    pub(super) file_index: usize,
    pub(super) matches: Vec<GrepMatch>,
    pub(super) max_matches: usize,
    pub(super) before_context: usize,
    pub(super) after_context: usize,
    pub(super) classify_definitions: bool,
}

impl SinkState {
    #[inline]
    pub(super) fn prepare_line<'a>(
        line_bytes: &'a [u8],
        mat: &SinkMatch<'_>,
    ) -> (&'a [u8], u32, u64, u64) {
        let line_number = mat.line_number().unwrap_or(0);
        let byte_offset = mat.absolute_byte_offset();

        // Trim trailing newline/CR directly on bytes to avoid UTF-8 conversion.
        let trimmed_bytes = strip_line_terminators(line_bytes);

        // Truncate for display (floor to a char boundary).
        let display_bytes = truncate_display_bytes(trimmed_bytes);

        let display_len = display_bytes.len() as u32;
        (display_bytes, display_len, line_number, byte_offset)
    }

    #[inline]
    #[allow(clippy::too_many_arguments)]
    pub(super) fn push_match(
        &mut self,
        line_number: u64,
        col: usize,
        byte_offset: u64,
        line_content: String,
        match_byte_offsets: SmallVec<[(u32, u32); 4]>,
        context_before: Vec<String>,
        context_after: Vec<String>,
    ) {
        let is_definition = classify_definition(self.classify_definitions, &line_content);
        self.matches.push(GrepMatch {
            file_index: self.file_index,
            line_number,
            col,
            byte_offset,
            line_content,
            match_byte_offsets,
            fuzzy_score: None,
            is_definition,
            context_before,
            context_after,
        });
    }

    /// Extract context lines from the full buffer around a matched region.
    pub(super) fn extract_context(&self, mat: &SinkMatch<'_>) -> (Vec<String>, Vec<String>) {
        if self.before_context == 0 && self.after_context == 0 {
            return (Vec::new(), Vec::new());
        }

        let buffer = mat.buffer();
        let range = mat.bytes_range_in_buffer();

        let mut before = Vec::new();
        if self.before_context > 0 && range.start > 0 {
            // Walk backward from the start of the match line to find preceding lines
            let mut pos = range.start;
            let mut lines_found = 0;
            while lines_found < self.before_context && pos > 0 {
                // Skip the newline just before our current position
                pos -= 1;
                // Find the previous newline
                let line_start = match memchr::memrchr(b'\n', &buffer[..pos]) {
                    Some(nl) => nl + 1,
                    None => 0,
                };
                let line = &buffer[line_start..pos];
                // Trim trailing \r
                let line = if line.last() == Some(&b'\r') {
                    &line[..line.len() - 1]
                } else {
                    line
                };
                let truncated = truncate_display_bytes(line);
                before.push(String::from_utf8_lossy(truncated).into_owned());
                pos = line_start;
                lines_found += 1;
            }
            before.reverse();
        }

        let mut after = Vec::new();
        if self.after_context > 0 && range.end < buffer.len() {
            let mut pos = range.end;
            let mut lines_found = 0;
            while lines_found < self.after_context && pos < buffer.len() {
                // Find the next newline
                let line_end = match memchr::memchr(b'\n', &buffer[pos..]) {
                    Some(nl) => pos + nl,
                    None => buffer.len(),
                };
                let line = &buffer[pos..line_end];
                // Trim trailing \r
                let line = if line.last() == Some(&b'\r') {
                    &line[..line.len() - 1]
                } else {
                    line
                };
                let truncated = truncate_display_bytes(line);
                after.push(String::from_utf8_lossy(truncated).into_owned());
                pos = if line_end < buffer.len() {
                    line_end + 1 // skip past \n
                } else {
                    buffer.len()
                };
                lines_found += 1;
            }
        }

        (before, after)
    }
}

/// Truncate a byte slice for display, respecting UTF-8 char boundaries.
#[inline]
pub(super) fn truncate_display_bytes(bytes: &[u8]) -> &[u8] {
    if bytes.len() <= MAX_LINE_DISPLAY_LEN {
        bytes
    } else {
        let mut end = MAX_LINE_DISPLAY_LEN;
        while end > 0 && !is_utf8_char_boundary(bytes[end]) {
            end -= 1;
        }
        &bytes[..end]
    }
}

/// Split a multiline match blob (from the MultiLine searcher strategy) into
/// the first line and the remaining lines so `line_content` stays single-line.
pub(super) fn split_multiline_blob(display_bytes: &[u8]) -> (&[u8], Vec<String>) {
    match memchr::memchr(b'\n', display_bytes) {
        None => (display_bytes, Vec::new()),
        Some(pos) => {
            let first = strip_line_terminators(&display_bytes[..pos + 1]);
            let extra = display_bytes[pos + 1..]
                .split(|&b| b == b'\n')
                .map(|l| String::from_utf8_lossy(strip_line_terminators(l)).into_owned())
                .collect();
            (first, extra)
        }
    }
}

/// Convert character-position indices from neo_frizbee into byte-offset
/// pairs (start, end) suitable for `match_byte_offsets`.
///
/// frizbee returns character positions (0-based index into the char
/// iterator). We need byte ranges because the UI renderer and Lua layer
/// use byte offsets for extmark highlights.
///
/// Each matched character becomes its own (byte_start, byte_end) pair.
/// Adjacent characters are merged into a single contiguous range.
pub(super) fn char_indices_to_byte_offsets(
    line: &str,
    char_indices: &[u32],
) -> SmallVec<[(u32, u32); 4]> {
    if char_indices.is_empty() {
        return SmallVec::new();
    }

    // Build a map: char_index -> (byte_start, byte_end) for all chars.
    // Iterating all chars is O(n) in the line length which is bounded by MAX_LINE_DISPLAY_LEN (512).
    let char_byte_ranges: Vec<(usize, usize)> = line
        .char_indices()
        .map(|(byte_pos, ch)| (byte_pos, byte_pos + ch.len_utf8()))
        .collect();

    // Convert char indices to byte ranges, merging adjacent ranges
    let mut result: SmallVec<[(u32, u32); 4]> = SmallVec::with_capacity(char_indices.len());

    for &ci in char_indices {
        let ci = ci as usize;
        if ci >= char_byte_ranges.len() {
            continue; // out of bounds (shouldn't happen with valid data)
        }
        let (start, end) = char_byte_ranges[ci];
        // Merge with previous range if adjacent
        if let Some(last) = result.last_mut()
            && last.1 == start as u32
        {
            last.1 = end as u32;
            continue;
        }
        result.push((start as u32, end as u32));
    }

    result
}

// copied from the rust u8 private method
#[inline]
const fn is_utf8_char_boundary(b: u8) -> bool {
    (b as i8) >= -0x40
}
