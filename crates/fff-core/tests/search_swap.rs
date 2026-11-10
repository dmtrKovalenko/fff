use std::{fs, mem, path::Path};

use fff_search::{
    DirSearchConfig, FilePicker, FilePickerOptions, FileSearchConfig, FuzzySearchOptions, GrepMode,
    GrepSearchOptions, MixedItemRef, MixedSearchConfig, PaginationArgs, QueryParser, SearchBuffer,
    SearchMode, parse_grep_query, refine_grep_into,
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
    picker.fuzzy_search_swap(
        &parser.parse("control"),
        None,
        options,
        &previous,
        &mut next,
    );
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
