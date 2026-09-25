use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use fff_search::{FilePicker, FilePickerOptions, FuzzySearchOptions, PaginationArgs, QueryParser};
use std::{hint::black_box, time::Duration};

mod support;

fn bench_fuzzy_search(c: &mut Criterion) {
    if let Some(picker) = support::repo_picker() {
        let current_file =
            std::env::var("FFF_BENCH_CURRENT_FILE").unwrap_or_else(|_| "README.md".into());
        assert!(picker.base_path().join(&current_file).is_file());
        bench_picker(c, &picker, "fuzzy_search_repo", &current_file, true);
        return;
    }
    for count in [1_000, 10_000, 100_000] {
        let (_root, picker) = make_picker(count);
        bench_picker(
            c,
            &picker,
            &format!("fuzzy_search_{count}"),
            "src/components/group_0/controller_0.rs",
            false,
        );
    }
}

fn bench_picker(
    c: &mut Criterion,
    picker: &FilePicker,
    name: &str,
    current_file: &str,
    repo: bool,
) {
    let parser = QueryParser::default();
    let mut group = c.benchmark_group(name);
    group.sample_size(20);
    group.warm_up_time(Duration::from_millis(if repo { 1000 } else { 300 }));
    group.measurement_time(Duration::from_secs(if repo { 3 } else { 1 }));

    let path_query =
        std::env::var("FFF_BENCH_PATH_QUERY").unwrap_or_else(|_| "src/components".into());
    let multipart_query = std::env::var("FFF_BENCH_MULTIPART_QUERY")
        .unwrap_or_else(|_| "components controller".into());
    for (name, text) in [
        ("empty", ""),
        ("short", "mo"),
        ("filename", "controller"),
        ("path", path_query.as_str()),
        ("multipart", multipart_query.as_str()),
        ("extension", "*.rs"),
    ] {
        let query = parser.parse(text);
        for current_file in [None, Some(current_file)] {
            let options = FuzzySearchOptions {
                max_threads: 4,
                current_file,
                pagination: PaginationArgs {
                    offset: 0,
                    limit: 50,
                },
                ..Default::default()
            };
            let suffix = if current_file.is_some() {
                "current_file"
            } else {
                "no_current_file"
            };
            if repo {
                let result = picker.fuzzy_search(&query, None, options);
                eprintln!("Query {name}/{suffix}: matched={}", result.total_matched);
            }
            group.bench_function(BenchmarkId::new(name, suffix), |b| {
                b.iter(|| black_box(picker.fuzzy_search(black_box(&query), None, options)));
            });
        }
    }
    group.finish();
}

fn make_picker(count: usize) -> (tempfile::TempDir, FilePicker) {
    let root = tempfile::tempdir().unwrap();
    let areas = [
        "src/components",
        "src/core",
        "tests/integration",
        "packages/server",
    ];
    let names = ["controller", "model", "service", "utils", "index"];
    let extensions = ["rs", "ts", "lua", "py"];
    for group in 0..count.div_ceil(100) {
        let directory = root
            .path()
            .join(format!("{}/group_{group}", areas[group % areas.len()]));
        std::fs::create_dir_all(&directory).unwrap();
        for index in group * 100..((group + 1) * 100).min(count) {
            let name = format!(
                "{}_{index}.{}",
                names[index % names.len()],
                extensions[index % extensions.len()]
            );
            let file = std::fs::File::create(directory.join(name)).unwrap();
            let modified =
                std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000 + (index % 1000) as u64);
            file.set_times(std::fs::FileTimes::new().set_modified(modified))
                .unwrap();
        }
    }
    let mut picker = FilePicker::new(FilePickerOptions {
        base_path: root.path().to_string_lossy().into_owned(),
        enable_mmap_cache: false,
        enable_content_indexing: false,
        watch: false,
        ..Default::default()
    })
    .unwrap();
    picker.collect_files().unwrap();
    assert_eq!(picker.live_file_count(), count);
    (root, picker)
}

criterion_group!(benches, bench_fuzzy_search);
criterion_main!(benches);
