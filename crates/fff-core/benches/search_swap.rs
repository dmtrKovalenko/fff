use criterion::{
    BatchSize, BenchmarkGroup, Criterion, criterion_group, criterion_main, measurement::WallTime,
};
use fff_search::{
    FFFQuery, FilePicker, FilePickerOptions, FuzzySearchOptions, GrepMode, GrepSearchOptions,
    QueryParser, SearchBuffer, SearchMode, parse_grep_query,
};
use std::{fs, hint::black_box};

fn search_swap(c: &mut Criterion) {
    let root = tempfile::tempdir().unwrap();
    for i in 0..2048 {
        let name = match i % 64 {
            0 => "needle_handler",
            1 => "needle_reader",
            _ => "module",
        };
        let content = if name == "module" {
            "static X: u8 = 0;\n".into()
        } else {
            format!("fn {name}() {{}}\n")
        };
        fs::write(root.path().join(format!("{name}_{i}.rs")), content).unwrap();
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
    assert_eq!(picker.live_file_count(), 2048);

    let parser = QueryParser::default();
    let queries = ["needle", "needle_h"].map(|text| parser.parse(text));
    let options = FuzzySearchOptions {
        max_threads: 4,
        ..Default::default()
    };
    let mut group = c.benchmark_group("search_swap");
    compare(
        &mut group,
        "files",
        &queries,
        picker
            .fuzzy_search(&queries[1], None, options)
            .total_matched,
        |query, previous, next| {
            black_box(picker.fuzzy_search_swap(query, None, options, previous, next)).total_matched
        },
    );

    for (name, mode, prefix, refined) in [
        ("plain_grep", GrepMode::PlainText, "needle", "needle_h"),
        ("regex_grep", GrepMode::Regex, "ne.dle", "ne.dle_h"),
        ("fuzzy_grep", GrepMode::Fuzzy, "needle", "needle_h"),
    ] {
        let queries = [prefix, refined].map(parse_grep_query);
        let options = GrepSearchOptions {
            mode,
            page_limit: usize::MAX,
            time_budget_ms: 0,
            ..Default::default()
        };
        compare(
            &mut group,
            name,
            &queries,
            picker.grep(&queries[1], &options).matches.len(),
            |query, previous, next| {
                black_box(picker.grep_swap(query, &options, previous, next))
                    .matches
                    .len()
            },
        );
    }
}

fn compare<P: Default>(
    group: &mut BenchmarkGroup<'_, WallTime>,
    name: &str,
    [prefix, refined]: &[FFFQuery<'_>; 2],
    expected: usize,
    search: impl Fn(&FFFQuery<'_>, &mut SearchBuffer<P>, &mut SearchBuffer<P>) -> usize,
) {
    assert!(expected > 0);
    for (label, filled) in [("empty_previous", false), ("filled_previous", true)] {
        let prepare = || {
            let [mut previous, mut next] = Default::default();
            // Populate both slots so the destination has identical capacity in both runs.
            search(prefix, &mut previous, &mut next);
            search(prefix, &mut previous, &mut next);
            if !filled {
                previous = SearchBuffer::default();
            }
            [previous, next]
        };
        let [mut previous, mut next] = prepare();
        let scan = previous.metadata().map(|m| m.scanned_at);
        assert_eq!(search(refined, &mut previous, &mut next), expected);
        let meta = previous.metadata().unwrap();
        assert_eq!(scan == Some(meta.scanned_at), filled);
        if matches!(meta.mode, SearchMode::Grep(_)) {
            assert_eq!(meta.grep_files_reused > 0, filled);
        }
        group.bench_function(format!("{name}/{label}"), |b| {
            b.iter_batched_ref(
                prepare,
                |[previous, next]| search(refined, previous, next),
                BatchSize::PerIteration,
            );
        });
    }
}

criterion_group!(benches, search_swap);
criterion_main!(benches);
