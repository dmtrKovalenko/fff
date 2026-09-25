use criterion::{Criterion, criterion_group, criterion_main};
use fff_search::file_picker::{FilePicker, FilePickerOptions};
use fff_search::{GrepMode, GrepSearchOptions, parse_grep_query};
use std::io::Write;

mod support;

/// Synthetic repo: half the files contain the needle on every line (stresses
/// the per-match find/highlight path), half are pure noise (stresses the
/// whole-file prefilter path).
fn setup_repo(dir: &std::path::Path) {
    for i in 0..400 {
        let mut f = std::fs::File::create(dir.join(format!("match_{i}.rs"))).unwrap();
        for j in 0..100 {
            writeln!(
                f,
                "fn handle_{j}() {{ let controller = Controller::new({j}); controller.run(); }}"
            )
            .unwrap();
        }
    }
    for i in 0..400 {
        let mut f = std::fs::File::create(dir.join(format!("noise_{i}.rs"))).unwrap();
        for j in 0..100 {
            writeln!(
                f,
                "fn compute_{j}() {{ let value = {j} * 42; process(value); }}"
            )
            .unwrap();
        }
    }
}

fn options(mode: GrepMode) -> GrepSearchOptions {
    GrepSearchOptions {
        // Force a full scan of every file so we measure matcher/sink work,
        // not pagination early-exit.
        page_limit: usize::MAX,
        max_matches_per_file: 0,
        mode,
        ..Default::default()
    }
}

fn bench_grep(c: &mut Criterion) {
    if let Some(picker) = support::repo_picker() {
        bench_repo(c, &picker);
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    setup_repo(dir.path());

    let mut picker = FilePicker::new(FilePickerOptions {
        base_path: dir.path().to_str().unwrap().into(),
        watch: false,
        ..Default::default()
    })
    .unwrap();
    picker.collect_files().unwrap();
    assert_eq!(picker.get_files().len(), 800);

    let mut group = c.benchmark_group("grep_e2e");
    group.sample_size(30);

    // Case-sensitive, 40k matched lines: hottest find_at/highlight path
    let query = parse_grep_query("Controller");
    let opts = options(GrepMode::PlainText);
    group.bench_function("plain_case_sensitive_many_matches", |b| {
        b.iter(|| {
            let r = picker.grep(&query, &opts);
            assert_eq!(r.files_with_matches, 400);
            std::hint::black_box(r.matches.len())
        });
    });

    // Case-insensitive (SIMD folding path), 120k matched spans
    let query = parse_grep_query("controller");
    group.bench_function("plain_case_insensitive_many_matches", |b| {
        b.iter(|| {
            let r = picker.grep(&query, &opts);
            assert_eq!(r.files_with_matches, 400);
            std::hint::black_box(r.matches.len())
        });
    });

    // No matches anywhere: whole-file prefilter dominates
    let query = parse_grep_query("Qqzyx");
    group.bench_function("plain_no_matches", |b| {
        b.iter(|| {
            let r = picker.grep(&query, &opts);
            assert_eq!(r.files_with_matches, 0);
            std::hint::black_box(r.total_files_searched)
        });
    });

    // Regex mode: must be unaffected by NeedleFinder changes
    let query = parse_grep_query("Contr[a-z]+ller");
    let regex_opts = options(GrepMode::Regex);
    group.bench_function("regex_many_matches", |b| {
        b.iter(|| {
            let r = picker.grep(&query, &regex_opts);
            assert_eq!(r.files_with_matches, 400);
            std::hint::black_box(r.matches.len())
        });
    });

    let fuzzy_opts = options(GrepMode::Fuzzy);
    for (name, text) in [
        ("fuzzy_exact_many_matches", "controller"),
        ("fuzzy_typo_many_matches", "contrller"),
    ] {
        let query = parse_grep_query(text);
        group.bench_function(name, |b| {
            b.iter(|| {
                let result = picker.grep(&query, &fuzzy_opts);
                assert_eq!(result.files_with_matches, 400);
                std::hint::black_box(result.matches.len())
            });
        });
    }

    group.finish();
}

fn bench_repo(c: &mut Criterion, picker: &FilePicker) {
    let mut group = c.benchmark_group("grep_repo");
    group.sample_size(10);
    group.sampling_mode(criterion::SamplingMode::Flat);
    group.warm_up_time(std::time::Duration::from_secs(1));
    group.measurement_time(std::time::Duration::from_secs(3));
    for (name, text, mode) in [
        ("plain_sensitive", "Controller", GrepMode::PlainText),
        ("plain_insensitive", "controller", GrepMode::PlainText),
        (
            "plain_no_matches",
            "FFF_BENCH_absent_f891c75d",
            GrepMode::PlainText,
        ),
        ("regex", "Contr[a-z]+ller", GrepMode::Regex),
        ("fuzzy_exact", "controller", GrepMode::Fuzzy),
        ("fuzzy_typo", "contrller", GrepMode::Fuzzy),
    ] {
        let query = parse_grep_query(text);
        let options = GrepSearchOptions {
            max_file_size: 10 * 1024 * 1024,
            mode,
            ..Default::default()
        };
        let result = picker.grep(&query, &options);
        eprintln!(
            "Query {name}: matches={} files_with_matches={} searched={} filtered={}",
            result.matches.len(),
            result.files_with_matches,
            result.total_files_searched,
            result.filtered_file_count
        );
        assert!(result.regex_fallback_error.is_none());
        drop(result);
        group.bench_function(name, |b| {
            b.iter(|| std::hint::black_box(picker.grep(&query, &options)));
        });
    }
    group.finish();
}

criterion_group!(benches, bench_grep);
criterion_main!(benches);
