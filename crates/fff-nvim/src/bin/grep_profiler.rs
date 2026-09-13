/// Live grep benchmark profiler for fff.nvim
///
/// Benchmarks the full grep pipeline against a large repository (Linux kernel).
/// Measures cold-cache, warm-cache, and incremental typing latencies to simulate
/// real user interaction patterns.
///
/// Uses FilePicker::collect_files for synchronous scanning (no background thread).
///
/// Usage:
///   cargo build --release --bin grep_profiler
///   ./target/release/grep_profiler [--path /path/to/repo]
use fff::file_picker::FilePicker;
use fff::grep::{GrepMode, GrepSearchOptions, parse_grep_query};
use std::time::{Duration, Instant};

fn create_picker(path: &std::path::Path) -> FilePicker {
    let mut picker = FilePicker::new(fff::FilePickerOptions {
        base_path: path.to_string_lossy().to_string(),
        enable_mmap_cache: false,
        mode: fff::FFFMode::Neovim,
        ..Default::default()
    })
    .expect("Failed to create FilePicker");
    picker.collect_files().expect("Failed to collect files");
    picker
}

struct BenchStats {
    times: Vec<Duration>,
}

impl BenchStats {
    fn new() -> Self {
        Self { times: Vec::new() }
    }

    fn push(&mut self, d: Duration) {
        self.times.push(d);
    }

    fn mean(&self) -> Duration {
        let total: Duration = self.times.iter().sum();
        total / self.times.len() as u32
    }

    fn median(&self) -> Duration {
        let mut sorted = self.times.clone();
        sorted.sort();
        sorted[sorted.len() / 2]
    }

    fn p95(&self) -> Duration {
        let mut sorted = self.times.clone();
        sorted.sort();
        let idx = ((sorted.len() as f64) * 0.95) as usize;
        sorted[idx.min(sorted.len() - 1)]
    }

    fn p99(&self) -> Duration {
        let mut sorted = self.times.clone();
        sorted.sort();
        let idx = ((sorted.len() as f64) * 0.99) as usize;
        sorted[idx.min(sorted.len() - 1)]
    }

    fn min(&self) -> Duration {
        *self.times.iter().min().unwrap()
    }

    fn max(&self) -> Duration {
        *self.times.iter().max().unwrap()
    }
}

struct GrepBench<'a> {
    picker: &'a FilePicker,
    options: GrepSearchOptions,
}

impl<'a> GrepBench<'a> {
    fn new(picker: &'a FilePicker) -> Self {
        Self::with_mode(picker, GrepMode::PlainText)
    }

    fn with_mode(picker: &'a FilePicker, mode: GrepMode) -> Self {
        Self {
            picker,
            options: GrepSearchOptions {
                max_file_size: 10 * 1024 * 1024,
                max_matches_per_file: 200,
                smart_case: true,
                file_offset: 0,
                page_limit: 50,
                mode,
                time_budget_ms: 0,
                before_context: 0,
                after_context: 0,
                classify_definitions: false,
                trim_whitespace: false,
                abort_signal: None,
            },
        }
    }

    /// Run a single grep search, return (duration, match_count, files_searched)
    fn run_once(&self, query: &str) -> (Duration, usize, usize) {
        let parsed = parse_grep_query(query);
        let start = Instant::now();
        let result = self.picker.grep(&parsed, &self.options);
        let elapsed = start.elapsed();
        (elapsed, result.matches.len(), result.total_files_searched)
    }

    /// Benchmark a query with multiple iterations
    fn bench_query(&self, query: &str, iterations: usize) -> (BenchStats, usize, usize) {
        let mut stats = BenchStats::new();
        let mut last_matches = 0;
        let mut last_files_searched = 0;

        for _ in 0..iterations {
            let (elapsed, matches, files_searched) = self.run_once(query);
            stats.push(elapsed);
            last_matches = matches;
            last_files_searched = files_searched;
        }

        (stats, last_matches, last_files_searched)
    }
}

fn fmt_dur(d: Duration) -> String {
    let us = d.as_micros();
    if us > 1_000_000 {
        format!("{:.2}s", d.as_secs_f64())
    } else if us > 1000 {
        format!("{:.2}ms", us as f64 / 1000.0)
    } else {
        format!("{}us", us)
    }
}

fn print_row(name: &str, stats: &BenchStats, matches: usize, files_searched: usize, iters: usize) {
    eprintln!(
        "  {:<24} | {:>8} | {:>8} | {:>8} | {:>8} | {:>8} | {:>8} | {:>6} | {:>6} | {:>4}",
        name,
        fmt_dur(stats.mean()),
        fmt_dur(stats.median()),
        fmt_dur(stats.p95()),
        fmt_dur(stats.p99()),
        fmt_dur(stats.min()),
        fmt_dur(stats.max()),
        matches,
        files_searched,
        iters,
    );
}

fn print_header() {
    eprintln!(
        "  {:<24} | {:>8} | {:>8} | {:>8} | {:>8} | {:>8} | {:>8} | {:>6} | {:>6} | {:>4}",
        "Name", "Mean", "Median", "P95", "P99", "Min", "Max", "Match", "Files", "Iter"
    );
    eprintln!(
        "  {:-<24}-+-{:-<8}-+-{:-<8}-+-{:-<8}-+-{:-<8}-+-{:-<8}-+-{:-<8}-+-{:-<6}-+-{:-<6}-+-{:-<4}",
        "", "", "", "", "", "", "", "", "", ""
    );
}

fn main() {
    // Parse args
    let args: Vec<String> = std::env::args().collect();
    let repo_path = if let Some(idx) = args.iter().position(|a| a == "--path") {
        args.get(idx + 1)
            .map(|s| s.as_str())
            .unwrap_or("./big-repo")
    } else {
        "./big-repo"
    };

    let repo = std::path::PathBuf::from(repo_path);
    if !repo.exists() {
        eprintln!("Repository not found at: {}", repo_path);
        eprintln!("Usage: grep_profiler [--path /path/to/large/repo]");
        std::process::exit(1);
    }

    let canonical = fff::path_utils::canonicalize(&repo).expect("Failed to canonicalize path");
    eprintln!("=== FFF Live Grep Profiler ===");
    eprintln!("Repository: {:?}", canonical);

    // Direct file loading (no background thread)
    eprintln!("\n[1/7] Loading files...");
    let load_start = Instant::now();
    let picker = create_picker(&canonical);
    let load_time = load_start.elapsed();
    let files = picker.get_files();
    let non_binary = files.iter().filter(|f| !f.is_binary()).count();
    let large_files = files.iter().filter(|f| f.size > 10 * 1024 * 1024).count();
    eprintln!(
        "  Loaded {} files in {:.2}s ({} non-binary, {} >10MB skipped)\n",
        files.len(),
        load_time.as_secs_f64(),
        non_binary,
        large_files,
    );

    let bench = GrepBench::new(&picker);

    eprintln!("[2/7] Cold cache benchmarks (first search, mmap not yet loaded)");
    eprintln!("  Each query runs once with fresh FileItem mmaps.\n");
    print_header();

    let cold_queries: Vec<(&str, &str)> = vec![
        ("cold_common_2char", "if"),
        ("cold_common_word", "return"),
        ("cold_specific_func", "mutex_lock"),
        ("cold_struct_name", "inode_operations"),
        ("cold_define", "MODULE_LICENSE"),
        ("cold_rare_string", "phylink_ethtool"),
        ("cold_path_filter", "printk *.c"),
        ("cold_long_query", "static int __init"),
    ];

    for (name, query) in &cold_queries {
        // Re-load files to get fresh FileItems with no cached mmaps
        let fresh_picker = create_picker(&canonical);
        let fresh_bench = GrepBench::new(&fresh_picker);
        let (stats, matches, files_searched) = fresh_bench.bench_query(query, 1);
        print_row(name, &stats, matches, files_searched, 1);
    }

    eprintln!("\n[3/7] Warm cache benchmarks (plain text, mmap cache populated)");
    eprintln!("  Running 3 warmup iterations, then measuring.\n");
    print_header();

    let warm_queries: Vec<(&str, &str, usize)> = vec![
        ("warm_2char", "if", 10),
        ("warm_common_word", "return", 10),
        ("warm_function_call", "mutex_lock", 15),
        ("warm_struct_name", "inode_operations", 15),
        ("warm_define", "MODULE_LICENSE", 15),
        ("warm_rare_string", "phylink_ethtool", 20),
        ("warm_include", "#include", 10),
        ("warm_comment", "TODO", 15),
        ("warm_type_decl", "struct file", 15),
        ("warm_error_path", "err = -EINVAL", 15),
        ("warm_long_pattern", "static int __init", 15),
        ("warm_very_common", "int", 10),
        ("warm_single_char", "x", 10),
        ("warm_path_constraint", "printk *.c", 15),
        ("warm_dir_constraint", "mutex /kernel/", 15),
    ];

    // Warmup pass - populate mmap cache
    for (_, query, _) in &warm_queries {
        for _ in 0..3 {
            bench.run_once(query);
        }
    }

    for (name, query, iters) in &warm_queries {
        let (stats, matches, files_searched) = bench.bench_query(query, *iters);
        print_row(name, &stats, matches, files_searched, *iters);
    }

    // Bigram-related benchmarks are omitted: the bigram index is built
    // asynchronously by the picker and is exercised through picker.grep().

    // ── Fuzzy grep benchmarks ─────────────────────────────────────────────
    eprintln!("\n[4/7] Fuzzy grep warm benchmarks");
    eprintln!("  Running 3 warmup iterations, then measuring.\n");
    print_header();

    let fuzzy_bench = GrepBench::with_mode(&picker, GrepMode::Fuzzy);

    let fuzzy_queries: Vec<(&str, &str, usize)> = vec![
        ("fuzzy_exact", "mutex_lock", 15),
        ("fuzzy_typo", "mutx_lock", 15),
        ("fuzzy_camel", "InodeOps", 15),
        ("fuzzy_abbrev", "sched_rt", 15),
        ("fuzzy_short", "kfr", 15),
        ("fuzzy_common", "return", 10),
        ("fuzzy_define", "MODULE_LICENSE", 15),
        ("fuzzy_struct", "file_operations", 15),
        ("fuzzy_long", "static_int_init", 15),
        ("fuzzy_path", "printk *.c", 15),
    ];

    // Warmup
    for (_, query, _) in &fuzzy_queries {
        for _ in 0..3 {
            fuzzy_bench.run_once(query);
        }
    }

    for (name, query, iters) in &fuzzy_queries {
        let (stats, matches, files_searched) = fuzzy_bench.bench_query(query, *iters);
        print_row(name, &stats, matches, files_searched, *iters);
    }

    // ── Fuzzy incremental typing ────────────────────────────────────────
    eprintln!("\n[5/7] Fuzzy incremental typing simulation");
    eprintln!("  Simulates user typing character by character (fuzzy mode).\n");

    let fuzzy_typing_sequences: Vec<(&str, Vec<&str>)> = vec![
        (
            "mutex_lock",
            vec![
                "m",
                "mu",
                "mut",
                "mute",
                "mutex",
                "mutex_",
                "mutex_l",
                "mutex_lo",
                "mutex_loc",
                "mutex_lock",
            ],
        ),
        ("printk", vec!["p", "pr", "pri", "prin", "print", "printk"]),
        ("kfree", vec!["k", "kf", "kfr", "kfre", "kfree"]),
    ];

    for (name, sequence) in &fuzzy_typing_sequences {
        eprintln!("  Typing '{}' ({} keystrokes):", name, sequence.len());
        eprintln!(
            "    {:>16} | {:>8} | {:>6} | {:>6}",
            "Query", "Latency", "Match", "Files"
        );
        eprintln!("    {:-<16}-+-{:-<8}-+-{:-<6}-+-{:-<6}", "", "", "", "");

        for prefix in sequence {
            let (elapsed, matches, files_searched) = fuzzy_bench.run_once(prefix);
            eprintln!(
                "    {:>16} | {:>8} | {:>6} | {:>6}",
                format!("\"{}\"", prefix),
                fmt_dur(elapsed),
                matches,
                files_searched,
            );
        }
        eprintln!();
    }

    eprintln!("[6/7] Incremental typing simulation (plain text)");
    eprintln!("  Simulates user typing character by character.\n");

    let bench = GrepBench::new(&picker);
    let typing_sequences: Vec<(&str, Vec<&str>)> = vec![
        (
            "mutex_lock",
            vec![
                "m",
                "mu",
                "mut",
                "mute",
                "mutex",
                "mutex_",
                "mutex_l",
                "mutex_lo",
                "mutex_loc",
                "mutex_lock",
            ],
        ),
        ("printk", vec!["p", "pr", "pri", "prin", "print", "printk"]),
        ("inode", vec!["i", "in", "ino", "inod", "inode"]),
        ("kfree", vec!["k", "kf", "kfr", "kfre", "kfree"]),
    ];

    for (name, sequence) in &typing_sequences {
        eprintln!("  Typing '{}' ({} keystrokes):", name, sequence.len());
        eprintln!(
            "    {:>16} | {:>8} | {:>6} | {:>6}",
            "Query", "Latency", "Match", "Files"
        );
        eprintln!("    {:-<16}-+-{:-<8}-+-{:-<6}-+-{:-<6}", "", "", "", "");

        for prefix in sequence {
            let (elapsed, matches, files_searched) = bench.run_once(prefix);
            eprintln!(
                "    {:>16} | {:>8} | {:>6} | {:>6}",
                format!("\"{}\"", prefix),
                fmt_dur(elapsed),
                matches,
                files_searched,
            );
        }
        eprintln!();
    }

    eprintln!("[7/7] Pagination benchmark");
    eprintln!("  Testing page_offset performance for common query.\n");

    let pagination_query = "return";
    eprintln!("  Query: \"{}\"", pagination_query);
    eprintln!(
        "    {:>6} | {:>12} | {:>8} | {:>6} | {:>12}",
        "Page", "File offset", "Latency", "Matches", "Next offset"
    );
    eprintln!(
        "    {:-<6}-+-{:-<12}-+-{:-<8}-+-{:-<6}-+-{:-<12}",
        "", "", "", "", ""
    );

    let mut file_offset = 0usize;
    for page in 0..10 {
        let parsed = parse_grep_query(pagination_query);
        let opts = GrepSearchOptions {
            max_file_size: 10 * 1024 * 1024,
            max_matches_per_file: 200,
            smart_case: true,
            file_offset,
            page_limit: 50,
            mode: Default::default(),
            time_budget_ms: 0,
            before_context: 0,
            after_context: 0,
            classify_definitions: false,
            trim_whitespace: false,
            abort_signal: None,
        };
        let start = Instant::now();
        let result = picker.grep(&parsed, &opts);

        let elapsed = start.elapsed();
        eprintln!(
            "    {:>6} | {:>12} | {:>8} | {:>6} | {:>12}",
            page,
            file_offset,
            fmt_dur(elapsed),
            result.matches.len(),
            result.next_file_offset,
        );

        if result.next_file_offset == 0 || result.matches.is_empty() {
            eprintln!("    (no more results)");
            break;
        }
        file_offset = result.next_file_offset;
    }

    eprintln!("\n=== Summary ===");
    eprintln!("  Total indexed files: {}", files.len());
    eprintln!("  Non-binary files: {}", non_binary);
    eprintln!("  Files > 10MB (skipped): {}", large_files);

    std::thread::sleep(Duration::from_millis(100));

    eprintln!("\nDone. For perf profiling:");
    eprintln!("  perf record -g --call-graph dwarf -F 999 ./target/release/grep_profiler");
    eprintln!("  perf report --no-children");
}
