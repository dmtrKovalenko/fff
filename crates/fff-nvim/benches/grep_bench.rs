use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use fff::file_picker::{FFFMode, FilePicker};
use fff::{
    FilePickerOptions, GrepMode, GrepSearchOptions, SharedFilePicker, SharedFrecency,
    parse_grep_query,
};
use std::sync::OnceLock;
use std::time::Duration;

struct TestData {
    shared_picker: SharedFilePicker,
}

static SETUP: OnceLock<TestData> = OnceLock::new();
static SETUP_NO_INDEX: OnceLock<TestData> = OnceLock::new();

fn big_repo_path() -> String {
    if let Some(path) = std::env::var_os("BIG_REPO_PATH") {
        return path.to_string_lossy().into_owned();
    }

    let candidates = ["./big-repo", "../../big-repo"];
    for p in &candidates {
        if std::path::Path::new(p).exists() {
            return p.to_string();
        }
    }
    panic!(
        "./big-repo not found. Run from workspace root:\n  \
         git clone --depth 1 https://github.com/torvalds/linux.git big-repo"
    );
}

fn setup() -> &'static TestData {
    SETUP.get_or_init(|| {
        let path = big_repo_path();
        let shared_picker = SharedFilePicker::default();
        let shared_frecency = SharedFrecency::default();

        eprintln!("Initializing FilePicker for {:?}...", path);
        FilePicker::new_with_shared_state(
            shared_picker.clone(),
            shared_frecency.clone(),
            FilePickerOptions {
                base_path: path,
                enable_mmap_cache: true,
                enable_content_indexing: true,
                mode: FFFMode::Neovim,
                ..Default::default()
            },
        )
        .expect("create picker");

        eprintln!("Waiting for scan completion...");
        shared_picker.wait_for_scan(Duration::from_secs(120));

        eprintln!("Waiting for warmup (bigram index)...");
        loop {
            let guard = shared_picker.read().expect("read lock");
            let picker = guard.as_ref().expect("picker present");
            let progress = picker.get_scan_progress();
            if progress.is_warmup_complete {
                let file_count = picker.get_files().len();
                eprintln!("Ready: {} files indexed, bigram built", file_count);
                break;
            }
            drop(guard);
            std::thread::sleep(Duration::from_millis(100));
        }

        TestData { shared_picker }
    })
}

/// Persistent picker with the bigram index disabled — every grep scans all
/// candidate files. Isolates raw scan throughput from bigram prefilter wins.
fn setup_no_index() -> &'static TestData {
    SETUP_NO_INDEX.get_or_init(|| {
        let path = big_repo_path();
        let shared_picker = SharedFilePicker::default();
        let shared_frecency = SharedFrecency::default();

        eprintln!("Initializing FilePicker (no bigram) for {:?}...", path);
        FilePicker::new_with_shared_state(
            shared_picker.clone(),
            shared_frecency.clone(),
            FilePickerOptions {
                base_path: path,
                enable_mmap_cache: false,
                enable_content_indexing: false,
                mode: FFFMode::Neovim,
                watch: false,
                ..Default::default()
            },
        )
        .expect("create picker");

        shared_picker.wait_for_scan(Duration::from_secs(120));
        let file_count = {
            let guard = shared_picker.read().expect("read lock");
            guard.as_ref().expect("picker present").get_files().len()
        };
        eprintln!("Ready (no bigram): {} files indexed", file_count);

        TestData { shared_picker }
    })
}

fn plain_options() -> GrepSearchOptions {
    GrepSearchOptions {
        max_file_size: 10 * 1024 * 1024,
        max_matches_per_file: 200,
        smart_case: true,
        file_offset: 0,
        page_limit: 50,
        mode: GrepMode::PlainText,
        time_budget_ms: 0,
        before_context: 0,
        after_context: 0,
        classify_definitions: false,
        trim_whitespace: false,
        ..Default::default()
    }
}

fn fuzzy_options() -> GrepSearchOptions {
    GrepSearchOptions {
        mode: GrepMode::Fuzzy,
        ..plain_options()
    }
}

/// One query per selectivity bucket: single-char, common, medium, rare,
/// multi-word, path-constrained.
const PLAIN_QUERIES: &[(&str, &str)] = &[
    ("single_char_x", "x"),
    ("common_return", "return"),
    ("func_mutex_lock", "mutex_lock"),
    ("rare_phylink_ethtool", "phylink_ethtool"),
    ("long_static_int_init", "static int __init"),
    ("path_printk_c", "printk *.c"),
];

/// Fuzzy is expensive (>1s/iter even on warm). Keep three: exact, typo, abbrev.
const FUZZY_QUERIES: &[(&str, &str)] = &[
    ("exact_mutex_lock", "mutex_lock"),
    ("typo_mutx_lock", "mutx_lock"),
    ("abbrev_sched_rt", "sched_rt"),
];

fn bench_plain_warm(c: &mut Criterion) {
    let data = setup();
    let opts = plain_options();

    let mut group = c.benchmark_group("plain_warm");
    group.sample_size(15);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(3));

    for (name, query) in PLAIN_QUERIES {
        group.bench_with_input(BenchmarkId::from_parameter(name), query, |b, q| {
            let guard = data.shared_picker.read().expect("read lock");
            let picker = guard.as_ref().expect("picker present");
            b.iter(|| {
                let parsed = parse_grep_query(q);
                black_box(picker.grep(&parsed, &opts))
            });
        });
    }

    group.finish();
}

fn bench_fuzzy_warm(c: &mut Criterion) {
    let data = setup();
    let opts = fuzzy_options();

    let mut group = c.benchmark_group("fuzzy_warm");
    // Fuzzy iters cost >1s; small sample + tight window keeps the suite fast.
    group.sample_size(10);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(5));

    for (name, query) in FUZZY_QUERIES {
        group.bench_with_input(BenchmarkId::from_parameter(name), query, |b, q| {
            let guard = data.shared_picker.read().expect("read lock");
            let picker = guard.as_ref().expect("picker present");
            b.iter(|| {
                let parsed = parse_grep_query(q);
                black_box(picker.grep(&parsed, &opts))
            });
        });
    }

    group.finish();
}

/// `bench_plain_warm` with the bigram index off. Side-by-side with the warm
/// group it shows the per-query bigram-prefilter contribution.
fn bench_plain_no_index(c: &mut Criterion) {
    let data = setup_no_index();
    let opts = plain_options();

    // Common + medium only; rare queries cost 10-15s/iter without bigram.
    let queries: &[(&str, &str)] = &[
        ("common_return", "return"),
        ("func_mutex_lock", "mutex_lock"),
    ];

    let mut group = c.benchmark_group("plain_no_index");
    group.sample_size(10);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(5));

    for (name, query) in queries {
        group.bench_with_input(BenchmarkId::from_parameter(name), query, |b, q| {
            let guard = data.shared_picker.read().expect("read lock");
            let picker = guard.as_ref().expect("picker present");
            b.iter(|| {
                let parsed = parse_grep_query(q);
                black_box(picker.grep(&parsed, &opts))
            });
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_plain_warm,
    bench_fuzzy_warm,
    bench_plain_no_index,
);

criterion_main!(benches);
