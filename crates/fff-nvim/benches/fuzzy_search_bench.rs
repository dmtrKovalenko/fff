use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use fff::file_picker::{FFFMode, FilePicker};
use fff::types::PaginationArgs;
use fff::{
    FilePickerOptions, FuzzySearchOptions, GrepMode, GrepSearchOptions, QueryParser,
    SharedFilePicker, SharedFrecency,
};
use std::path::PathBuf;
use std::time::Duration;

fn init_tracing() {
    // use tracing_subscriber::EnvFilter;
    // use tracing_subscriber::fmt;
    // let _ = fmt()
    //     .with_env_filter(
    //         EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
    //     )
    //     .with_target(false)
    //     .with_thread_ids(true)
    //     .with_line_number(true)
    //     .try_init();
}

fn init_file_picker_internal(
    path: &str,
    shared_picker: &SharedFilePicker,
    shared_frecency: &SharedFrecency,
) -> Result<(), String> {
    FilePicker::new_with_shared_state(
        shared_picker.clone(),
        shared_frecency.clone(),
        FilePickerOptions {
            base_path: path.to_string(),
            enable_mmap_cache: false,
            mode: FFFMode::Neovim,
            ..Default::default()
        },
    )
    .map_err(|e| format!("Failed to create FilePicker: {:?}", e))
}

fn wait_for_scan_completion(
    shared_picker: &SharedFilePicker,
    timeout_secs: u64,
) -> Result<usize, String> {
    let start = std::time::Instant::now();
    let timeout = Duration::from_secs(timeout_secs);
    let mut last_log = std::time::Instant::now();
    let mut iteration = 0;

    loop {
        iteration += 1;

        {
            let picker_guard = shared_picker
                .read()
                .map_err(|_| "Failed to acquire read lock")?;
            if let Some(ref picker) = *picker_guard {
                let is_scanning = picker.is_scan_active();
                let file_count = picker.get_files().len();

                // Log progress every 2 seconds
                if last_log.elapsed() >= Duration::from_secs(2) {
                    eprintln!(
                        "  [{:.1}s] Scanning: {}, Files: {}, Iterations: {}",
                        start.elapsed().as_secs_f32(),
                        is_scanning,
                        file_count,
                        iteration
                    );
                    last_log = std::time::Instant::now();
                }

                if !is_scanning && file_count > 0 {
                    eprintln!(
                        "  ✓ Scan complete after {:.2}s: {} files found",
                        start.elapsed().as_secs_f32(),
                        file_count
                    );
                    return Ok(file_count);
                }
            } else {
                if iteration % 100 == 0 {
                    eprintln!(
                        "  [{:.1}s] FilePicker is None (iteration {})",
                        start.elapsed().as_secs_f32(),
                        iteration
                    );
                }
            }
        }

        if start.elapsed() > timeout {
            return Err(format!(
                "Scan timed out after {} seconds (iteration {})",
                timeout_secs, iteration
            ));
        }

        std::thread::sleep(Duration::from_millis(50));
    }
}

fn setup_once() -> Result<(SharedFilePicker, SharedFrecency), String> {
    init_tracing();

    let big_repo_path = PathBuf::from("./big-repo");
    if !big_repo_path.exists() {
        return Err("./big-repo directory does not exist. Run git clone https://github.com/torvalds/linux.git big-repo".to_string());
    }

    let canonical_path = fff::path_utils::canonicalize(&big_repo_path)
        .map_err(|e| format!("Failed to canonicalize path: {}", e))?;
    eprintln!("  Path: {:?}", canonical_path);

    let shared_picker = SharedFilePicker::default();
    let shared_frecency = SharedFrecency::default();

    init_file_picker_internal(
        &canonical_path.to_string_lossy(),
        &shared_picker,
        &shared_frecency,
    )?;

    eprintln!("  Waiting for background scan to complete...");
    let file_count = wait_for_scan_completion(&shared_picker, 120)?;
    eprintln!(
        "  ✓ Indexed {} files (will be reused for all benchmarks)\n",
        file_count
    );

    Ok((shared_picker, shared_frecency))
}

fn bench_search_queries(c: &mut Criterion) {
    let (sp, _sf) = match setup_once() {
        Ok(result) => result,
        Err(e) => {
            eprint!("Failed to setup picker {e:?}");
            return;
        }
    };

    let guard = sp.read().unwrap();
    let picker = guard.as_ref().unwrap();

    let mut group = c.benchmark_group("search");
    group.sample_size(100);

    let test_queries = vec![
        ("short", "mod"),
        ("medium", "controller"),
        ("long", "user_authentication"),
        ("typo", "contrlr"),
        ("partial", "src/lib"),
    ];

    let parser = QueryParser::default();

    for (name, query) in test_queries {
        let parsed = parser.parse(query);
        group.bench_with_input(BenchmarkId::new("query", name), &query, |b, &_query| {
            b.iter(|| {
                let results = picker.fuzzy_search(
                    black_box(&parsed),
                    None,
                    FuzzySearchOptions {
                        max_threads: 4,
                        current_file: None,
                        project_path: None,

                        combo_boost_score_multiplier: 100,
                        min_combo_count: 3,
                        pagination: PaginationArgs {
                            offset: 0,
                            limit: 100,
                        },
                    },
                );
                results.total_matched
            });
        });
    }

    group.finish();
}

/// Benchmark search with different thread counts
fn bench_search_thread_scaling(c: &mut Criterion) {
    let (sp, _sf) = match setup_once() {
        Ok(result) => result,
        Err(e) => {
            eprintln!("Skipping thread scaling benchmarks: {}", e);
            return;
        }
    };

    let guard = sp.read().unwrap();
    let picker = guard.as_ref().unwrap();

    let mut group = c.benchmark_group("thread_scaling");
    group.sample_size(100);

    let query = "controller";
    let parser = QueryParser::default();
    let parsed = parser.parse(query);
    let thread_counts = vec![1, 2, 4, 8];

    for threads in thread_counts {
        group.bench_with_input(
            BenchmarkId::from_parameter(threads),
            &threads,
            |b, &threads| {
                b.iter(|| {
                    let results = picker.fuzzy_search(
                        black_box(&parsed),
                        None,
                        FuzzySearchOptions {
                            max_threads: threads,
                            current_file: None,
                            project_path: None,

                            combo_boost_score_multiplier: 100,
                            min_combo_count: 3,
                            pagination: PaginationArgs {
                                offset: 0,
                                limit: 100,
                            },
                        },
                    );
                    results.total_matched
                });
            },
        );
    }

    group.finish();
}

/// Benchmark search with different result limits
fn bench_search_result_limits(c: &mut Criterion) {
    let (sp, _sf) = match setup_once() {
        Ok(result) => result,
        Err(e) => {
            eprintln!("Skipping result limit benchmarks: {}", e);
            return;
        }
    };

    let guard = sp.read().unwrap();
    let picker = guard.as_ref().unwrap();

    let mut group = c.benchmark_group("result_limits");
    group.sample_size(100);

    let query = "mod";
    let parser = QueryParser::default();
    let parsed = parser.parse(query);
    let result_limits = vec![10, 50, 100, 500];

    for limit in result_limits {
        group.bench_with_input(BenchmarkId::from_parameter(limit), &limit, |b, &limit| {
            b.iter(|| {
                let results = picker.fuzzy_search(
                    black_box(&parsed),
                    None,
                    FuzzySearchOptions {
                        max_threads: 4,
                        current_file: None,
                        project_path: None,

                        combo_boost_score_multiplier: 100,
                        min_combo_count: 3,
                        pagination: PaginationArgs { offset: 0, limit },
                    },
                );
                results.total_matched
            });
        });
    }

    group.finish();
}

/// Benchmark search algorithm performance with queries of varying selectivity
fn bench_search_scalability(c: &mut Criterion) {
    let (sp, _sf) = match setup_once() {
        Ok(result) => result,
        Err(e) => {
            eprintln!("Skipping scalability benchmarks: {}", e);
            return;
        }
    };

    let guard = sp.read().unwrap();
    let picker = guard.as_ref().unwrap();

    if picker.get_files().len() < 1000 {
        eprintln!(
            "Skipping scalability benchmark: need at least 1000 files, got {}",
            picker.get_files().len()
        );
        return;
    }

    let mut group = c.benchmark_group("search_scalability");
    group.sample_size(50);

    let parser = QueryParser::default();
    let selectivity_queries = vec![
        ("broad_a", "a"),
        ("medium_mod", "mod"),
        ("narrow_controller", "controller"),
        ("very_narrow_user_auth", "user_authentication"),
    ];

    for (name, query) in selectivity_queries {
        let parsed = parser.parse(query);
        group.bench_with_input(BenchmarkId::from_parameter(name), &name, |b, _| {
            b.iter(|| {
                let results = picker.fuzzy_search(
                    black_box(&parsed),
                    None,
                    FuzzySearchOptions {
                        max_threads: 4,
                        current_file: None,
                        project_path: None,

                        combo_boost_score_multiplier: 100,
                        min_combo_count: 3,
                        pagination: PaginationArgs {
                            offset: 0,
                            limit: 100,
                        },
                    },
                );
                results.total_matched
            });
        });
    }

    group.finish();
}

/// Benchmark search performance with different ordering modes
fn bench_search_ordering(c: &mut Criterion) {
    let (sp, _sf) = match setup_once() {
        Ok(result) => result,
        Err(e) => {
            eprintln!("Skipping ordering benchmarks: {}", e);
            return;
        }
    };

    let guard = sp.read().unwrap();
    let picker = guard.as_ref().unwrap();

    let mut group = c.benchmark_group("ordering");
    group.sample_size(100);

    let parser = QueryParser::default();
    let parsed_controller = parser.parse("controller");
    let parsed_mod = parser.parse("mod");

    // Benchmark normal order (descending)
    group.bench_function("normal_order", |b| {
        b.iter(|| {
            let results = picker.fuzzy_search(
                black_box(&parsed_controller),
                None,
                FuzzySearchOptions {
                    max_threads: 4,
                    current_file: None,
                    project_path: None,

                    combo_boost_score_multiplier: 100,
                    min_combo_count: 3,
                    pagination: PaginationArgs {
                        offset: 0,
                        limit: 100,
                    },
                },
            );
            results.total_matched
        });
    });

    // Benchmark reverse order (ascending)
    group.bench_function("reverse_order", |b| {
        b.iter(|| {
            let results = picker.fuzzy_search(
                black_box(&parsed_controller),
                None,
                FuzzySearchOptions {
                    max_threads: 4,
                    current_file: None,
                    project_path: None,

                    combo_boost_score_multiplier: 100,
                    min_combo_count: 3,
                    pagination: PaginationArgs {
                        offset: 0,
                        limit: 100,
                    },
                },
            );
            results.total_matched
        });
    });

    // Benchmark with large result set
    group.bench_function("normal_order_large", |b| {
        b.iter(|| {
            let results = picker.fuzzy_search(
                black_box(&parsed_mod),
                None,
                FuzzySearchOptions {
                    max_threads: 4,
                    current_file: None,
                    project_path: None,

                    combo_boost_score_multiplier: 100,
                    min_combo_count: 3,
                    pagination: PaginationArgs {
                        offset: 0,
                        limit: 500,
                    },
                },
            );
            results.total_matched
        });
    });

    group.bench_function("reverse_order_large", |b| {
        b.iter(|| {
            let results = picker.fuzzy_search(
                black_box(&parsed_mod),
                None,
                FuzzySearchOptions {
                    max_threads: 4,
                    current_file: None,
                    project_path: None,

                    combo_boost_score_multiplier: 100,
                    min_combo_count: 3,
                    pagination: PaginationArgs {
                        offset: 0,
                        limit: 500,
                    },
                },
            );
            results.total_matched
        });
    });

    // Benchmark with small result set
    group.bench_function("normal_order_small", |b| {
        b.iter(|| {
            let results = picker.fuzzy_search(
                black_box(&parsed_controller),
                None,
                FuzzySearchOptions {
                    max_threads: 4,
                    current_file: None,
                    project_path: None,

                    combo_boost_score_multiplier: 100,
                    min_combo_count: 3,
                    pagination: PaginationArgs {
                        offset: 0,
                        limit: 10,
                    },
                },
            );
            results.total_matched
        });
    });

    group.bench_function("reverse_order_small", |b| {
        b.iter(|| {
            let results = picker.fuzzy_search(
                black_box(&parsed_controller),
                None,
                FuzzySearchOptions {
                    max_threads: 4,
                    current_file: None,
                    project_path: None,

                    combo_boost_score_multiplier: 100,
                    min_combo_count: 3,
                    pagination: PaginationArgs {
                        offset: 0,
                        limit: 10,
                    },
                },
            );
            results.total_matched
        });
    });

    group.finish();
}

/// Benchmark pagination: first page vs deep page
fn bench_pagination_performance(c: &mut Criterion) {
    let (sp, _sf) = match setup_once() {
        Ok(result) => result,
        Err(e) => {
            eprintln!("Skipping pagination benchmarks: {}", e);
            return;
        }
    };

    let guard = sp.read().unwrap();
    let picker = guard.as_ref().unwrap();

    let mut group = c.benchmark_group("pagination");
    group.sample_size(100);

    let query = "mod";
    let parser = QueryParser::default();
    let parsed = parser.parse(query);
    let page_size = 40;

    // Benchmark first page (uses partial sort optimization)
    group.bench_function("page_0_size_40", |b| {
        b.iter(|| {
            let results = picker.fuzzy_search(
                black_box(&parsed),
                None,
                FuzzySearchOptions {
                    max_threads: 4,
                    current_file: None,
                    project_path: None,

                    combo_boost_score_multiplier: 100,
                    min_combo_count: 3,
                    pagination: PaginationArgs {
                        offset: 0,
                        limit: page_size,
                    },
                },
            );
            results.total_matched
        });
    });

    // Benchmark 10th page (requires full sort, no optimization)
    group.bench_function("page_10_size_40", |b| {
        b.iter(|| {
            let results = picker.fuzzy_search(
                black_box(&parsed),
                None,
                FuzzySearchOptions {
                    max_threads: 4,
                    current_file: None,
                    project_path: None,

                    combo_boost_score_multiplier: 100,
                    min_combo_count: 3,
                    pagination: PaginationArgs {
                        offset: 10,
                        limit: page_size,
                    },
                },
            );
            results.total_matched
        });
    });

    // Benchmark 50th page (even deeper pagination)
    group.bench_function("page_50_size_40", |b| {
        b.iter(|| {
            let results = picker.fuzzy_search(
                black_box(&parsed),
                None,
                FuzzySearchOptions {
                    max_threads: 4,
                    current_file: None,
                    project_path: None,

                    combo_boost_score_multiplier: 100,
                    min_combo_count: 3,
                    pagination: PaginationArgs {
                        offset: 50,
                        limit: page_size,
                    },
                },
            );
            results.total_matched
        });
    });

    group.finish();
}

/// Benchmark grep search via the FilePicker public API
fn bench_grep_search(c: &mut Criterion) {
    let (sp, _sf) = match setup_once() {
        Ok(result) => result,
        Err(e) => {
            eprintln!("Skipping grep benchmarks: {}", e);
            return;
        }
    };

    let guard = sp.read().unwrap();
    let picker = guard.as_ref().unwrap();

    let mut group = c.benchmark_group("grep");
    group.sample_size(50);

    let options = GrepSearchOptions {
        max_file_size: 10 * 1024 * 1024,
        max_matches_per_file: 0,
        smart_case: true,
        file_offset: 0,
        page_limit: 100,
        mode: GrepMode::PlainText,
        time_budget_ms: 0,
        before_context: 0,
        after_context: 0,
        classify_definitions: false,
        trim_whitespace: false,
        abort_signal: None,
    };

    let test_queries = vec![
        ("common", "struct"),
        ("specific", "DEFINE_MUTEX"),
        ("path_filter", "*.h mutex"),
    ];

    let grep_parser = fff::QueryParser::new(fff::GrepConfig);

    for (name, query) in &test_queries {
        let parsed = grep_parser.parse(query);

        group.bench_with_input(BenchmarkId::new("grep", name), query, |b, _| {
            b.iter(|| {
                let result = picker.grep(black_box(&parsed), black_box(&options));
                result.matches.len()
            });
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_search_queries,
    bench_search_thread_scaling,
    bench_search_result_limits,
    bench_search_scalability,
    bench_search_ordering,
    bench_pagination_performance,
    bench_grep_search,
);

criterion_main!(benches);
