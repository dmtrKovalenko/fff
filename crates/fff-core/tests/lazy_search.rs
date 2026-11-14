use fff_search::file_picker::FilePicker;
use fff_search::grep::{GrepSearchOptions, parse_grep_query};
use fff_search::{
    FilePickerOptions, FuzzySearchOptions, LazySource, PaginationArgs, QueryParser,
    SharedFilePicker, SharedFrecency,
};
use std::fs;
use std::path::Path;
use std::time::Duration;

const FILES: usize = 600;

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..FILES {
        let sub = dir.path().join(format!("pkg{}", i % 12));
        fs::create_dir_all(&sub).unwrap();
        let marker = if i % 3 == 0 { "lazy_needle" } else { "plain" };
        fs::write(
            sub.join(format!("file_{i}.rs")),
            format!("fn item_{i}() {{}}\n// {marker} {i}\n"),
        )
        .unwrap();
    }
    fs::write(dir.path().join("unique_target.txt"), "only_here\n").unwrap();
    dir
}

fn lazy_picker(root: &Path) -> SharedFilePicker {
    let shared = SharedFilePicker::default();
    FilePicker::new_lazy_with_shared_state(
        shared.clone(),
        FilePickerOptions {
            base_path: root.to_string_lossy().into_owned(),
            watch: false,
            ..Default::default()
        },
        Default::default(),
    )
    .unwrap();
    shared
}

fn grep_paths(
    shared: &SharedFilePicker,
    query: &str,
    options: &GrepSearchOptions,
) -> (Vec<String>, LazySource) {
    let parsed = parse_grep_query(query);
    shared
        .grep_lazy(&SharedFrecency::noop(), &parsed, options, |page| {
            let mut paths: Vec<String> = page
                .result
                .matches
                .iter()
                .map(|m| page.result.files[m.file_index].relative_path(page.storage))
                .collect();
            paths.sort();
            (paths, page.source)
        })
        .unwrap()
}

fn fuzzy_paths(shared: &SharedFilePicker, query: &str, limit: usize) -> (Vec<String>, LazySource) {
    let parser = QueryParser::default();
    let parsed = parser.parse(query);
    let options = FuzzySearchOptions {
        pagination: PaginationArgs { offset: 0, limit },
        ..Default::default()
    };
    shared
        .fuzzy_search_lazy(
            &SharedFrecency::noop(),
            &parsed,
            None,
            options,
            None,
            |page| {
                let paths = page
                    .result
                    .items
                    .iter()
                    .map(|file| file.relative_path(page.storage))
                    .collect();
                (paths, page.source)
            },
        )
        .unwrap()
}

#[test]
fn lazy_picker_indexes_nothing_until_the_first_search() {
    let dir = fixture();
    let shared = lazy_picker(dir.path());

    std::thread::sleep(Duration::from_millis(50));
    let guard = shared.read().unwrap();
    let picker = guard.as_ref().unwrap();
    assert!(!picker.is_scan_active());
    assert_eq!(picker.get_files().len(), 0);
}

#[test]
fn first_grep_streams_and_the_next_one_uses_the_index() {
    let dir = fixture();
    let shared = lazy_picker(dir.path());
    let options = GrepSearchOptions {
        page_limit: usize::MAX,
        ..Default::default()
    };

    let (streamed, source) = grep_paths(&shared, "lazy_needle", &options);
    assert_eq!(source, LazySource::Stream { complete: true });
    assert_eq!(streamed.len(), FILES.div_ceil(3));

    assert!(shared.wait_for_scan(Duration::from_secs(10)));
    let (indexed, source) = grep_paths(&shared, "lazy_needle", &options);
    assert_eq!(source, LazySource::Index);
    assert_eq!(indexed, streamed);
}

#[test]
fn full_page_returns_before_the_walk_is_searched_completely() {
    let dir = fixture();
    let shared = lazy_picker(dir.path());
    let options = GrepSearchOptions {
        page_limit: 5,
        ..Default::default()
    };

    let (paths, source) = grep_paths(&shared, "fn item_", &options);
    assert!(paths.len() >= 5);
    assert!(matches!(source, LazySource::Stream { .. }));

    // Indexing went on behind the returned page.
    assert!(shared.wait_for_scan(Duration::from_secs(10)));
    let guard = shared.read().unwrap();
    assert_eq!(guard.as_ref().unwrap().get_files().len(), FILES + 1);
}

#[test]
fn first_fuzzy_search_matches_the_indexed_ranking() {
    let dir = fixture();
    let shared = lazy_picker(dir.path());

    let (streamed, source) = fuzzy_paths(&shared, "unique_target", 10);
    assert_eq!(source, LazySource::Stream { complete: true });
    assert_eq!(
        streamed.first().map(String::as_str),
        Some("unique_target.txt")
    );

    let (streamed_many, _) = {
        let other = lazy_picker(dir.path());
        fuzzy_paths(&other, "file_1", 25)
    };

    assert!(shared.wait_for_scan(Duration::from_secs(10)));
    let (indexed, source) = fuzzy_paths(&shared, "unique_target", 10);
    assert_eq!(source, LazySource::Index);
    assert_eq!(indexed, streamed);

    let (indexed_many, _) = fuzzy_paths(&shared, "file_1", 25);
    assert_eq!(indexed_many.len(), 25);
    assert_eq!(streamed_many.len(), 25);
}

#[test]
fn constrained_miss_falls_back_to_the_literal_retry() {
    let dir = fixture();
    fs::write(dir.path().join("ops.txt"), "foo !bar_baz qux\n").unwrap();
    let shared = lazy_picker(dir.path());

    // `!bar_baz` parses as an exclusion, only the literal retry finds the line.
    let (paths, source) = grep_paths(&shared, "foo !bar_baz qux", &GrepSearchOptions::default());
    assert_eq!(source, LazySource::Index);
    assert_eq!(paths, ["ops.txt"]);
}

#[test]
fn concurrent_lazy_searches_share_one_scan() {
    let dir = fixture();
    let shared = lazy_picker(dir.path());
    let options = GrepSearchOptions {
        page_limit: usize::MAX,
        ..Default::default()
    };

    let counts: Vec<usize> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..6)
            .map(|_| scope.spawn(|| grep_paths(&shared, "lazy_needle", &options).0.len()))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    assert!(counts.iter().all(|&count| count == FILES.div_ceil(3)));
}

#[test]
fn regular_picker_streams_during_its_initial_scan_too() {
    let dir = fixture();
    let shared = SharedFilePicker::default();
    FilePicker::new_with_shared_state(
        shared.clone(),
        SharedFrecency::noop(),
        FilePickerOptions {
            base_path: dir.path().to_string_lossy().into_owned(),
            watch: false,
            ..Default::default()
        },
    )
    .unwrap();

    let options = GrepSearchOptions {
        page_limit: usize::MAX,
        ..Default::default()
    };
    let (paths, _) = grep_paths(&shared, "only_here", &options);
    assert_eq!(paths, ["unique_target.txt"]);
}
