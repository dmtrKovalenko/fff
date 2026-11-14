use fff::file_picker::FilePicker;
use fff::grep::{GrepSearchOptions, parse_grep_query};
use fff::{
    FilePickerOptions, FuzzySearchOptions, LazySource, PaginationArgs, QueryParser,
    SharedFilePicker, SharedFrecency,
};
use std::time::{Duration, Instant};

const WARM_ROUNDS: usize = 30;

struct Sample {
    first_page: Duration,
    index_ready: Duration,
    results: usize,
    source: Option<LazySource>,
}

fn options(path: &str) -> FilePickerOptions {
    FilePickerOptions {
        base_path: path.to_string(),
        watch: false,
        ..Default::default()
    }
}

fn grep_options() -> GrepSearchOptions {
    GrepSearchOptions {
        page_limit: 50,
        ..Default::default()
    }
}

fn fuzzy_options<'a>() -> FuzzySearchOptions<'a> {
    FuzzySearchOptions {
        pagination: PaginationArgs {
            offset: 0,
            limit: 50,
        },
        ..Default::default()
    }
}

// Tighter than `wait_for_scan`, whose 10ms poll would blur the comparison.
fn wait_indexed(shared: &SharedFilePicker) {
    loop {
        let scanning = shared
            .read()
            .ok()
            .and_then(|guard| guard.as_ref().map(|picker| picker.is_scan_active()))
            .unwrap_or(false);
        if !scanning {
            return;
        }
        std::thread::sleep(Duration::from_micros(100));
    }
}

fn eager(path: &str, search: impl Fn(&FilePicker) -> usize) -> Sample {
    let shared = SharedFilePicker::default();
    let started = Instant::now();
    FilePicker::new_with_shared_state(shared.clone(), SharedFrecency::noop(), options(path))
        .expect("picker");
    wait_indexed(&shared);
    let index_ready = started.elapsed();
    let results = search(shared.read().unwrap().as_ref().unwrap());

    Sample {
        first_page: started.elapsed(),
        index_ready,
        results,
        source: None,
    }
}

fn lazy(path: &str, search: impl Fn(&SharedFilePicker) -> (usize, LazySource)) -> Sample {
    let shared = SharedFilePicker::default();
    let started = Instant::now();
    FilePicker::new_lazy_with_shared_state(shared.clone(), options(path), Default::default())
        .expect("picker");
    let (results, source) = search(&shared);
    let first_page = started.elapsed();
    wait_indexed(&shared);

    Sample {
        first_page,
        index_ready: started.elapsed(),
        results,
        source: Some(source),
    }
}

fn report(label: &str, sample: Sample) {
    println!(
        "{label:<26} first page {:>9.1?}  index ready {:>9.1?}  results {:>4}  {:?}",
        sample.first_page, sample.index_ready, sample.results, sample.source,
    );
}

fn median(mut times: Vec<Duration>) -> Duration {
    times.sort();
    times[times.len() / 2]
}

// One scenario per process: a second picker in the same process pays for
// the allocator collect of the first one, which skews the comparison.
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let scenario = args.get(1).cloned().unwrap_or_else(|| "warm".to_string());
    let path = args.get(2).cloned().unwrap_or_else(|| ".".to_string());
    let grep_text = args.get(3).cloned().unwrap_or_else(|| "struct".to_string());
    let fuzzy_text = args.get(4).cloned().unwrap_or_else(|| "main".to_string());
    let frecency = SharedFrecency::noop();
    let parser = QueryParser::default();

    let lazy_fuzzy = |budget: Option<Duration>| {
        lazy(&path, |shared| {
            let query = parser.parse(&fuzzy_text);
            shared
                .fuzzy_search_lazy(&frecency, &query, None, fuzzy_options(), budget, |page| {
                    (page.result.items.len(), page.source)
                })
                .expect("fuzzy_search_lazy")
        })
    };

    let sample = match scenario.as_str() {
        "grep-eager" => eager(&path, |picker| {
            let query = parse_grep_query(&grep_text);
            picker.grep(&query, &grep_options()).matches.len()
        }),
        "grep-lazy" => lazy(&path, |shared| {
            let query = parse_grep_query(&grep_text);
            shared
                .grep_lazy(&frecency, &query, &grep_options(), |page| {
                    (page.result.matches.len(), page.source)
                })
                .expect("grep_lazy")
        }),
        "fuzzy-eager" => eager(&path, |picker| {
            let query = parser.parse(&fuzzy_text);
            picker
                .fuzzy_search(&query, None, fuzzy_options())
                .items
                .len()
        }),
        "fuzzy-lazy" => lazy_fuzzy(None),
        "fuzzy-lazy-budget" => lazy_fuzzy(Some(Duration::from_millis(50))),
        "warm" => return warm(&path, &grep_text, &fuzzy_text),
        other => {
            eprintln!(
                "unknown scenario {other:?}: grep-eager, grep-lazy, fuzzy-eager, fuzzy-lazy, \
                 fuzzy-lazy-budget, warm"
            );
            std::process::exit(2);
        }
    };
    report(&scenario, sample);
}

// Prebuilt index: the lazy entry points against the regular ones.
fn warm(path: &str, grep_text: &str, fuzzy_text: &str) {
    let frecency = SharedFrecency::noop();
    let parser = QueryParser::default();
    let shared = SharedFilePicker::default();
    FilePicker::new_with_shared_state(shared.clone(), frecency.clone(), options(path))
        .expect("picker");
    wait_indexed(&shared);
    let grep_query = parse_grep_query(grep_text);
    let fuzzy_query = parser.parse(fuzzy_text);

    let mut direct_grep = Vec::new();
    let mut lazy_grep_warm = Vec::new();
    let mut direct_fuzzy = Vec::new();
    let mut lazy_fuzzy_warm = Vec::new();
    for _ in 0..WARM_ROUNDS {
        let started = Instant::now();
        {
            let guard = shared.read().unwrap();
            let picker = guard.as_ref().unwrap();
            std::hint::black_box(picker.grep(&grep_query, &grep_options()).matches.len());
        }
        direct_grep.push(started.elapsed());

        let started = Instant::now();
        shared
            .grep_lazy(&frecency, &grep_query, &grep_options(), |page| {
                std::hint::black_box(page.result.matches.len());
            })
            .expect("grep_lazy");
        lazy_grep_warm.push(started.elapsed());

        let started = Instant::now();
        {
            let guard = shared.read().unwrap();
            let picker = guard.as_ref().unwrap();
            let result = picker.fuzzy_search(&fuzzy_query, None, fuzzy_options());
            std::hint::black_box(result.items.len());
        }
        direct_fuzzy.push(started.elapsed());

        let started = Instant::now();
        shared
            .fuzzy_search_lazy(
                &frecency,
                &fuzzy_query,
                None,
                fuzzy_options(),
                None,
                |page| {
                    std::hint::black_box(page.result.items.len());
                },
            )
            .expect("fuzzy_search_lazy");
        lazy_fuzzy_warm.push(started.elapsed());
    }

    println!(
        "prebuilt index: grep {:?} vs grep_lazy {:?} | fuzzy_search {:?} vs fuzzy_search_lazy {:?}",
        median(direct_grep),
        median(lazy_grep_warm),
        median(direct_fuzzy),
        median(lazy_fuzzy_warm),
    );
}
