use fff::file_picker::FilePicker;
use fff::{FuzzySearchOptions, PaginationArgs, QueryParser};
use std::time::{Duration, Instant};

const REPS: usize = 20;
const DEFAULT_QUERIES: &[&str] = &[
    "main",
    "score",
    "picker_ui",
    "fpr",
    "grep_bench",
    "LICENSE",
    "srcfilepickerrs",
];

fn main() {
    let mut args = std::env::args().skip(1);
    let root = args.next().unwrap_or_else(|| ".".into());
    let mut threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let mut queries: Vec<String> = DEFAULT_QUERIES.iter().map(|s| s.to_string()).collect();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--threads" => threads = args.next().unwrap().parse().unwrap(),
            "--queries" => queries = args.next().unwrap().split(',').map(String::from).collect(),
            _ => panic!("unknown arg {a}"),
        }
    }

    let canonical = fff::path_utils::canonicalize(std::path::Path::new(&root)).expect("path");
    let t = Instant::now();
    let mut picker = FilePicker::new(fff::FilePickerOptions {
        base_path: canonical.to_string_lossy().to_string(),
        enable_mmap_cache: false,
        mode: fff::FFFMode::Neovim,
        ..Default::default()
    })
    .expect("picker");
    picker.collect_files().expect("collect");
    println!(
        "indexed {} files in {:.2}s, {threads} threads",
        picker.get_files().len(),
        t.elapsed().as_secs_f64()
    );
    println!(
        "{:<16} {:>10} {:>10} {:>10} {:>10}",
        "query", "median", "min", "matched", "typos"
    );

    for q in &queries {
        let parser = QueryParser::default();
        let parsed = parser.parse(q);
        let mut times = Vec::with_capacity(REPS);
        let mut matched = 0;
        for _ in 0..REPS {
            let t = Instant::now();
            let r = picker.fuzzy_search(
                &parsed,
                None,
                FuzzySearchOptions {
                    max_threads: threads,
                    current_file: None,
                    project_path: None,
                    combo_boost_score_multiplier: 100,
                    min_combo_count: 3,
                    pagination: PaginationArgs {
                        offset: 0,
                        limit: 50,
                    },
                },
            );
            times.push(t.elapsed());
            matched = r.total_matched;
        }
        times.sort();
        println!(
            "{:<16} {:>10} {:>10} {:>10} {:>10}",
            q,
            fmt(times[REPS / 2]),
            fmt(times[0]),
            matched,
            (q.len() as u16 / 4).clamp(2, 6)
        );
    }
}

fn fmt(d: Duration) -> String {
    if d < Duration::from_millis(1) {
        format!("{:.0}µs", d.as_secs_f64() * 1e6)
    } else {
        format!("{:.2}ms", d.as_secs_f64() * 1e3)
    }
}
