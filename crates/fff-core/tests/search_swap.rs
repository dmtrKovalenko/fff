use std::{fs, mem, path::Path};

use fff_search::{
    DirSearchConfig, FilePicker, FilePickerOptions, FileSearchConfig, FuzzySearchOptions, GrepMode,
    GrepResult, GrepSearchOptions, MixedItemRef, MixedSearchConfig, PaginationArgs, QueryParser,
    SearchBuffer, SearchMode, parse_grep_query, refine_grep_into,
};

#[test]
fn fuzzy_swap_reuses_candidates_beyond_the_page() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("components")).unwrap();
    let controller = root.path().join("components/controller.rs");
    fs::write(&controller, "").unwrap();
    fs::write(root.path().join("co"), "").unwrap();
    let mut picker = picker(root.path());
    let parser = QueryParser::new(FileSearchConfig);
    let options = FuzzySearchOptions {
        pagination: PaginationArgs {
            offset: 0,
            limit: 1,
        },
        ..Default::default()
    };
    let mut previous = SearchBuffer::default();
    let mut next = SearchBuffer::default();

    let first = picker.fuzzy_search_swap(&parser.parse("co"), None, options, &previous, &mut next);
    assert_eq!(first.items[0].relative_path(&picker), "co");
    assert!(first.total_matched > first.items.len());
    mem::swap(&mut previous, &mut next);

    let query = parser.parse("control");
    let page = picker.fuzzy_search_swap(&query, None, options, &previous, &mut next);
    assert_eq!(
        page.items[0].relative_path(&picker),
        "components/controller.rs"
    );
    assert_eq!(next.metadata().unwrap().max_typos, Some(2));
    assert!(!page.match_byte_offsets[0].is_empty());
    mem::swap(&mut previous, &mut next);

    for input in [
        "contr",
        "control",
        "controller",
        "controllerxxx",
        "Controller",
        "contról",
    ] {
        let query = parser.parse(input);
        let full = picker.fuzzy_search(&query, None, options);
        let page = picker.fuzzy_search_swap(&query, None, options, &previous, &mut next);
        assert_eq!(page.total_matched, full.total_matched, "{input}");
        assert!(
            page.items
                .iter()
                .zip(&full.items)
                .all(|(a, b)| std::ptr::eq(*a, *b))
        );
        assert_eq!(page.match_byte_offsets, full.match_byte_offsets, "{input}");
        mem::swap(&mut previous, &mut next);
    }

    assert!(picker.remove_file_by_path(&controller));
    let removed = picker.fuzzy_search_swap(
        &parser.parse("control"),
        None,
        options,
        &previous,
        &mut next,
    );
    assert_eq!(removed.total_matched, 0);
    mem::swap(&mut previous, &mut next);
    picker.handle_create_or_modify(&controller).unwrap();
    let page = picker.fuzzy_search_swap(
        &parser.parse("controller"),
        None,
        options,
        &previous,
        &mut next,
    );
    assert_eq!(
        page.items[0].relative_path(&picker),
        "components/controller.rs"
    );

    mem::swap(&mut previous, &mut next);
    fs::remove_file(root.path().join("co")).unwrap();
    fs::write(root.path().join("controllers"), "").unwrap();
    picker.collect_files().unwrap();
    let page = picker.fuzzy_search_swap(
        &parser.parse("controllers"),
        None,
        options,
        &previous,
        &mut next,
    );
    assert_eq!(page.items[0].relative_path(&picker), "controllers");

    let prefix = parser.parse("contr");
    picker.fuzzy_search_swap(&prefix, None, options, &previous, &mut next);
    mem::swap(&mut previous, &mut next);
    let added = root.path().join("contr1");
    fs::write(&added, "").unwrap();
    picker.add_new_file(&added).unwrap();
    let query = parser.parse("contr1");
    let page = picker.fuzzy_search_swap(&query, None, options, &previous, &mut next);
    assert_eq!(page.items[0].relative_path(&picker), "contr1");
}

#[test]
fn fuzzy_swap_preserves_rank_and_highlights_through_consecutive_edits() {
    let root = tempfile::tempdir().unwrap();
    for prefix in ["controller", "comptroller", "container", "connector"] {
        for suffix in ["service", "services", "server", "servant"] {
            fs::write(root.path().join(format!("{prefix}_{suffix}.rs")), "").unwrap();
        }
    }
    let picker = picker(root.path());
    let parser = QueryParser::new(FileSearchConfig);
    let options = FuzzySearchOptions::default();
    let mut previous = SearchBuffer::default();
    let mut next = SearchBuffer::default();
    let input = "controller_service";
    for end in 2..=input.len() {
        let query = parser.parse(&input[..end]);
        let full = picker.fuzzy_search(&query, None, options);
        let page = picker.fuzzy_search_swap(&query, None, options, &previous, &mut next);
        assert_eq!(page.total_matched, full.total_matched);
        assert!(
            page.items
                .iter()
                .map(|&f| std::ptr::from_ref(f))
                .eq(full.items.iter().map(|&f| std::ptr::from_ref(f)))
        );
        assert!(
            page.scores
                .iter()
                .map(|s| s.total)
                .eq(full.scores.iter().map(|s| s.total))
        );
        assert_eq!(page.match_byte_offsets, full.match_byte_offsets);
        mem::swap(&mut previous, &mut next);
    }
}

#[test]
fn directory_and_mixed_swap_match_one_shot_search() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("components")).unwrap();
    fs::write(root.path().join("components/component.rs"), "").unwrap();
    fs::create_dir(root.path().join("common")).unwrap();
    let picker = picker(root.path());
    let options = FuzzySearchOptions::default();
    let mut previous = SearchBuffer::default();
    let mut next = SearchBuffer::default();

    let parser = QueryParser::new(DirSearchConfig);
    for input in ["com", "comp", "components"] {
        let query = parser.parse(input);
        let full = picker.fuzzy_search_directories(&query, options);
        let page = picker.fuzzy_search_directories_swap(&query, options, &previous, &mut next);
        assert_eq!(page.total_matched, full.total_matched);
        assert!(
            page.items
                .iter()
                .zip(&full.items)
                .all(|(a, b)| std::ptr::eq(*a, *b))
        );
        mem::swap(&mut previous, &mut next);
    }

    let parser = QueryParser::new(MixedSearchConfig);
    for input in ["com", "components/", "components/component"] {
        let query = parser.parse(input);
        let full = picker.fuzzy_search_mixed(&query, None, options);
        let page = picker.fuzzy_search_mixed_swap(&query, None, options, &previous, &mut next);
        assert_eq!(page.total_matched, full.total_matched);
        assert!(
            page.items
                .iter()
                .zip(&full.items)
                .all(|(a, b)| match (a, b) {
                    (MixedItemRef::File(a), MixedItemRef::File(b)) => std::ptr::eq(*a, *b),
                    (MixedItemRef::Dir(a), MixedItemRef::Dir(b)) => std::ptr::eq(*a, *b),
                    _ => false,
                })
        );
        mem::swap(&mut previous, &mut next);
    }
}

#[test]
fn grep_swap_reports_pattern_mode_and_typo_budget() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("a.rs"),
        "fn schema() {}\nname(\nfoo !bar_baz qux\n",
    )
    .unwrap();
    let picker = picker(root.path());
    let mut previous = SearchBuffer::default();
    let mut next = SearchBuffer::default();

    for (raw, mode, effective_mode, typos) in [
        (
            "schema *.rs",
            GrepMode::PlainText,
            GrepMode::PlainText,
            None,
        ),
        ("sch.ma", GrepMode::Regex, GrepMode::Regex, None),
        ("shcema", GrepMode::Fuzzy, GrepMode::Fuzzy, Some(2)),
        ("name(", GrepMode::Regex, GrepMode::PlainText, None),
        (
            "foo !bar_baz qux",
            GrepMode::PlainText,
            GrepMode::PlainText,
            None,
        ),
    ] {
        let query = parse_grep_query(raw);
        let options = GrepSearchOptions {
            mode,
            ..Default::default()
        };
        let page = picker.grep_swap(&query, &options, &previous, &mut next);
        assert_eq!(page.matches.len(), 1, "{raw}");
        assert!(!page.matches[0].match_byte_offsets.is_empty(), "{raw}");
        mem::swap(&mut previous, &mut next);
        let metadata = previous.metadata().unwrap();
        assert_eq!(metadata.raw_query, raw);
        assert_eq!(metadata.effective_pattern, raw.trim_end_matches(" *.rs"));
        assert_eq!(metadata.mode, SearchMode::Grep(effective_mode));
        assert_eq!(metadata.max_typos, typos);
    }

    let query = parse_grep_query("schema");
    let options = GrepSearchOptions {
        classify_definitions: fff_search::DefinitionMode::DefinitionOnly,
        ..Default::default()
    };
    let page = picker.grep_swap(&query, &options, &previous, &mut next);
    assert!(page.matches[0].is_definition);

    let hits = [("a.rs", "fn schema() {}"), ("b.rs", "let other = 1;")];
    let mut rows = Vec::new();
    refine_grep_into(hits, "schema", &mut rows);
    assert_eq!(rows, [0]);
    refine_grep_into(hits, "", &mut rows);
    assert_eq!(rows, [0, 1]);
}

#[test]
fn grep_swap_reuses_misses_but_searches_edited_and_unvisited_files() {
    let root = tempfile::tempdir().unwrap();
    for (name, content) in [
        ("a.rs", "nothing here!\n"),
        ("b.rs", "fn schema() {}\nfn schemas() {}\n"),
        ("c.rs", "fn schemas() {}\n"),
        ("d.rs", "other xyz!!!!\n"),
    ] {
        fs::write(root.path().join(name), content).unwrap();
    }
    let mut picker = picker(root.path());
    for index in 0..picker.get_files().len() {
        let name = picker.get_files()[index].relative_path(&picker);
        picker.get_file_mut(index).unwrap().1.access_frecency_score =
            200 - i16::from(name.as_bytes()[0]);
    }

    for (mode, prefix, refined) in [
        (GrepMode::PlainText, "schema", "schemas"),
        (GrepMode::Regex, "sch.ma", "sch.mas"),
        (GrepMode::Fuzzy, "shcema", "shcemas"),
    ] {
        let mut previous = SearchBuffer::default();
        let mut next = SearchBuffer::default();
        let mut options = GrepSearchOptions {
            mode,
            page_limit: 1,
            max_matches_per_file: 1,
            classify_definitions: fff_search::DefinitionMode::DefinitionOnly,
            ..Default::default()
        };
        let prefix = parse_grep_query(prefix);
        let refined = parse_grep_query(refined);
        let page = picker.grep_swap(&prefix, &options, &next, &mut previous);
        assert_ne!(page.next_file_offset, 0, "{mode:?}");
        options.page_limit = usize::MAX;
        let all = picker.grep(&refined, &options);
        let page = picker.grep_swap(&refined, &options, &previous, &mut next);
        assert_eq!(all.files_with_matches, 2, "{mode:?}");
        assert_same_page(&page, &all);
        assert!(next.metadata().unwrap().grep_files_reused > 0, "{mode:?}");

        // Same length, no watcher notification.
        fs::write(root.path().join("a.rs"), "fn schemas(){}").unwrap();
        let all = picker.grep(&refined, &options);
        let page = picker.grep_swap(&refined, &options, &previous, &mut next);
        assert_eq!(all.files_with_matches, 3, "{mode:?}");
        assert_same_page(&page, &all);
        fs::write(root.path().join("a.rs"), "nothing here!\n").unwrap();
    }
}

#[cfg(unix)]
#[test]
fn grep_swap_sees_edits_through_an_open_writable_mapping() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("source.txt");
    fs::write(&path, b"old value").unwrap();
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    let mut mapping = unsafe { memmap2::MmapMut::map_mut(&file) }.unwrap();
    mapping[0] = b'O';
    let picker = picker(root.path());
    let options = GrepSearchOptions::default();
    let mut previous = SearchBuffer::default();
    let mut next = SearchBuffer::default();

    // Start a later second so a timestamp-only check would accept the unchanged inode.
    std::thread::sleep(std::time::Duration::from_secs(1));
    assert!(
        picker
            .grep_swap(&parse_grep_query("new"), &options, &next, &mut previous)
            .matches
            .is_empty()
    );
    mapping[..3].copy_from_slice(b"new");
    let query = parse_grep_query("new value");
    let full = picker.grep(&query, &options);
    assert_eq!(full.matches.len(), 1);
    assert_same_page(
        &picker.grep_swap(&query, &options, &previous, &mut next),
        &full,
    );
}

#[test]
fn grep_swap_widening_queries_find_new_matches() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("a.rs"), "fn schema() {}\nfn other() {}\n").unwrap();
    fs::write(root.path().join("b.rs"), "xxxxxxxx\n").unwrap();
    let picker = picker(root.path());
    let mut previous = SearchBuffer::default();
    let mut next = SearchBuffer::default();
    for (mode, first, second) in [
        (GrepMode::Regex, "absent", "absent|other"),
        (GrepMode::Regex, "absent", "absent?"),
        (GrepMode::PlainText, "otherx", "other"),
        (GrepMode::Fuzzy, "xch", "xchema"),
        (GrepMode::Regex, "schema(", "schema()"),
        (GrepMode::PlainText, "schema\\", "schema\\n"),
    ] {
        let options = GrepSearchOptions {
            mode,
            ..Default::default()
        };
        picker.grep_swap(&parse_grep_query(first), &options, &next, &mut previous);
        let query = parse_grep_query(second);
        let page = picker.grep_swap(&query, &options, &previous, &mut next);
        assert_eq!(
            next.metadata().unwrap().grep_files_reused,
            0,
            "{first} -> {second}"
        );
        assert_same_page(&page, &picker.grep(&query, &options));
    }
}

#[test]
fn regex_misses_do_not_hide_the_literal_fallback() {
    let root = tempfile::tempdir().unwrap();
    let prefix = "x[yz]";
    let oversized = format!("{prefix}{}", "a".repeat(600_000));
    fs::write(root.path().join("source.txt"), &oversized).unwrap();
    let picker = picker(root.path());
    let options = GrepSearchOptions {
        mode: GrepMode::Regex,
        ..Default::default()
    };
    let mut previous = SearchBuffer::default();
    let mut next = SearchBuffer::default();
    picker.grep_swap(&parse_grep_query(prefix), &options, &next, &mut previous);
    let query = parse_grep_query(&oversized);
    let page = picker.grep_swap(&query, &options, &previous, &mut next);
    assert!(page.regex_fallback_error.is_some());
    assert_eq!(page.matches.len(), 1);
}

#[test]
fn fuzzy_grep_keeps_raw_candidates_that_missed_the_score_cutoff() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("a.rs"), "ab....cdefghijkl").unwrap();
    fs::write(root.path().join("b.rs"), "zzzzz").unwrap();
    let picker = picker(root.path());
    let options = GrepSearchOptions {
        mode: GrepMode::Fuzzy,
        ..Default::default()
    };
    let mut previous = SearchBuffer::default();
    let mut next = SearchBuffer::default();
    let prefix = picker.grep_swap(&parse_grep_query("abc"), &options, &next, &mut previous);
    assert!(prefix.matches.is_empty());
    let query = parse_grep_query("abcd");
    let page = picker.grep_swap(&query, &options, &previous, &mut next);
    assert_eq!(page.matches.len(), 1);
    assert_eq!(next.metadata().unwrap().grep_files_reused, 1);
    assert_same_page(&page, &picker.grep(&query, &options));
}

#[test]
fn grep_swap_keeps_unsearched_files_and_lines_hidden_by_filters() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("a.rs"), "schemas();\n").unwrap();
    fs::write(root.path().join("b.rs"), "fn schema() {}\nschemas();\n").unwrap();
    fs::write(root.path().join("c.txt"), "schemas();\n").unwrap();
    let picker = picker(root.path());

    for mode in [GrepMode::PlainText, GrepMode::Regex, GrepMode::Fuzzy] {
        let mut previous = SearchBuffer::default();
        let mut next = SearchBuffer::default();
        let mut options = GrepSearchOptions {
            mode,
            file_offset: 1,
            classify_definitions: fff_search::DefinitionMode::DefinitionOnly,
            max_matches_per_file: 1,
            ..Default::default()
        };
        picker.grep_swap(
            &parse_grep_query("schema *.rs"),
            &options,
            &next,
            &mut previous,
        );
        options.file_offset = 0;
        options.classify_definitions = fff_search::DefinitionMode::UsageOnly;
        let query = parse_grep_query("schemas");
        let full = picker.grep(&query, &options);
        assert_eq!(full.files_with_matches, 3);
        let page = picker.grep_swap(&query, &options, &previous, &mut next);
        assert_same_page(&page, &full);
    }
}

fn picker(base: &Path) -> FilePicker {
    let mut picker = FilePicker::new(FilePickerOptions {
        base_path: base.to_string_lossy().into_owned(),
        watch: false,
        ..Default::default()
    })
    .unwrap();
    picker.collect_files().unwrap();
    picker
}

fn assert_same_page(page: &GrepResult<'_>, expected: &GrepResult<'_>) {
    assert_eq!(page.matches, expected.matches);
    assert!(
        page.files
            .iter()
            .map(|file| std::ptr::from_ref(*file))
            .eq(expected.files.iter().map(|file| std::ptr::from_ref(*file)))
    );
    assert_eq!(page.next_file_offset, expected.next_file_offset);
    assert_eq!(page.filtered_file_count, expected.filtered_file_count);
    assert_eq!(page.regex_fallback_error, expected.regex_fallback_error);
    assert_eq!(page.literal_fallback, expected.literal_fallback);
}
