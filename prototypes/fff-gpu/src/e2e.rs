use crate::hybrid::{top_k_matches, top_k_scores};
use fff_gpu::{GpuMatcher, Kernel};
use std::time::{Duration, Instant};

// Query string in, sorted top-50 (score, id) out. Median of `reps`.
unsafe extern "C" {
    fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: i32) -> i32;
}

pub fn run(paths: &[String], queries: &[String], threads: usize, reps: usize) {
    // QOS=1 marks this thread user-interactive so macOS keeps it on a P-core.
    if std::env::var("QOS").is_ok() {
        unsafe { pthread_set_qos_class_self_np(0x21, 0) };
    }
    let scoring = neo_frizbee::Scoring::default();
    let config = neo_frizbee::Config {
        max_typos: Some(0),
        sort: false,
        scoring,
        ..Default::default()
    };
    let gpu = GpuMatcher::new(paths, Kernel::Scalar);
    for _ in 0..10 {
        gpu.search("warmup", &scoring);
    }
    let sorted = |mut v: Vec<(u32, u32)>| {
        v.sort_by(|a, b| (b.0, a.1).cmp(&(a.0, b.1)));
        v
    };
    // GPU top-k must equal top-k over the full score readback.
    for q in queries {
        gpu.set_incremental(false);
        let full = sorted(top_k_scores(&gpu.search(q, &scoring)));
        let fast = gpu.search_topk(q, &scoring, 50).entries;
        if full != fast {
            println!(
                "TOPK MISMATCH for {q}: full {:?} vs gpu {:?}",
                &full[..5.min(full.len())],
                &fast[..5.min(fast.len())]
            );
        }
    }
    gpu.set_incremental(true);

    println!(
        "\n=== end to end, {} paths: query -> sorted top-50 ===",
        paths.len()
    );
    println!(
        "{:<16} {:>9} {:>9} {:>9} | {:>9} {:>9} {:>9} | {:>7}",
        "query", "gpu-e2e", "search", "top-k", "cpu-e2e", "match", "top-k", "speedup"
    );
    let mut typing = Vec::new();
    for q in queries {
        let g = measure(reps, || {
            let t = Instant::now();
            let _top = gpu.search_topk(q, &scoring, 50);
            (t.elapsed(), Duration::ZERO)
        });
        let c = measure(reps, || {
            let t = Instant::now();
            let m = neo_frizbee::match_list_parallel(q, paths, &config, threads);
            let t1 = t.elapsed();
            let mut top = top_k_matches(&m, 0);
            top.sort_by(|a, b| (b.0, a.1).cmp(&(a.0, b.1)));
            (t1, t.elapsed() - t1)
        });
        println!(
            "{:<16} {:>9} {:>9} {:>9} | {:>9} {:>9} {:>9} | {:>6.1}x",
            q,
            fmt(g.0 + g.1),
            fmt(g.0),
            fmt(g.1),
            fmt(c.0 + c.1),
            fmt(c.0),
            fmt(c.1),
            (c.0 + c.1).as_secs_f64() / (g.0 + g.1).as_secs_f64()
        );
        typing.push(q.clone());
    }

    // Keystroke sequence: each prefix of the query, in order, once per rep.
    // CADENCE=ms idles between keystrokes; KEEPALIVE=1 nudges the GPU while idle.
    let cadence = std::env::var("CADENCE")
        .ok()
        .and_then(|v| v.parse().ok())
        .map(Duration::from_millis);
    // KEEPALIVE=1 one-thread nudge every 2ms; KEEPALIVE=2 a full prefilter every 4ms.
    let keepalive: u32 = std::env::var("KEEPALIVE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let idle = |gpu: Option<&GpuMatcher>| {
        let Some(d) = cadence else { return };
        let until = Instant::now() + d;
        match gpu {
            Some(g) if keepalive == 1 => {
                while Instant::now() < until {
                    g.nudge();
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
            Some(g) if keepalive == 2 => {
                while Instant::now() < until {
                    g.search("~~~~", &scoring);
                    std::thread::sleep(Duration::from_millis(4));
                }
            }
            _ => std::thread::sleep(d),
        }
    };
    // TYPE=a,b overrides the words typed out keystroke by keystroke.
    let words: Vec<String> = std::env::var("TYPE")
        .map(|v| v.split(',').map(String::from).collect())
        .unwrap_or_else(|_| vec!["picker_ui".into(), "srcfilepickerrs".into()]);
    for word in words.iter().map(String::as_str) {
        println!(
            "\n--- typing \"{word}\" one keystroke at a time (median of {reps}, cadence {:?}, keepalive {keepalive}) ---",
            cadence
        );
        println!(
            "{:<16} {:>9} {:>9} {:>9}",
            "prefix", "gpu-e2e", "cpu-e2e", "speedup"
        );
        let prefixes: Vec<&str> = (1..=word.len()).map(|i| &word[..i]).collect();
        let mut gpu_t = vec![Vec::new(); prefixes.len()];
        let mut cpu_t = vec![Vec::new(); prefixes.len()];
        // Incremental prefilter results must match a from-scratch search.
        gpu.set_incremental(true);
        gpu.search("~", &scoring);
        for p in &prefixes {
            let inc = gpu.search_topk(p, &scoring, 50).entries;
            gpu.set_incremental(false);
            let full = gpu.search_topk(p, &scoring, 50).entries;
            gpu.set_incremental(true);
            if inc != full {
                println!("INCREMENTAL MISMATCH at {p}");
            }
            gpu.search_topk(p, &scoring, 50);
        }
        for _ in 0..reps {
            gpu.search("~", &scoring);
            for (i, p) in prefixes.iter().enumerate() {
                idle(Some(&gpu));
                let t = Instant::now();
                let _top = gpu.search_topk(p, &scoring, 50);
                gpu_t[i].push(t.elapsed());
            }
            for (i, p) in prefixes.iter().enumerate() {
                idle(None);
                let t = Instant::now();
                let m = neo_frizbee::match_list_parallel(p, paths, &config, threads);
                let mut top = top_k_matches(&m, 0);
                top.sort_by(|a, b| (b.0, a.1).cmp(&(a.0, b.1)));
                cpu_t[i].push(t.elapsed());
            }
        }
        for (i, p) in prefixes.iter().enumerate() {
            let g = median(&mut gpu_t[i]);
            let c = median(&mut cpu_t[i]);
            println!(
                "{:<16} {:>9} {:>9} {:>8.1}x",
                p,
                fmt(g),
                fmt(c),
                c.as_secs_f64() / g.as_secs_f64()
            );
        }
    }
}

fn measure(reps: usize, mut f: impl FnMut() -> (Duration, Duration)) -> (Duration, Duration) {
    let mut a = Vec::with_capacity(reps);
    let mut b = Vec::with_capacity(reps);
    for _ in 0..reps {
        let (x, y) = f();
        a.push(x);
        b.push(y);
    }
    (median(&mut a), median(&mut b))
}

fn median(v: &mut Vec<Duration>) -> Duration {
    v.sort();
    v[v.len() / 2]
}

fn fmt(d: Duration) -> String {
    if d < Duration::from_millis(1) {
        format!("{:.0}µs", d.as_secs_f64() * 1e6)
    } else {
        format!("{:.2}ms", d.as_secs_f64() * 1e3)
    }
}
