use std::hint::black_box;
use std::time::{Duration, Instant};

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use fff_search::file_picker::{FilePicker, FilePickerOptions};
use fff_search::{
    FuzzySearchOptions, IndexEntry, LayerBuilder, LayerSnapshot, PaginationArgs, QueryParser,
};

const DIRS: usize = 400;
const FILES: usize = 100_000;

// Deterministic tree shaped like a real monorepo: ~250 files per dir,
// 3-4 path segments, mixed extensions.
fn synthetic_paths(n: usize) -> Vec<String> {
    let exts = ["ts", "tsx", "rs", "go", "py", "md", "json", "css"];
    (0..n)
        .map(|i| {
            let dir = i % DIRS;
            format!(
                "packages/pkg{}/src/{}/{}_{}.{}",
                dir % 40,
                ["core", "ui", "api", "util"][dir % 4],
                ["handler", "service", "model", "view", "index"][i % 5],
                i,
                exts[i % exts.len()]
            )
        })
        .collect()
}

fn entries(paths: &[String]) -> Vec<IndexEntry<'_>> {
    paths
        .iter()
        .map(|p| IndexEntry::new(p).with_metadata(4096, 1_700_000_000))
        .collect()
}

fn empty_picker(dir: &std::path::Path) -> FilePicker {
    let mut picker = FilePicker::new(FilePickerOptions {
        base_path: dir.to_str().unwrap().into(),
        watch: false,
        enable_home_dir_scanning: true,
        ..Default::default()
    })
    .unwrap();
    picker.collect_files().unwrap();
    picker
}

fn snapshot(paths: &[String], label: &str) -> LayerSnapshot {
    let mut builder = LayerBuilder::with_capacity(Some(label), paths.len());
    for entry in entries(paths) {
        builder.push(&entry);
    }
    builder.finish()
}

fn search(picker: &FilePicker, query: &str) -> usize {
    let parser = QueryParser::default();
    let parsed = parser.parse(query);
    picker
        .fuzzy_search(
            &parsed,
            None,
            FuzzySearchOptions {
                max_threads: 0,
                pagination: PaginationArgs {
                    offset: 0,
                    limit: 50,
                },
                ..Default::default()
            },
        )
        .total_matched
}

fn bench_build(c: &mut Criterion) {
    let paths = synthetic_paths(FILES);
    let mut group = c.benchmark_group("layer_build");
    group.throughput(Throughput::Elements(FILES as u64));
    group.sample_size(20);

    group.bench_function("builder_push_finish_100k", |b| {
        b.iter(|| black_box(snapshot(&paths, "bench")))
    });

    let dir = tempfile::tempdir().unwrap();
    group.bench_function("add_entries_one_batch_100k", |b| {
        b.iter_batched(
            || empty_picker(dir.path()),
            |mut picker| {
                picker.add_entries(entries(&paths)).unwrap();
                picker
            },
            BatchSize::PerIteration,
        )
    });

    group.bench_function("add_entries_batches_of_1k_100k", |b| {
        b.iter_batched(
            || empty_picker(dir.path()),
            |mut picker| {
                for chunk in paths.chunks(1_000) {
                    picker.add_entries(entries(chunk)).unwrap();
                }
                picker
            },
            BatchSize::PerIteration,
        )
    });

    // Watcher-style trickle: 1 path per call into an already-large layer.
    group.throughput(Throughput::Elements(1_000));
    group.bench_function("add_entries_one_at_a_time_1k_into_100k", |b| {
        b.iter_batched(
            || {
                let mut picker = empty_picker(dir.path());
                picker.add_entries(entries(&paths)).unwrap();
                picker
            },
            |mut picker| {
                for i in 0..1_000 {
                    let path = format!("trickle/file_{i}.rs");
                    picker.add_entries([IndexEntry::new(&path)]).unwrap();
                }
                picker
            },
            BatchSize::PerIteration,
        )
    });

    group.throughput(Throughput::Elements(FILES as u64));
    group.bench_function("import_layer_100k", |b| {
        b.iter_batched(
            || (empty_picker(dir.path()), snapshot(&paths, "bench")),
            |(mut picker, snap)| {
                picker.import_layer(snap).unwrap();
                picker
            },
            BatchSize::PerIteration,
        )
    });
    group.finish();
}

fn bench_persist(c: &mut Criterion) {
    let paths = synthetic_paths(FILES);
    let snap = snapshot(&paths, "bench");
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("layer.fff");
    let bytes = snap.save(&file).unwrap();
    eprintln!(
        "layer file: {FILES} files, {} dirs, {bytes} bytes ({:.1} B/file), arena {} bytes",
        snap.dir_count(),
        bytes as f64 / FILES as f64,
        snap.arena_bytes()
    );

    let mut group = c.benchmark_group("layer_persist");
    group.throughput(Throughput::Elements(FILES as u64));
    group.sample_size(20);
    group.bench_function("save_100k", |b| b.iter(|| snap.save(&file).unwrap()));
    group.bench_function("load_100k", |b| {
        b.iter(|| black_box(LayerSnapshot::load(&file).unwrap()))
    });

    let mut picker = empty_picker(dir.path());
    picker
        .import_layer(LayerSnapshot::load(&file).unwrap())
        .unwrap();
    let layer = picker.current_layer();
    group.bench_function("export_layer_100k", |b| {
        b.iter(|| black_box(picker.export_layer(layer).unwrap()))
    });
    group.bench_function("load_import_base_100k", |b| {
        b.iter_batched(
            || {
                (
                    empty_picker(dir.path()),
                    LayerSnapshot::load(&file).unwrap(),
                )
            },
            |(mut picker, snap)| {
                picker.import_base(snap, None).unwrap();
                picker
            },
            BatchSize::PerIteration,
        )
    });
    group.finish();
}

fn bench_search(c: &mut Criterion) {
    let paths = synthetic_paths(FILES);
    let dir = tempfile::tempdir().unwrap();

    let mut as_base = empty_picker(dir.path());
    as_base.import_base(snapshot(&paths, "base"), None).unwrap();

    let mut as_layer = empty_picker(dir.path());
    as_layer.import_layer(snapshot(&paths, "layer")).unwrap();

    let mut as_open = empty_picker(dir.path());
    as_open.add_entries(entries(&paths)).unwrap();

    let mut group = c.benchmark_group("layer_search");
    group.sample_size(30);
    for query in ["handler", "pkg7 service", "idx99"] {
        group.bench_function(format!("base/{query}"), |b| {
            b.iter(|| black_box(search(&as_base, query)))
        });
        group.bench_function(format!("sealed_layer/{query}"), |b| {
            b.iter(|| black_box(search(&as_layer, query)))
        });
        group.bench_function(format!("open_layer/{query}"), |b| {
            b.iter(|| black_box(search(&as_open, query)))
        });
    }

    // Path lookup: watcher hits resolve a relative path to an index slot.
    let probe = dir.path().join(&paths[FILES / 2]);
    group.bench_function("lookup_path/base", |b| {
        b.iter(|| black_box(as_base.get_file_by_path(&probe).is_some()))
    });
    group.bench_function("lookup_path/sealed_layer", |b| {
        b.iter(|| black_box(as_layer.get_file_by_path(&probe).is_some()))
    });
    group.bench_function("lookup_path/open_layer", |b| {
        b.iter(|| black_box(as_open.get_file_by_path(&probe).is_some()))
    });
    group.finish();
}

// Real repo: fresh filesystem scan vs restoring the saved base. Set
// FFF_BENCH_REPO to enable.
fn bench_real_repo(c: &mut Criterion) {
    let Ok(repo) = std::env::var("FFF_BENCH_REPO") else {
        return;
    };
    let repo = std::path::PathBuf::from(repo);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("base.fff");

    let mut picker = FilePicker::new(FilePickerOptions {
        base_path: repo.to_str().unwrap().into(),
        watch: false,
        ..Default::default()
    })
    .unwrap();
    let started = Instant::now();
    picker.collect_files().unwrap();
    let scan = started.elapsed();
    let files = picker.get_files().len();
    let bytes = picker.save_layer(0, &file).unwrap();
    eprintln!(
        "{}: {files} files, first scan {scan:?}, base file {bytes} bytes",
        repo.display()
    );

    let mut group = c.benchmark_group("real_repo");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(20));
    group.bench_function("fresh_scan", |b| {
        b.iter_batched(
            || {
                FilePicker::new(FilePickerOptions {
                    base_path: repo.to_str().unwrap().into(),
                    watch: false,
                    ..Default::default()
                })
                .unwrap()
            },
            |mut picker| {
                picker.collect_files().unwrap();
                picker
            },
            BatchSize::PerIteration,
        )
    });
    group.bench_function("load_base_from_file", |b| {
        b.iter_batched(
            || {
                FilePicker::new(FilePickerOptions {
                    base_path: repo.to_str().unwrap().into(),
                    watch: false,
                    ..Default::default()
                })
                .unwrap()
            },
            |mut picker| {
                picker
                    .import_base(LayerSnapshot::load(&file).unwrap(), None)
                    .unwrap();
                picker
            },
            BatchSize::PerIteration,
        )
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_build,
    bench_persist,
    bench_search,
    bench_real_repo
);
criterion_main!(benches);
