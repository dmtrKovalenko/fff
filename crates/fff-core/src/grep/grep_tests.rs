use super::grep::replace_newline_escapes;
use super::*;

use crate::file_picker::{FilePicker, FilePickerOptions};
use crate::index::BigramIndexBuilder;
use std::io::Write;
use std::sync::atomic::AtomicBool;

#[test]
fn test_replace_newline_escapes() {
    // Single \n → multiline: replaced with a real newline at byte 3
    assert_eq!(
        replace_newline_escapes("foo\\nbar"),
        Some(("foo\nbar".to_string(), 3))
    );
    // \\n → escaped backslash + literal n, NOT multiline
    // (this is what the user types when grepping Rust source with `\\nvim`)
    assert_eq!(replace_newline_escapes("foo\\\\nvim-data"), None);
    // Real-world: source file has literal \\AppData\\Local\\nvim-data
    // (double backslash in the file, so user types double backslash)
    assert_eq!(
        replace_newline_escapes(r#"format!("{}\\AppData\\Local\\nvim-data","#),
        None
    );
    // No \n at all
    assert_eq!(replace_newline_escapes("hello world"), None);
    // \\\\n → even number of backslashes before n → NOT multiline
    assert_eq!(replace_newline_escapes("foo\\\\\\\\nbar"), None);
    // \\\n → 3 backslashes: first two pair up, third + n = \n → multiline,
    // newline lands after "foo" + 2 kept backslashes = byte 5
    assert_eq!(
        replace_newline_escapes("foo\\\\\\nbar"),
        Some(("foo\\\\\nbar".to_string(), 5))
    );
    // Position is for the FIRST newline when there are several
    assert_eq!(
        replace_newline_escapes("a\\nb\\nc"),
        Some(("a\nb\nc".to_string(), 1))
    );
}

#[test]
fn test_fuzzy_typo_scoring() {
    // Mirror the config from fuzzy_grep_search
    let needle = "schema";
    let max_typos = (needle.len() / 3).min(2); // 2
    let config = neo_frizbee::Config {
        max_typos: Some(max_typos as u16),
        sort: false,
        scoring: neo_frizbee::Scoring {
            exact_match_bonus: 100,
            ..neo_frizbee::Scoring::default()
        },
        ..Default::default()
    };
    let min_matched = needle.len().saturating_sub(1).max(1); // 5
    let max_match_span = needle.len() + 4; // 10

    // Helper: check if a match would pass our post-filters
    let passes = |n: &str, h: &str| -> bool {
        let Some(mut mi) = neo_frizbee::match_list_indices(n, &[h], &config)
            .into_iter()
            .next()
        else {
            return false;
        };
        // upstream returns indices in reverse order, sort ascending
        mi.indices.sort_unstable();
        if mi.indices.len() < min_matched {
            return false;
        }
        if let (Some(&first), Some(&last)) = (mi.indices.first(), mi.indices.last()) {
            let span = last - first + 1;
            if span > max_match_span {
                return false;
            }
            let density = (mi.indices.len() * 100) / span;
            if density < 70 {
                return false;
            }
        }
        true
    };

    // Exact match: must pass
    assert!(passes("schema", "schema"));
    // Exact in longer line: must pass
    assert!(passes("schema", "  schema: String,"));
    // In identifier: must pass
    assert!(passes("schema", "pub fn validate_schema() {}"));
    // Transposition: must pass
    assert!(passes("shcema", "schema"));
    // Partial "ema" only line: must NOT pass
    assert!(!passes("schema", "it has ema in it"));
    // Completely unrelated: must NOT pass
    assert!(!passes("schema", "hello world foo bar"));
}

#[test]
fn test_multi_grep_search() {
    use crate::file_picker::{FilePicker, FilePickerOptions};
    use std::io::Write;

    let dir = tempfile::tempdir().unwrap();

    // File 1: has "GrepMode" and "GrepMatch"
    {
        let mut f = std::fs::File::create(dir.path().join("grep.rs")).unwrap();
        writeln!(f, "pub enum GrepMode {{").unwrap();
        writeln!(f, "    PlainText,").unwrap();
        writeln!(f, "    Regex,").unwrap();
        writeln!(f, "}}").unwrap();
        writeln!(f, "pub struct GrepMatch {{").unwrap();
        writeln!(f, "    pub line_number: u64,").unwrap();
        writeln!(f, "}}").unwrap();
    }

    // File 2: has "PlainTextMatcher" only
    {
        let mut f = std::fs::File::create(dir.path().join("matcher.rs")).unwrap();
        writeln!(f, "struct PlainTextMatcher {{").unwrap();
        writeln!(f, "    needle: Vec<u8>,").unwrap();
        writeln!(f, "}}").unwrap();
    }

    // File 3: no matches
    {
        let mut f = std::fs::File::create(dir.path().join("other.rs")).unwrap();
        writeln!(f, "fn main() {{").unwrap();
        writeln!(f, "    println!(\"hello\");").unwrap();
        writeln!(f, "}}").unwrap();
    }

    let mut picker = FilePicker::new(FilePickerOptions {
        base_path: dir.path().to_str().unwrap().into(),
        watch: false,
        ..Default::default()
    })
    .unwrap();
    picker.collect_files().unwrap();

    let files = picker.get_files();
    let arena = picker.arena_base_ptr();

    let options = super::GrepSearchOptions {
        max_file_size: MAX_FFFILE_SIZE,
        max_matches_per_file: 0,
        smart_case: true,
        file_offset: 0,
        page_limit: 100,
        mode: super::GrepMode::PlainText,
        time_budget_ms: 0,
        enforce_time_budget: false,
        before_context: 0,
        after_context: 0,
        classify_definitions: false,
        trim_whitespace: false,
        abort_signal: None,
    };
    let no_cancel = AtomicBool::new(false);

    // Test with 3 patterns
    let result = super::multi_grep_search(
        files,
        &["GrepMode", "GrepMatch", "PlainTextMatcher"],
        &[],
        &options,
        picker.cache_budget(),
        None,
        None,
        &no_cancel,
        dir.path(),
        arena,
        arena,
    );

    assert!(
        result.matches.len() >= 3,
        "Expected at least 3 matches, got {}",
        result.matches.len()
    );

    let has_grep_mode = result
        .matches
        .iter()
        .any(|m| m.line_content.contains("GrepMode"));
    let has_grep_match = result
        .matches
        .iter()
        .any(|m| m.line_content.contains("GrepMatch"));
    let has_plain_text_matcher = result
        .matches
        .iter()
        .any(|m| m.line_content.contains("PlainTextMatcher"));

    assert!(has_grep_mode, "Should find GrepMode");
    assert!(has_grep_match, "Should find GrepMatch");
    assert!(has_plain_text_matcher, "Should find PlainTextMatcher");

    assert_eq!(result.files.len(), 2, "Should match exactly 2 files");

    // Test with single pattern
    let result2 = super::multi_grep_search(
        files,
        &["PlainTextMatcher"],
        &[],
        &options,
        picker.cache_budget(),
        None,
        None,
        &no_cancel,
        dir.path(),
        arena,
        arena,
    );
    assert_eq!(
        result2.matches.len(),
        1,
        "Single pattern should find 1 match"
    );

    // Test with empty patterns
    let result3 = super::multi_grep_search(
        files,
        &[],
        &[],
        &options,
        picker.cache_budget(),
        None,
        None,
        &no_cancel,
        dir.path(),
        arena,
        arena,
    );
    assert_eq!(
        result3.matches.len(),
        0,
        "Empty patterns should find nothing"
    );
}

/// E2E: multiline grep (`\n` in query) and escaped-backslash literals (`\\n`)
/// through the full picker.grep pipeline, in both PlainText and Regex modes.
#[test]
fn test_grep_multiline_and_escaped_newline_e2e() {
    let dir = tempfile::tempdir().unwrap();
    let base = crate::path_utils::canonicalize(dir.path()).unwrap();

    // Content spanning two lines: "hello unicorn\nrainbow world"
    {
        let mut f = std::fs::File::create(base.join("multi.txt")).unwrap();
        writeln!(f, "hello unicorn").unwrap();
        writeln!(f, "rainbow world").unwrap();
    }
    // Content with a literal double backslash before "nvim": `C:\\Users\\nvim-data`
    {
        let mut f = std::fs::File::create(base.join("winpath.rs")).unwrap();
        writeln!(f, "let p = \"C:\\\\Users\\\\nvim-data\";").unwrap();
    }
    {
        let mut f = std::fs::File::create(base.join("noise.txt")).unwrap();
        writeln!(f, "nothing interesting here").unwrap();
    }

    let mut picker = FilePicker::new(FilePickerOptions {
        base_path: base.to_str().unwrap().into(),
        watch: false,
        ..Default::default()
    })
    .unwrap();
    picker.collect_files().unwrap();

    let options = crate::GrepSearchOptions {
        page_limit: 100,
        max_matches_per_file: 0,
        ..Default::default()
    };

    // 1. PlainText + `\n`: needle becomes a real newline, match spans two lines
    let query = super::parse_grep_query("unicorn\\nrainbow");
    let result = picker.grep(&query, &options);
    assert_eq!(
        result.files.len(),
        1,
        "multiline plaintext should match multi.txt"
    );
    assert_eq!(result.files[0].relative_path(&picker), "multi.txt");
    let m = &result.matches[0];
    assert_eq!(m.line_content, "hello unicorn");
    // Auto after-context: the rest of the matched span is returned
    assert_eq!(m.context_after, vec!["rainbow world"]);
    // First needle segment highlighted as the line suffix
    assert_eq!(m.match_byte_offsets.as_slice(), &[(6, 13)]);
    assert_eq!(m.col, 6);

    // 2. PlainText + `\\n`: literal backslash + n, must NOT be mangled to newline
    let query = super::parse_grep_query("\\\\nvim-data");
    let result = picker.grep(&query, &options);
    assert_eq!(
        result.files.len(),
        1,
        "escaped backslash should match winpath.rs literally"
    );
    assert_eq!(result.files[0].relative_path(&picker), "winpath.rs");
    assert!(result.matches[0].line_content.contains("\\\\nvim-data"));
    assert!(result.matches[0].context_after.is_empty());

    // 3. Regex + `\n`: goes through the MultiLine searcher strategy
    let regex_options = super::GrepSearchOptions {
        mode: super::GrepMode::Regex,
        ..options.clone()
    };
    let query = super::parse_grep_query("unicorn\\nrainbow");
    let result = picker.grep(&query, &regex_options);
    assert!(result.regex_fallback_error.is_none());
    assert_eq!(
        result.files.len(),
        1,
        "multiline regex should match multi.txt"
    );
    assert_eq!(result.files[0].relative_path(&picker), "multi.txt");
    let m = &result.matches[0];
    // Blob is normalized: single-line content + remaining lines as context
    assert_eq!(m.line_content, "hello unicorn");
    assert_eq!(m.context_after, vec!["rainbow world"]);
    // Highlight clamped to the visible first line
    assert_eq!(m.match_byte_offsets.as_slice(), &[(6, 13)]);
}

/// Regression test for issue #407: Live grep returns duplicate results
/// when the bigram candidate bitset has trailing bits set beyond
/// `base_file_count`. The bitset is rounded up to a multiple of 64 bits
/// so any trailing bit that happens to be set (e.g. from overlay data)
/// would previously map to an overflow file index, which was then also
/// unconditionally appended by the overflow loop, producing duplicates.
#[test]
fn test_grep_no_duplicates_with_overflow_trailing_bits() {
    let dir = tempfile::tempdir().unwrap();
    // Match the picker's internal dunce-canonicalize so paths passed to
    // on_create_or_modify resolve back to the same base_path on Windows.
    let base = crate::path_utils::canonicalize(dir.path()).unwrap();

    // Five base files: only three contain the pattern "unicorn".
    // We need some files WITHOUT the pattern so the bigrams for
    // "unicorn" aren't treated as ubiquitous (≥90% of files) and
    // dropped from the index during compress().
    let base_contents: &[(&str, &str)] = &[
        ("a.txt", "hello unicorn world"),
        ("b.txt", "another unicorn line"),
        ("c.txt", "one more unicorn here"),
        ("d.txt", "nothing special in here"),
        ("e.txt", "just some random content"),
    ];
    for (name, content) in base_contents {
        let mut f = std::fs::File::create(base.join(name)).unwrap();
        writeln!(f, "{}", content).unwrap();
    }

    let mut picker = FilePicker::new(FilePickerOptions {
        base_path: base.to_str().unwrap().into(),
        watch: false,
        ..Default::default()
    })
    .unwrap();
    picker.collect_files().unwrap();
    assert_eq!(picker.get_files().len(), 5);

    // Manually build a bigram index over the 5 base files.
    let base_count = 5usize;
    let consec_builder = BigramIndexBuilder::new(base_count);
    let skip_builder = BigramIndexBuilder::new(base_count);
    for (i, (_, content)) in base_contents.iter().enumerate() {
        consec_builder.add_file_content(&skip_builder, i, content.as_bytes());
    }
    let mut index = consec_builder.compress(Some(0));
    index.set_skip_index(skip_builder.compress(Some(0)));
    picker.set_bigram_index(index);

    // Add three overflow files (new after the bigram index was built),
    // all containing "unicorn".
    for name in ["f.txt", "g.txt", "h.txt"] {
        let path = base.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "overflow unicorn entry").unwrap();
        drop(f);
        picker.handle_create_or_modify(&path);
    }
    assert_eq!(picker.get_files().len(), 8);

    // Inject a trailing bit into the overlay at a file index that
    // corresponds to an overflow file (i.e. >= base_file_count=5 but
    // < bitset_word_size=64). Without the fix, the bigram-candidate
    // merge would set this bit in the bitset, and the bitset loop would
    // push files[6] while the overflow loop also appends files[5..]
    // which includes files[6], producing a duplicate.
    let overflow_rel = "g.txt"; // middle overflow file
    let overflow_abs = picker
        .get_files()
        .iter()
        .position(|f| f.relative_path(&picker) == overflow_rel)
        .expect("overflow file should be present");
    assert!(overflow_abs >= base_count);
    assert!(
        overflow_abs < 64,
        "index must fit in the single bitset word"
    );

    if let Some(overlay) = picker.bigram_overlay() {
        overlay
            .write()
            .modify_file(overflow_abs, b"overflow unicorn entry");
    }

    // Run a grep for "unicorn": six files match
    // (a, b, c in base + f, g, h in overflow).
    let query = super::parse_grep_query("unicorn");
    let options = super::GrepSearchOptions {
        max_file_size: MAX_FFFILE_SIZE,
        max_matches_per_file: 0,
        smart_case: true,
        file_offset: 0,
        page_limit: 100,
        mode: super::GrepMode::PlainText,
        time_budget_ms: 0,
        enforce_time_budget: false,
        before_context: 0,
        after_context: 0,
        classify_definitions: false,
        trim_whitespace: false,
        abort_signal: Some(std::sync::Arc::new(AtomicBool::new(false))),
    };
    let result = picker.grep(&query, &options);

    // Collect the matched relative paths via the returned files list.
    let mut paths: Vec<String> = result
        .files
        .iter()
        .map(|f| f.relative_path(&picker))
        .collect();
    paths.sort();

    // Every file (base + overflow) should match exactly once.
    let mut dedup = paths.clone();
    dedup.dedup();
    assert_eq!(
        dedup, paths,
        "grep must not return duplicate results (issue #407): {:?}",
        paths
    );
    assert_eq!(
        paths,
        vec!["a.txt", "b.txt", "c.txt", "f.txt", "g.txt", "h.txt"],
    );

    // And the match count must equal the number of files (one line per
    // file). A duplicate entry in files_to_search would double-count
    // matches for the duplicated file.
    assert_eq!(
        result.matches.len(),
        6,
        "expected exactly one match per file, got {}",
        result.matches.len()
    );
}

/// Issue #756: an AI-mode regex query with an inline FilePath scope and
/// top-level alternation. The regex fragments are swallowed as bogus Glob
/// constraints, the constrained search finds nothing, and the literal/regex
/// fallback must NOT drop the FilePath scope — otherwise the `|` branch leaks
/// matches into files outside the pinned path.
#[test]
fn regex_fallback_keeps_file_path_scope_issue_756() {
    use fff_query_parser::{AiGrepConfig, QueryParser};
    let dir = tempfile::tempdir().unwrap();
    let base = crate::path_utils::canonicalize(dir.path()).unwrap();
    std::fs::create_dir(base.join("scope")).unwrap();
    std::fs::write(
        base.join("scope").join("target.css"),
        "/* ---------- target ---------- */\n",
    )
    .unwrap();
    std::fs::write(
        base.join("outside.css"),
        "/* ---------- outside ---------- */\n",
    )
    .unwrap();

    let mut picker = FilePicker::new(FilePickerOptions {
        base_path: base.to_str().unwrap().into(),
        watch: false,
        ..Default::default()
    })
    .unwrap();
    picker.collect_files().unwrap();

    let options = crate::GrepSearchOptions {
        mode: super::GrepMode::Regex,
        smart_case: true,
        max_matches_per_file: 80,
        page_limit: 100,
        ..Default::default()
    };

    let raw = r"scope/target.css ^/\* |^\s*/\* ----------";
    let query = QueryParser::new(AiGrepConfig).parse(raw);
    let result = picker.grep(&query, &options);
    let mut paths: Vec<String> = result
        .files
        .iter()
        .map(|f| f.relative_path(&picker))
        .collect();
    paths.sort();

    assert_eq!(
        paths,
        vec!["scope/target.css"],
        "regex fallback must not leak outside the FilePath scope"
    );
}
