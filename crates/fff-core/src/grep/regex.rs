use super::sink::{
    SinkState, debug_assert_newline_terminator, split_multiline_blob, strip_line_terminators,
};
use super::types::Casing;
use fff_grep::{
    Searcher, Sink, SinkMatch,
    matcher::{Match, Matcher, NoError},
};
use smallvec::SmallVec;

pub fn has_regex_metacharacters(text: &str) -> bool {
    regex::escape(text) != text
}

pub(super) fn build_regex(pattern: &str, casing: Casing) -> Result<regex::bytes::Regex, String> {
    if pattern.is_empty() {
        return Err("empty pattern".to_string());
    }

    let regex_pattern = if pattern.contains("\\n") {
        pattern.replace("\\n", "\n")
    } else {
        pattern.to_string()
    };

    let case_insensitive = casing.is_insensitive_for(pattern);

    regex::bytes::RegexBuilder::new(&regex_pattern)
        .case_insensitive(case_insensitive)
        .multi_line(true)
        .unicode(false)
        .build()
        .map_err(|e| e.to_string())
}

pub(super) struct RegexMatcher<'r> {
    pub(super) regex: &'r regex::bytes::Regex,
    pub(super) is_multiline: bool,
}

impl Matcher for RegexMatcher<'_> {
    type Error = NoError;

    #[inline]
    fn find_at(&self, haystack: &[u8], at: usize) -> Result<Option<Match>, NoError> {
        let mut at = at;
        while let Some(m) = self.regex.find_at(haystack, at) {
            if !self.is_multiline
                && let Some(newline) = memchr::memchr(b'\n', &haystack[m.start()..m.end()])
            {
                let end = m.start() + newline;
                if let Some(line_match) = self.regex.find_at(&haystack[..end], at) {
                    return Ok(Some(Match::new(line_match.start(), line_match.end())));
                }
                at = end + 1;
                continue;
            }
            return Ok(Some(Match::new(m.start(), m.end())));
        }
        Ok(None)
    }

    #[inline]
    fn line_terminator(&self) -> Option<fff_grep::LineTerminator> {
        if self.is_multiline {
            None
        } else {
            Some(fff_grep::LineTerminator::byte(b'\n'))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitespace_classes_do_not_consume_previous_lines() {
        let source = b"\n\n    def example():\n";
        let regex = build_regex(r"^\s*def example", Casing::Sensitive).unwrap();
        let matcher = RegexMatcher {
            regex: &regex,
            is_multiline: false,
        };
        let found = matcher.find(source).unwrap().unwrap();
        assert_eq!(found.start(), 2);
        assert_eq!(&source[found.start()..found.end()], b"    def example");
        let multiline = RegexMatcher {
            regex: &regex,
            is_multiline: true,
        };
        assert_eq!(multiline.find(source).unwrap().unwrap().start(), 0);
    }

    #[test]
    fn line_bounded_regex_retains_shorter_alternatives() {
        let source = b"foo\nbar\n";
        let regex = build_regex(r"foo\s+bar|foo$", Casing::Sensitive).unwrap();
        let matcher = RegexMatcher {
            regex: &regex,
            is_multiline: false,
        };
        let found = matcher.find(source).unwrap().unwrap();
        assert_eq!(&source[found.start()..found.end()], b"foo");
    }
}

pub(super) struct RegexSink<'r> {
    pub(super) state: SinkState<'r>,
    pub(super) re: &'r regex::bytes::Regex,
}

impl Sink for RegexSink<'_> {
    type Error = std::io::Error;

    fn matched(
        &mut self,
        searcher: &Searcher,
        sink_match: &SinkMatch<'_>,
    ) -> Result<bool, Self::Error> {
        debug_assert_newline_terminator(searcher);
        let line_bytes = sink_match.bytes();
        let (display_bytes, _, line_number, byte_offset) =
            SinkState::prepare_line(line_bytes, sink_match);

        // MultiLine strategy hands over all matched lines as one blob: keep
        // `line_content` single-line, the remaining lines become after-context.
        let first_end = memchr::memchr(b'\n', display_bytes);
        let first_line =
            strip_line_terminators(&display_bytes[..first_end.unwrap_or(display_bytes.len())]);
        let first_len = first_line.len() as u32;
        let line_content = String::from_utf8_lossy(first_line);
        let mut match_byte_offsets: SmallVec<[(u32, u32); 4]> = SmallVec::new();
        let mut col = 0usize;
        let mut first = true;

        for m in self.re.find_iter(display_bytes) {
            let abs_start = m.start() as u32;
            if abs_start >= first_len {
                continue; // highlight only spans visible in the first line
            }
            let abs_end = (m.end() as u32).min(first_len);
            if first {
                col = abs_start as usize;
                first = false;
            }
            match_byte_offsets.push((abs_start, abs_end));
        }

        let Some(is_definition) = self.state.definition_mode.classify_match(
            self.state.file,
            strip_line_terminators(&line_bytes[..first_end.unwrap_or(line_bytes.len())]),
            &match_byte_offsets,
            false,
        ) else {
            return Ok(true);
        };
        let (_, extra_after) = split_multiline_blob(display_bytes);
        let (context_before, context_after) = self.state.extract_context(sink_match);
        let context_after = if extra_after.is_empty() {
            context_after
        } else {
            let mut combined = extra_after;
            combined.extend(context_after);
            combined
        };
        Ok(self.state.push_match(
            line_number,
            col,
            byte_offset,
            line_content.into_owned(),
            match_byte_offsets,
            is_definition,
            context_before,
            context_after,
        ))
    }

    fn finish(&mut self, _: &Searcher, _: &fff_grep::SinkFinish) -> Result<(), Self::Error> {
        Ok(())
    }
}
