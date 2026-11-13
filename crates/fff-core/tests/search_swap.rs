use fff_search::{
    DefinitionMode, DirSearchConfig, FilePicker, FilePickerOptions, FileSearchConfig,
    FuzzySearchOptions, GrepMode, GrepResult, GrepSearchOptions, MixedItemRef, MixedSearchConfig,
    PaginationArgs, QueryParser, SearchBuffer, SearchMode, SearchResult, parse_grep_query,
};
use std::{fs, ptr, time::Duration};

#[test]
fn refine_a_search_in_two_caller_owned_buffers() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("components")).unwrap();
    for (path, text) in [
        ("co", ""),
        (
            "components/controller.rs",
            "fn schema() {}\nschemas();\nname(\n*.txt needle\nab....cdefghijkl\n",
        ),
        ("components/controller_test.txt", "schemas();\n"),
    ] {
        fs::write(root.path().join(path), text).unwrap();
    }
    for i in 0..70 {
        fs::write(root.path().join(format!("{i}.txt")), "unrelated").unwrap();
    }
    let mut picker = FilePicker::new(FilePickerOptions {
        base_path: root.path().display().to_string(),
        enable_mmap_cache: false,
        enable_content_indexing: false,
        watch: false,
        ..Default::default()
    })
    .unwrap();
    picker.collect_files().unwrap();
    check_fuzzy_searches(&picker);

    let mut buffers = Default::default();
    let full = GrepSearchOptions {
        page_limit: usize::MAX,
        time_budget_ms: 0,
        ..Default::default()
    };
    for (mode, prefix, refined) in [
        (GrepMode::PlainText, "schema", "schemas"),
        (GrepMode::Regex, "sch.ma", "sch.mas"),
        (GrepMode::Fuzzy, "shcema", "shcemas"),
    ] {
        let options = GrepSearchOptions {
            mode,
            ..full.clone()
        };
        check_grep(&picker, &mut buffers, prefix, &options);
        check_grep(&picker, &mut buffers, refined, &options);
        let meta = buffers[0].metadata().unwrap();
        assert!(meta.grep_files_reused >= 64);
        assert_eq!(meta.scanned_at, buffers[1].metadata().unwrap().scanned_at);
        assert_eq!(meta.max_typos.is_some(), mode == GrepMode::Fuzzy);

        let partial = GrepSearchOptions {
            page_limit: 1,
            max_matches_per_file: 1,
            ..options.clone()
        };
        check_grep(&picker, &mut buffers, prefix, &partial);
        let resume = GrepSearchOptions {
            file_offset: buffers[0].page.next_file_offset,
            ..options.clone()
        };
        assert!(resume.file_offset > 0);
        check_grep(&picker, &mut buffers, prefix, &resume);
        check_grep(&picker, &mut buffers, refined, &options);
        let definitions = GrepSearchOptions {
            classify_definitions: DefinitionMode::DefinitionOnly,
            ..options.clone()
        };
        check_grep(&picker, &mut buffers, "schema *.rs", &definitions);
        check_grep(&picker, &mut buffers, "schemas", &options);
    }

    for (mode, text) in [
        (GrepMode::Regex, "absent"),
        (GrepMode::Regex, "absent|schema"),
        (GrepMode::Regex, "name("),
        (GrepMode::PlainText, "*.txt needle"),
        (GrepMode::Fuzzy, "abc"),
        (GrepMode::Fuzzy, "abcd"),
        (GrepMode::PlainText, "schema"),
        (GrepMode::PlainText, "schema\\n"),
        (GrepMode::PlainText, ""),
    ] {
        check_grep(
            &picker,
            &mut buffers,
            text,
            &GrepSearchOptions {
                mode,
                ..full.clone()
            },
        );
    }

    let path = root.path().join("0.txt");
    check_grep(&picker, &mut buffers, "new", &full);
    fs::write(&path, "new hit").unwrap();
    let mut buffers = buffers.map(SearchBuffer::<GrepResult>::release_picker);
    picker.handle_create_or_modify(&path).unwrap();
    check_grep(&picker, &mut buffers, "new hit", &full);
    assert_eq!(buffers[0].page.matches.len(), 1);
    assert_eq!(buffers[0].metadata().unwrap().grep_files_reused, 0);

    for ttl in [Duration::from_millis(1), Duration::ZERO] {
        let mut buffers = std::array::from_fn(|_| SearchBuffer::with_ttl(ttl));
        fs::write(&path, "unrelated").unwrap();
        check_grep(&picker, &mut buffers, "new", &full);
        fs::write(&path, "new hit").unwrap();
        std::thread::sleep(ttl);
        check_grep(&picker, &mut buffers, "new hit", &full);
        assert_eq!(buffers[0].page.matches.len(), 1);
        assert_eq!(buffers[0].metadata().unwrap().grep_files_reused, 0);
    }
    // A valid regex prefix can miss a file that its oversized literal fallback matches.
    let literal = format!("x[yz]{}", "a".repeat(600_000));
    fs::write(&path, &literal).unwrap();
    picker.handle_create_or_modify(&path).unwrap();
    let options = GrepSearchOptions {
        mode: GrepMode::Regex,
        ..full
    };
    let mut buffers = Default::default();
    check_grep(&picker, &mut buffers, "x[yz]", &options);
    check_grep(&picker, &mut buffers, &literal, &options);
    assert_eq!(buffers[0].page.matches.len(), 1);
    assert!(buffers[0].page.regex_fallback_error.is_some());
}

fn check_fuzzy_searches(picker: &FilePicker) {
    let options = FuzzySearchOptions {
        pagination: PaginationArgs {
            offset: 0,
            limit: 1,
        },
        ..Default::default()
    };
    let parser = QueryParser::new(FileSearchConfig);
    let [mut previous, mut next]: [SearchBuffer<SearchResult>; 2] = Default::default();
    for text in [
        "co",
        "con",
        "control",
        "controller",
        "Controller",
        "contról",
        "absent_xyz",
        "",
    ] {
        let query = parser.parse(text);
        let fresh = picker.fuzzy_search(&query, None, options);
        let warmed = next.page.items.capacity() > 0;
        let storage = (
            next.page.items.as_ptr(),
            next.page.scores.as_ptr(),
            next.page.match_byte_offsets.as_ptr(),
        );
        let page = picker.fuzzy_search_swap(&query, None, options, &mut previous, &mut next);
        if warmed {
            assert_eq!(
                storage,
                (
                    page.items.as_ptr(),
                    page.scores.as_ptr(),
                    page.match_byte_offsets.as_ptr()
                )
            );
        }
        assert_items(&page.items, &fresh.items);
        assert_eq!(page.total_matched, fresh.total_matched);
        assert!(
            page.scores
                .iter()
                .map(|s| s.total)
                .eq(fresh.scores.iter().map(|s| s.total))
        );
        assert_eq!(page.match_byte_offsets, fresh.match_byte_offsets);
        if text == "co" {
            assert!(page.total_matched > page.items.len());
            assert_eq!(page.items[0].relative_path(picker), "co");
        }
        if text == "control" {
            assert!(page.items[0].relative_path(picker).contains("controller"));
            assert_eq!(
                previous.metadata().unwrap().scanned_at,
                next.metadata().unwrap().scanned_at
            );
        }
    }
    let mut dirs = [SearchBuffer::default(), SearchBuffer::default()];
    let mut mixed = [SearchBuffer::default(), SearchBuffer::default()];
    for text in ["com", "comp", "components/", "components/controller"] {
        let query = QueryParser::new(DirSearchConfig).parse(text);
        let [previous, next] = &mut dirs;
        let page = picker.fuzzy_search_directories_swap(&query, options, previous, next);
        let fresh = picker.fuzzy_search_directories(&query, options);
        assert_items(&page.items, &fresh.items);
        assert_eq!(page.total_matched, fresh.total_matched);

        let query = QueryParser::new(MixedSearchConfig).parse(text);
        let [previous, next] = &mut mixed;
        let page = picker.fuzzy_search_mixed_swap(&query, None, options, previous, next);
        let fresh = picker.fuzzy_search_mixed(&query, None, options);
        let path = |item: &MixedItemRef| match item {
            MixedItemRef::Dir(dir) => dir.relative_path(picker),
            MixedItemRef::File(file) => file.relative_path(picker),
        };
        assert!(page.items.iter().map(path).eq(fresh.items.iter().map(path)));
        assert_eq!(page.total_matched, fresh.total_matched);
    }
}

fn check_grep<'p>(
    picker: &'p FilePicker,
    [previous, next]: &mut [SearchBuffer<GrepResult<'p>>; 2],
    text: &str,
    options: &GrepSearchOptions,
) {
    let query = parse_grep_query(text);
    let fresh = picker.grep(&query, options);
    let storage = (next.page.files.as_ptr(), next.page.matches.as_ptr());
    let fits = fresh.files.len() <= next.page.files.capacity()
        && fresh.matches.len() <= next.page.matches.capacity();
    let page = picker.grep_swap(&query, options, previous, next);
    assert_eq!(page.matches, fresh.matches, "{text}");
    assert_items(&page.files, &fresh.files);
    assert_eq!(page.next_file_offset, fresh.next_file_offset);
    assert_eq!(page.filtered_file_count, fresh.filtered_file_count);
    assert_eq!(page.literal_fallback, fresh.literal_fallback);
    assert_eq!(page.regex_fallback_error, fresh.regex_fallback_error);
    if fits {
        assert_eq!(storage, (page.files.as_ptr(), page.matches.as_ptr()));
    }
    let mode = if page.regex_fallback_error.is_some() {
        GrepMode::PlainText
    } else {
        options.mode
    };
    let meta = previous.metadata().unwrap();
    assert_eq!(meta.mode, SearchMode::Grep(mode));
    assert_eq!(meta.raw_query, text);
}

fn assert_items<T>(page: &[&T], expected: &[&T]) {
    assert!(
        page.len() == expected.len() && page.iter().zip(expected).all(|(a, b)| ptr::eq(*a, *b))
    );
}
