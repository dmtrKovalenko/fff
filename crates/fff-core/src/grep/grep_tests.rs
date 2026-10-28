use super::grep::replace_newline_escapes;
use super::*;

use crate::file_picker::{FilePicker, FilePickerOptions};
use crate::index::BigramIndexBuilder;
use std::io::Write;
use std::sync::atomic::AtomicBool;

#[test]
fn invalid_regex_error_survives_empty_literal_prefilter() {
    let root = tempfile::tempdir().unwrap();
    for index in 0..128 {
        std::fs::write(
            root.path().join(format!("file_{index}.txt")),
            "ordinary text",
        )
        .unwrap();
    }
    std::fs::write(root.path().join("file_0.txt"), "Qq marker").unwrap();
    std::fs::write(root.path().join("file_1.txt"), "Zz marker").unwrap();
    let mut picker = FilePicker::new(FilePickerOptions {
        base_path: root.path().to_string_lossy().into_owned(),
        watch: false,
        ..Default::default()
    })
    .unwrap();
    picker.collect_files().unwrap();
    let query = parse_grep_query("QqZz(");
    let options = GrepSearchOptions {
        mode: GrepMode::Regex,
        ..Default::default()
    };
    let before = picker.grep(&query, &options);
    assert!(before.matches.is_empty());
    assert!(before.regex_fallback_error.is_some());

    let builder = BigramIndexBuilder::new(picker.get_files().len());
    let skip = BigramIndexBuilder::new(picker.get_files().len());
    for (index, file) in picker.get_files().iter().enumerate() {
        let content = std::fs::read(root.path().join(file.relative_path(&picker))).unwrap();
        builder.add_file_content(&skip, index, &content);
    }
    picker.set_bigram_index(builder.compress(None));
    let after = picker.grep(&query, &options);
    assert!(after.matches.is_empty());
    assert_eq!(after.total_files_searched, 0);
    assert!(after.regex_fallback_error.is_some());
}

#[test]
fn sparse_matches_after_empty_batches_preserve_pagination() {
    let root = tempfile::tempdir().unwrap();
    for index in 0..1536 {
        let extension = if index < 512 { "rs" } else { "png" };
        let path = root.path().join(format!("file_{index}.{extension}"));
        let content = if [32, 100, 180, 240].contains(&index) {
            "needle\n"
        } else {
            "ordinary content\n"
        };
        std::fs::write(&path, content).unwrap();
        // `File::open` is read-only and Windows requires write access to set file times.
        std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(
                std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000 + index),
            ))
            .unwrap();
    }
    let mut picker = FilePicker::new(FilePickerOptions {
        base_path: root.path().to_string_lossy().into_owned(),
        watch: false,
        ..Default::default()
    })
    .unwrap();
    picker.collect_files().unwrap();

    for mode in [GrepMode::PlainText, GrepMode::Regex] {
        let query = parse_grep_query("needle *.rs");
        let mut options = GrepSearchOptions {
            mode,
            page_limit: usize::MAX,
            ..Default::default()
        };
        let result = picker.grep(&query, &options);
        let expected: Vec<_> = result
            .files
            .iter()
            .map(|file| file.relative_path(&picker))
            .collect();
        assert_eq!(expected.len(), 4);
        assert_eq!(result.filtered_file_count, 512);

        options.page_limit = 1;
        let mut actual = Vec::new();
        loop {
            let page = picker.grep(&query, &options);
            actual.extend(page.files.iter().map(|file| file.relative_path(&picker)));
            assert_eq!(page.total_files, 1536);
            if page.next_file_offset == 0 {
                break;
            }
            assert!(page.next_file_offset > options.file_offset);
            options.file_offset = page.next_file_offset;
        }
        assert_eq!(actual, expected);
    }
}

#[test]
fn definition_only_keeps_unknown_language_files_out_of_fallbacks() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("docs.txt"), "Needle\n").unwrap();
    std::fs::write(dir.path().join("definition.rs"), "fn Needle() {}\n").unwrap();
    let mut picker = FilePicker::new(FilePickerOptions {
        base_path: dir.path().to_string_lossy().into_owned(),
        enable_content_indexing: false,
        enable_mmap_cache: false,
        watch: false,
        ..Default::default()
    })
    .unwrap();
    picker.collect_files().unwrap();
    let parser = fff_query_parser::QueryParser::new(fff_query_parser::AiGrepConfig);
    for mode in [GrepMode::PlainText, GrepMode::Regex, GrepMode::Fuzzy] {
        let options = GrepSearchOptions {
            mode,
            classify_definitions: DefinitionMode::DefinitionOnly,
            time_budget_ms: 0,
            ..Default::default()
        };
        for raw in ["docs.txt Needle", "missing.rs Needle", "*.txt Needle"] {
            let parsed = parser.parse(raw);
            let result = picker.grep(&parsed, &options);
            assert!(result.matches.is_empty(), "{raw}");
            assert_eq!(result.filtered_file_count, 0, "{raw}");
            assert_eq!(result.total_files_searched, 0, "{raw}");
            assert_eq!(result.next_file_offset, 0, "{raw}");
        }
        let parsed = parser.parse("Needle");
        let result = picker.grep(&parsed, &options);
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.filtered_file_count, 1);
        assert!(result.matches[0].is_definition);
    }
}

#[test]
fn definition_only_keeps_empty_scopes_while_usage_only_retries() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("a.rs"),
        "fn Needle() {}\nNeedle();\n*.txt Needle\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("b.txt"), "unrelated\n").unwrap();
    let mut picker = FilePicker::new(FilePickerOptions {
        base_path: dir.path().to_string_lossy().into_owned(),
        enable_content_indexing: false,
        enable_mmap_cache: false,
        watch: false,
        ..Default::default()
    })
    .unwrap();
    picker.collect_files().unwrap();
    let parser = fff_query_parser::QueryParser::new(fff_query_parser::AiGrepConfig);
    for mode in [GrepMode::PlainText, GrepMode::Regex, GrepMode::Fuzzy] {
        let options = GrepSearchOptions {
            mode,
            classify_definitions: DefinitionMode::DefinitionOnly,
            time_budget_ms: 0,
            ..Default::default()
        };
        for raw in ["missing.rs Needle", "*.txt Needle", "!*.rs Needle"] {
            let result = picker.grep(&parser.parse(raw), &options);
            assert!(result.matches.is_empty(), "{mode:?}: {raw}");
            assert_eq!(result.next_file_offset, 0);
            assert!(!result.literal_fallback);
        }
    }

    // usage_only is public API: it keeps the path and literal retries of unfiltered grep.
    let options = GrepSearchOptions {
        classify_definitions: DefinitionMode::UsageOnly,
        time_budget_ms: 0,
        ..Default::default()
    };
    let lines = |raw: &str| {
        let result = picker.grep(&parser.parse(raw), &options);
        let lines: Vec<u64> = result.matches.iter().map(|m| m.line_number).collect();
        (lines, result.literal_fallback)
    };
    assert_eq!(lines("missing.rs Needle"), (vec![2, 3], false));
    assert_eq!(lines("*.txt Needle"), (vec![3], true));
    assert_eq!(lines("!*.rs Needle"), (vec![], false));
}

#[test]
fn pagination_keeps_unreturned_files_before_abort() {
    let file = crate::types::FileItem::new_raw(0, 1, 0, None, false);
    for (page_limit, next_offset) in [(1, 11), (2, 12), (10, 13)] {
        let options = GrepSearchOptions {
            file_offset: 10,
            page_limit,
            classify_definitions: DefinitionMode::DefinitionOnly,
            ..Default::default()
        };
        let results = (0..3)
            .map(|idx| {
                let matched = GrepMatch {
                    file_index: 0,
                    line_number: 1,
                    col: 3,
                    byte_offset: 0,
                    line_content: "fn needle() {}".into(),
                    match_byte_offsets: smallvec::smallvec![(3, 9)],
                    fuzzy_score: Some(100),
                    is_definition: true,
                    context_before: Vec::new(),
                    context_after: Vec::new(),
                };
                (idx, &file, vec![matched])
            })
            .collect();
        let result = GrepResult::collect(results, 5, &options, 15, 15, Some(3));
        assert_eq!(result.matches.len(), page_limit.min(3));
        assert_eq!(result.next_file_offset, next_offset);
    }
}

#[test]
fn filtered_match_limits_keep_late_definitions_and_pagination() {
    let root = tempfile::tempdir().unwrap();
    let source = format!(
        "{}  fn NeedleFirst() {{}}\n  fn NeedleSecond() {{}}\n",
        "  Needle();\n".repeat(50)
    );
    for index in 0..3 {
        std::fs::write(root.path().join(format!("file{index}.rs")), &source).unwrap();
    }
    let mut picker = FilePicker::new(FilePickerOptions {
        base_path: root.path().to_string_lossy().into_owned(),
        watch: false,
        enable_mmap_cache: false,
        enable_content_indexing: false,
        ..Default::default()
    })
    .unwrap();
    picker.collect_files().unwrap();
    let query = parse_grep_query("Needle");
    for mode in [
        Some(GrepMode::PlainText),
        Some(GrepMode::Regex),
        Some(GrepMode::Fuzzy),
        None,
    ] {
        for definition_mode in [
            DefinitionMode::Off,
            DefinitionMode::Classify,
            DefinitionMode::DefinitionOnly,
            DefinitionMode::UsageOnly,
        ] {
            let mut options = GrepSearchOptions {
                classify_definitions: definition_mode,
                mode: mode.unwrap_or(GrepMode::PlainText),
                max_matches_per_file: 1,
                page_limit: 1,
                before_context: 1,
                after_context: 1,
                trim_whitespace: true,
                time_budget_ms: 0,
                ..Default::default()
            };
            let mut seen = std::collections::HashSet::new();
            loop {
                let result = if mode.is_some() {
                    picker.grep(&query, &options)
                } else {
                    picker.multi_grep(&["Needle", "Absent"], &[], &options)
                };
                assert_eq!(result.matches.len(), 1, "{mode:?} {definition_mode:?}");
                let matched = &result.matches[0];
                let definition_only = definition_mode == DefinitionMode::DefinitionOnly;
                assert_eq!(matched.line_number, if definition_only { 51 } else { 1 });
                assert_eq!(matched.is_definition, definition_only);
                assert!(!matched.line_content.starts_with(' '));
                if mode != Some(GrepMode::Fuzzy) {
                    assert_eq!(matched.context_after.len(), 1);
                    assert_eq!(matched.context_before.len(), usize::from(definition_only));
                }
                assert!(seen.insert(result.files[matched.file_index].relative_path(&picker)));
                if result.next_file_offset == 0 {
                    break;
                }
                assert!(result.next_file_offset > options.file_offset);
                options.file_offset = result.next_file_offset;
            }
            assert_eq!(seen.len(), 3);
        }
    }
}

#[test]
fn definition_line_limit_uses_source_bytes_before_display_changes() {
    let root = tempfile::tempdir().unwrap();
    let declaration = "fn Needle() {}";
    let prefix = format!("{declaration} //");
    let unicode = format!(
        "{prefix}{}{}",
        "é".repeat((256 - prefix.len()) / 2),
        " ".repeat((256 - prefix.len()) % 2)
    );
    assert_eq!(unicode.len(), 256);
    let lines = [
        (format!("{declaration:<255}"), true),
        (format!("{declaration:<256}"), true),
        (format!("{declaration:<257}"), false),
        (format!("{declaration:<1024}"), false),
        (unicode.clone(), true),
        (format!("{unicode} "), false),
        (format!("{declaration:>256}"), true),
        (format!("{declaration:>257}"), false),
        (declaration.into(), true),
    ];
    for (filename, ending) in [("lf.rs", "\n"), ("crlf.rs", "\r\n")] {
        let source: String = lines
            .iter()
            .map(|(line, _)| format!("{line}{ending}"))
            .collect();
        std::fs::write(root.path().join(filename), source).unwrap();
    }
    let mut picker = FilePicker::new(FilePickerOptions {
        base_path: root.path().to_string_lossy().into_owned(),
        watch: false,
        enable_mmap_cache: false,
        enable_content_indexing: false,
        ..Default::default()
    })
    .unwrap();
    picker.collect_files().unwrap();
    let query = parse_grep_query("Needle");
    for mode in [
        Some(GrepMode::PlainText),
        Some(GrepMode::Regex),
        Some(GrepMode::Fuzzy),
        None,
    ] {
        for definition_mode in [
            DefinitionMode::Off,
            DefinitionMode::Classify,
            DefinitionMode::DefinitionOnly,
            DefinitionMode::UsageOnly,
        ] {
            for trim_whitespace in [false, true] {
                let options = GrepSearchOptions {
                    classify_definitions: definition_mode,
                    mode: mode.unwrap_or(GrepMode::PlainText),
                    max_matches_per_file: 0,
                    page_limit: usize::MAX,
                    trim_whitespace,
                    time_budget_ms: 0,
                    ..Default::default()
                };
                let result = if mode.is_some() {
                    picker.grep(&query, &options)
                } else {
                    picker.multi_grep(&["Needle", "Absent"], &[], &options)
                };
                let expected = lines
                    .iter()
                    .filter(|(_, definition)| definition_mode.retains(*definition))
                    .count();
                assert_eq!(
                    result.matches.len(),
                    expected * 2,
                    "{mode:?} {definition_mode:?}"
                );
                for matched in &result.matches {
                    let (_, definition) = &lines[matched.line_number as usize - 1];
                    assert!(definition_mode.retains(*definition));
                    assert_eq!(
                        matched.is_definition,
                        definition_mode.is_enabled() && *definition,
                        "{mode:?} {definition_mode:?}: line {}",
                        matched.line_number
                    );
                }
            }
        }
    }
}

#[test]
fn definition_line_limit_counts_only_the_first_multiline_regex_line() {
    let root = tempfile::tempdir().unwrap();
    for length in [256, 257, 1024] {
        let declaration = "fn Needle() {}";
        let source = format!("{declaration:<length$}\r\n{}\r\n", "x".repeat(300));
        std::fs::write(root.path().join(format!("line{length}.rs")), source).unwrap();
    }
    let mut picker = FilePicker::new(FilePickerOptions {
        base_path: root.path().to_string_lossy().into_owned(),
        watch: false,
        enable_mmap_cache: false,
        enable_content_indexing: false,
        ..Default::default()
    })
    .unwrap();
    picker.collect_files().unwrap();
    let result = picker.grep(
        &parse_grep_query(r"Needle[^\n]*\nxxx"),
        &GrepSearchOptions {
            mode: GrepMode::Regex,
            classify_definitions: DefinitionMode::DefinitionOnly,
            time_budget_ms: 0,
            ..Default::default()
        },
    );
    assert!(result.regex_fallback_error.is_none());
    assert_eq!(result.matches.len(), 1);
    let matched = &result.matches[0];
    assert_eq!(
        result.files[matched.file_index].relative_path(&picker),
        "line256.rs"
    );
    assert_eq!(matched.line_content.len(), 256);
    assert!(matched.is_definition);
    assert!(!matched.context_after.is_empty());
}

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
        sort: neo_frizbee::SortStrategy::Unsorted,
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
        let Some(mut mi) = neo_frizbee::Matcher::new(n, &config)
            .match_list_indices(&[h])
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
            let span = (last - first + 1) as usize;
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
        casing: None,
        file_offset: 0,
        page_limit: 100,
        mode: super::GrepMode::PlainText,
        time_budget_ms: 0,
        enforce_time_budget: false,
        before_context: 0,
        after_context: 0,
        classify_definitions: Default::default(),
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
        crate::index::layers::LayerArenas::uniform(arena),
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
        crate::index::layers::LayerArenas::uniform(arena),
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
        crate::index::layers::LayerArenas::uniform(arena),
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
        casing: None,
        file_offset: 0,
        page_limit: 100,
        mode: super::GrepMode::PlainText,
        time_budget_ms: 0,
        enforce_time_budget: false,
        before_context: 0,
        after_context: 0,
        classify_definitions: Default::default(),
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
        casing: None,
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

#[test]
fn multi_grep_keeps_files_for_a_pattern_without_bigrams() {
    let dir = tempfile::tempdir().unwrap();
    let base = crate::path_utils::canonicalize(dir.path()).unwrap();

    // Only a/b/c carry the ASCII needle, so its bigrams stay below the
    // ubiquity threshold that `compress` drops.
    let base_contents: &[(&str, &str)] = &[
        ("a.txt", "hello unicorn world"),
        ("b.txt", "another unicorn line"),
        ("c.txt", "one more unicorn here"),
        ("d.txt", "nothing special in here"),
        ("e.txt", "日本語のテキスト"),
        ("f.txt", "rainbow sky above"),
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
    assert_eq!(picker.get_files().len(), base_contents.len());

    let consec_builder = BigramIndexBuilder::new(base_contents.len());
    let skip_builder = BigramIndexBuilder::new(base_contents.len());
    for (i, (_, content)) in base_contents.iter().enumerate() {
        consec_builder.add_file_content(&skip_builder, i, content.as_bytes());
    }
    // Compressed as build_bigram_index does, so this is the production prefilter.
    let mut index = consec_builder.compress(None);
    index.set_skip_index(skip_builder.compress(Some(
        crate::index::bigram_filter::SKIP_INDEX_MIN_DENSITY_PCT,
    )));
    picker.set_bigram_index(index);

    let options = crate::GrepSearchOptions {
        mode: super::GrepMode::PlainText,
        smart_case: true,
        page_limit: 100,
        ..Default::default()
    };

    let grep = |patterns: &[&str]| -> (Vec<String>, usize) {
        let result = picker.multi_grep(patterns, &[], &options);
        let mut paths: Vec<String> = result
            .files
            .iter()
            .map(|f| f.relative_path(&picker))
            .collect();
        paths.sort();
        (paths, result.total_files_searched)
    };

    // The CJK needle has no printable-ASCII bigram, so nothing can be
    // prefiltered: every file is searched and e.txt is found.
    let (paths, searched) = grep(&["unicorn", "日本語"]);
    assert_eq!(paths, vec!["a.txt", "b.txt", "c.txt", "e.txt"]);
    assert_eq!(searched, base_contents.len());

    // Every pattern indexable: only the files carrying some pattern's
    // bigrams are searched, so the prefilter still narrows.
    let (paths, searched) = grep(&["unicorn", "rainbow"]);
    assert_eq!(paths, vec!["a.txt", "b.txt", "c.txt", "f.txt"]);
    assert_eq!(searched, 4);
    let (paths, searched) = grep(&["unicorn"]);
    assert_eq!(paths, vec!["a.txt", "b.txt", "c.txt"]);
    assert_eq!(searched, 3);
}
