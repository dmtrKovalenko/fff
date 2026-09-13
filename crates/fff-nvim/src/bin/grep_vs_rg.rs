use fff::file_picker::FilePicker;
/// FFF vs ripgrep comparison benchmark
///
/// Demonstrates why a persistent in-process search engine (fff) is fundamentally
/// faster than shelling out to ripgrep on every keystroke (telescope/fzf-lua).
///
/// Each query is run N iterations to show the real-world advantage:
/// - fff: pre-indexed files + cached mmaps = near-zero overhead per search
/// - rg:  fork/exec + directory traversal + gitignore parsing + file opens per invocation
///
/// Sections:
///   1. Raw engine speed — fff count-only vs rg --count-matches (N iterations)
///   2. Full results     — fff collect-all vs rg full line output (N iterations)
///   3. First-page       — fff paginated (50 results) vs rg telescope-style
///      (spawn, stream 50 lines, kill) — the real UI scenario (N iterations)
///
/// The rg commands use telescope's default vimgrep_arguments:
///   rg --color=never --no-heading --with-filename --line-number --column --smart-case
///
/// Usage:
///   cargo build --release --bin grep_vs_rg
///   ./target/release/grep_vs_rg [--path /path/to/repo] [--iters 5]
use fff::grep::{GrepSearchOptions, parse_grep_query};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

/// Number of times each query is repeated (overridable with --iters).
const DEFAULT_ITERS: usize = 5;

fn create_picker(path: &Path) -> FilePicker {
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

/// Telescope's default vimgrep_arguments applied to any rg command.
/// Also limits rg's thread count to match rayon's pool (fair comparison).
fn apply_telescope_args(cmd: &mut Command, threads: usize) {
    cmd.arg("--color=never")
        .arg("--no-heading")
        .arg("--with-filename")
        .arg("--line-number")
        .arg("--column")
        .arg("--smart-case")
        .arg("--fixed-strings")
        .arg("--max-filesize")
        .arg("10M")
        .arg("--threads")
        .arg(threads.to_string());
}

/// Run ripgrep counting matches via --count-matches.
fn run_rg_count(
    repo_path: &Path,
    pattern: &str,
    case_insensitive: bool,
    threads: usize,
) -> (usize, Duration) {
    let start = Instant::now();
    let mut cmd = Command::new("rg");
    cmd.arg("--count-matches").arg("--no-filename");
    apply_telescope_args(&mut cmd, threads);
    if case_insensitive {
        cmd.arg("--ignore-case");
    }
    cmd.arg(pattern).current_dir(repo_path);

    let output = cmd.output().expect("Failed to run rg");
    let elapsed = start.elapsed();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let count: usize = stdout
        .lines()
        .filter_map(|l| l.trim().parse::<usize>().ok())
        .sum();
    (count, elapsed)
}

/// Run ripgrep collecting full line output.
fn run_rg_lines(
    repo_path: &Path,
    pattern: &str,
    case_insensitive: bool,
    threads: usize,
) -> (usize, Duration) {
    let start = Instant::now();
    let mut cmd = Command::new("rg");
    apply_telescope_args(&mut cmd, threads);
    if case_insensitive {
        cmd.arg("--ignore-case");
    }
    cmd.arg(pattern).current_dir(repo_path);

    let output = cmd.output().expect("Failed to run rg");
    let elapsed = start.elapsed();
    let count = bytecount(&output.stdout, b'\n');
    (count, elapsed)
}

/// Run ripgrep the way telescope/fzf-lua actually do it: spawn rg as a
/// streaming subprocess, read stdout line-by-line, and kill the process
/// after `limit` lines. This is the realistic "first page" scenario.
fn run_rg_page(
    repo_path: &Path,
    pattern: &str,
    case_insensitive: bool,
    limit: usize,
    threads: usize,
) -> (usize, Duration) {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;

    let start = Instant::now();
    let mut rg_cmd = Command::new("rg");
    apply_telescope_args(&mut rg_cmd, threads);
    if case_insensitive {
        rg_cmd.arg("--ignore-case");
    }
    rg_cmd
        .arg(pattern)
        .current_dir(repo_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    let mut child = rg_cmd.spawn().expect("Failed to spawn rg");
    let stdout = child.stdout.take().expect("Failed to get rg stdout");
    let reader = BufReader::new(stdout);

    let mut count = 0;
    for _line in reader.lines() {
        if _line.is_err() {
            break;
        }
        count += 1;
        if count >= limit {
            break;
        }
    }

    // Kill rg immediately --- this is what telescope does when the picker
    // closes or the query changes (plenary.job:shutdown).
    let _ = child.kill();
    let _ = child.wait();
    let elapsed = start.elapsed();

    (count, elapsed)
}

fn bytecount(bytes: &[u8], needle: u8) -> usize {
    bytes.iter().filter(|&&b| b == needle).count()
}

/// fff full: collects all GrepMatch structs (what the UI uses).
fn run_fff_full(picker: &FilePicker, query: &str) -> (usize, Duration) {
    let parsed = parse_grep_query(query);
    let options = GrepSearchOptions {
        max_file_size: 10 * 1024 * 1024,
        max_matches_per_file: usize::MAX,
        smart_case: true,
        file_offset: 0,
        page_limit: usize::MAX,
        mode: Default::default(),
        time_budget_ms: 0,
        before_context: 0,
        after_context: 0,
        classify_definitions: false,
        trim_whitespace: false,
        abort_signal: None,
    };
    let start = Instant::now();
    let result = picker.grep(&parsed, &options);
    let elapsed = start.elapsed();
    (result.matches.len(), elapsed)
}

/// fff paginated: first 50 results only (real UI scenario).
fn run_fff_page(picker: &FilePicker, query: &str) -> (usize, Duration) {
    let parsed = parse_grep_query(query);
    let options = GrepSearchOptions {
        max_file_size: 10 * 1024 * 1024,
        max_matches_per_file: 200,
        smart_case: true,
        file_offset: 0,
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
    let result = picker.grep(&parsed, &options);
    let elapsed = start.elapsed();
    (result.matches.len(), elapsed)
}

#[allow(dead_code)]
struct IterStats {
    min: Duration,
    avg: Duration,
    count: usize,
}

fn run_n<F: Fn() -> (usize, Duration)>(f: F, n: usize) -> IterStats {
    let mut times = Vec::with_capacity(n);
    let mut count = 0;
    for _ in 0..n {
        let (c, d) = f();
        count = c;
        times.push(d);
    }
    times.sort();
    let min = times[0];
    let avg = times.iter().sum::<Duration>() / n as u32;
    IterStats { min, avg, count }
}

fn fmt_dur(d: Duration) -> String {
    let us = d.as_micros();
    if us > 1_000_000 {
        format!("{:.2}s", d.as_secs_f64())
    } else if us > 1000 {
        format!("{:.1}ms", us as f64 / 1000.0)
    } else {
        format!("{}us", us)
    }
}

fn ratio_str(a: Duration, b: Duration) -> String {
    if a.is_zero() || b.is_zero() {
        return "-".to_string();
    }
    let r = b.as_secs_f64() / a.as_secs_f64();
    format!("{:.1}x", r)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let repo_path = if let Some(idx) = args.iter().position(|a| a == "--path") {
        args.get(idx + 1)
            .map(|s| s.as_str())
            .unwrap_or("./big-repo")
    } else {
        "./big-repo"
    };
    let iters = if let Some(idx) = args.iter().position(|a| a == "--iters") {
        args.get(idx + 1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(DEFAULT_ITERS)
    } else {
        DEFAULT_ITERS
    };

    let repo = std::path::PathBuf::from(repo_path);
    if !repo.exists() {
        eprintln!("Repository not found at: {}", repo_path);
        std::process::exit(1);
    }

    let canonical = fff::path_utils::canonicalize(&repo).expect("Failed to canonicalize path");

    let rg_version = Command::new("rg")
        .arg("--version")
        .output()
        .expect("ripgrep (rg) not found in PATH");
    let rg_ver = String::from_utf8_lossy(&rg_version.stdout);

    // Match rg's thread count to rayon's (both default to logical CPU count).
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);

    eprintln!("=== FFF vs ripgrep (telescope-style) ===");
    eprintln!("Repo:       {:?}", canonical);
    eprintln!("rg:         {}", rg_ver.lines().next().unwrap_or("?"));
    eprintln!("Threads:    {} (rg -j{} = rayon default)", threads, threads);
    eprintln!("Iterations: {} per query", iters);
    eprintln!();

    eprintln!("[1/5] Indexing files...");
    let picker = create_picker(&canonical);
    let files = picker.get_files();
    let non_binary = files.iter().filter(|f| !f.is_binary()).count();
    eprintln!("  {} files ({} searchable)\n", files.len(), non_binary);

    eprintln!("[2/5] Warming caches (fff mmap + OS page cache)...");
    for q in &["return", "mutex", "struct", "include", "if", "int"] {
        let _ = run_fff_page(&picker, q);
        let _ = run_rg_count(&canonical, q, true, threads);
    }
    eprintln!("  mmap cache: warmed\n");

    // (name, query, case_insensitive_for_rg)
    let queries: Vec<(&str, &str, bool)> = vec![
        ("single_char", "x", true),
        ("short_common", "if", true),
        ("very_common", "int", true),
        ("common_keyword", "return", true),
        ("preprocessor", "#include", true),
        ("function_call", "mutex_lock", true),
        ("multi_word", "static int __init", true),
        ("type_decl", "struct file", true),
        ("macro_define", "MODULE_LICENSE", false),
        ("kernel_api", "EXPORT_SYMBOL", false),
        ("error_path", "err = -EINVAL", false),
        ("comment_tag", "TODO", false),
        ("struct_name", "inode_operations", true),
        ("rare_symbol", "phylink_ethtool", true),
        ("long_literal", "This program is free software", true),
    ];

    eprintln!(
        "\n[4/5] Full results: fff (collect all) vs rg (full line output) ({} iters, showing min)\n",
        iters
    );
    eprintln!(
        "  {:<22} | {:>9} {:>10} | {:>9} {:>10} | {:>7}",
        "Query", "fff min", "count", "rg min", "count", "fff/rg"
    );
    eprintln!(
        "  {:-<22}-+-{:-<9}-{:-<10}-+-{:-<9}-{:-<10}-+-{:-<7}",
        "", "", "", "", "", ""
    );

    let mut fff_full_total = Duration::ZERO;
    let mut rg_full_total = Duration::ZERO;

    for (name, query, ci) in &queries {
        let q = *query;
        let ci = *ci;
        let fs = run_n(|| run_fff_full(&picker, q), iters);
        let rs = run_n(|| run_rg_lines(&canonical, q, ci, threads), iters);

        eprintln!(
            "  {:<22} | {:>9} {:>10} | {:>9} {:>10} | {:>7}",
            name,
            fmt_dur(fs.min),
            fs.count,
            fmt_dur(rs.min),
            rs.count,
            ratio_str(fs.min, rs.min),
        );

        fff_full_total += fs.min;
        rg_full_total += rs.min;
    }

    eprintln!(
        "  {:<22} | {:>9} {:>10} | {:>9} {:>10} | {:>7}",
        "TOTAL",
        fmt_dur(fff_full_total),
        "",
        fmt_dur(rg_full_total),
        "",
        ratio_str(fff_full_total, rg_full_total),
    );

    eprintln!(
        "\n[5/5] First-page latency --- the real UI scenario ({} iters, showing min)",
        iters
    );
    eprintln!("  fff: paginated search (50 matches) from warm mmap cache");
    eprintln!("  rg:  telescope-style (spawn, stream 50 lines, kill) --- per-keystroke cost\n");
    eprintln!(
        "  {:<22} | {:>9} {:>10} | {:>9} {:>10} | {:>7}",
        "Query", "fff min", "matches", "rg min", "matches", "fff/rg"
    );
    eprintln!(
        "  {:-<22}-+-{:-<9}-{:-<10}-+-{:-<9}-{:-<10}-+-{:-<7}",
        "", "", "", "", "", ""
    );

    let mut fff_page_total = Duration::ZERO;
    let mut rg_page_total = Duration::ZERO;

    for (name, query, ci) in &queries {
        let q = *query;
        let ci = *ci;
        let fs = run_n(|| run_fff_page(&picker, q), iters);
        let rs = run_n(|| run_rg_page(&canonical, q, ci, 50, threads), iters);

        eprintln!(
            "  {:<22} | {:>9} {:>10} | {:>9} {:>10} | {:>7}",
            name,
            fmt_dur(fs.min),
            fs.count,
            fmt_dur(rs.min),
            rs.count,
            ratio_str(fs.min, rs.min),
        );

        fff_page_total += fs.min;
        rg_page_total += rs.min;
    }

    eprintln!(
        "  {:<22} | {:>9} {:>10} | {:>9} {:>10} | {:>7}",
        "TOTAL",
        fmt_dur(fff_page_total),
        "",
        fmt_dur(rg_page_total),
        "",
        ratio_str(fff_page_total, rg_page_total),
    );

    eprintln!(
        "\n=== Summary (total min across all queries, {} iterations) ===\n",
        iters
    );
    eprintln!(
        "  {:>25} | {:>12} | {:>12} | {:>7}",
        "", "fff", "rg", "speedup"
    );
    eprintln!("  {:->25}-+-{:->12}-+-{:->12}-+-{:->7}", "", "", "", "");
    eprintln!(
        "  {:>25} | {:>12} | {:>12} | {:>7}",
        "full results (collect)",
        fmt_dur(fff_full_total),
        fmt_dur(rg_full_total),
        ratio_str(fff_full_total, rg_full_total),
    );
    eprintln!(
        "  {:>25} | {:>12} | {:>12} | {:>7}",
        "first-page (UI latency)",
        fmt_dur(fff_page_total),
        fmt_dur(rg_page_total),
        ratio_str(fff_page_total, rg_page_total),
    );

    eprintln!();
    eprintln!("  Note: rg cost includes fork/exec + directory traversal + gitignore parsing");
    eprintln!("  on EVERY invocation (= every keystroke in telescope/fzf-lua).");
    eprintln!("  fff pays this cost once at startup, then searches from warm cached mmaps.");
    eprintln!();
}
