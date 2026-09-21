mod e2e;
mod hybrid;
mod reference;

use fff_gpu as gpu;

use std::collections::HashSet;
use std::time::{Duration, Instant};

const TOP_K: usize = 50;
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
    let mut sizes: Vec<usize> = vec![0];
    let mut queries: Vec<String> = DEFAULT_QUERIES.iter().map(|s| s.to_string()).collect();
    let mut threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let mut hybrid = false;
    let mut e2e = false;
    let mut typos: u16 = 0;
    let mut kernels = vec![gpu::Kernel::Scalar];
    while let Some(a) = args.next() {
        match a.as_str() {
            "--sizes" => {
                sizes = args
                    .next()
                    .unwrap()
                    .split(',')
                    .map(|s| s.parse().unwrap())
                    .collect()
            }
            "--queries" => queries = args.next().unwrap().split(',').map(String::from).collect(),
            "--hybrid" => hybrid = true,
            "--e2e" => e2e = true,
            "--typos" => typos = args.next().unwrap().parse().unwrap(),
            "--threads" => threads = args.next().unwrap().parse().unwrap(),
            "--kernel" => {
                kernels = vec![match args.next().unwrap().as_str() {
                    "item" => gpu::Kernel::ThreadPerItem,
                    "lanes" => gpu::Kernel::LanePerThread,
                    "scalar" => gpu::Kernel::Scalar,
                    _ => gpu::Kernel::Subgroup,
                }]
            }
            _ => panic!("unknown arg {a}"),
        }
    }

    let reps: usize = std::env::var("REPS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);
    let base = walk(&root);
    if std::env::var("CMP").is_ok() {
        let scoring = neo_frizbee::Scoring::default();
        let cfg = neo_frizbee::Config {
            max_typos: Some(0),
            sort: false,
            scoring,
            ..Default::default()
        };
        for q in &queries {
            let mut shown = 0;
            for p in &base {
                let a = reference::score_scalar(q.as_bytes(), p.as_bytes(), &scoring);
                let b = reference::score(q.as_bytes(), p.as_bytes(), &scoring, 16);
                let f = neo_frizbee::match_list(q, &[p.as_str()], &cfg)
                    .first()
                    .map(|m| m.score)
                    .unwrap_or(0);
                if a != b && shown < 6 {
                    println!("{q:>8} scalar={a:<4} lanes16={b:<4} frz={f:<4} {p}");
                    shown += 1;
                }
            }
        }
        return;
    }
    if std::env::var("DBG").is_ok() {
        for q in &queries {
            debug_mismatches(q, &base, &neo_frizbee::Scoring::default());
        }
        return;
    }
    println!("walked {} paths from {root}", base.len());
    let scoring = neo_frizbee::Scoring::default();
    let config = neo_frizbee::Config {
        max_typos: Some(typos),
        sort: false,
        scoring,
        ..Default::default()
    };

    for size in sizes {
        let paths = scale(&base, size);
        if hybrid {
            hybrid::run(&paths, &queries, threads);
            continue;
        }
        if e2e {
            e2e::run(&paths, &queries, threads, reps);
            continue;
        }
        println!(
            "\n=== {} paths, avg len {:.1} ===",
            paths.len(),
            avg_len(&paths)
        );
        for &kernel in &kernels {
            let t = Instant::now();
            let gpu = gpu::GpuMatcher::new(&paths, kernel);
            println!(
                "gpu: {} | kernel {kernel:?} | lanes {} | index upload {:?}",
                gpu.adapter_name,
                gpu.lanes,
                t.elapsed()
            );
            gpu.search("warmup", &scoring);

            println!(
                "{:<16} {:>10} {:>8} {:>10} {:>10} {:>8} {:>8} {:>10}",
                "query", "gpu", "", "cpu-frz", "cpu-ref", "top50∩", "ref=gpu", "frz≈gpu"
            );
            for q in &queries {
                // Burst first so the GPU clocks up before the timed reps.
                for _ in 0..10 {
                    gpu.search(q, &scoring);
                }
                gpu.take_pass_medians();
                let ((gpu_scores, gpu_t), gpu_min) = timed_min(reps, || gpu.search(q, &scoring));
                let mut passes = gpu
                    .take_pass_medians()
                    .map(|(p, d, _)| format!("  gpu prefilter {p:.2}ms dp {d:.2}ms"))
                    .unwrap_or_default();
                if std::env::var("TOPK").is_ok() {
                    let (_, tk) = timed(reps, || gpu.search_topk(q, &scoring, 50));
                    let extra = gpu
                        .take_pass_medians()
                        .map(|(_, _, k)| format!(" (topk pass {k:.2}ms)"))
                        .unwrap_or_default();
                    passes.push_str(&format!("  topk-e2e {}{extra}", fmt(tk)));
                }
                let (frz, frz_t) = timed(reps, || {
                    neo_frizbee::match_list_parallel(q, &paths, &config, threads)
                });
                let (reference, ref_t) = timed(1, || {
                    paths
                        .iter()
                        .map(|p| {
                            if gpu.uses_scalar(q.len().min(gpu::MAX_NEEDLE)) {
                                reference::score_scalar(q.as_bytes(), p.as_bytes(), &scoring)
                            } else {
                                reference::score(
                                    q.as_bytes(),
                                    p.as_bytes(),
                                    &scoring,
                                    gpu.lanes as usize,
                                )
                            }
                        })
                        .collect::<Vec<_>>()
                });

                let ref_agree = gpu_scores
                    .iter()
                    .zip(&reference)
                    .filter(|(a, b)| a == b)
                    .count();
                if std::env::var("SHOW").is_ok() {
                    for (i, (g, r)) in gpu_scores
                        .iter()
                        .zip(&reference)
                        .enumerate()
                        .filter(|(_, (g, r))| g != r)
                        .take(5)
                    {
                        println!(
                            "  mismatch {} gpu={g:#x} ref={r} {}",
                            paths[i],
                            paths[i].len()
                        );
                    }
                }
                let mut frz_agree = 0usize;
                for m in &frz {
                    if gpu_scores[m.index as usize] == m.score as u32 {
                        frz_agree += 1;
                    }
                }
                let gpu_top: HashSet<u32> = top_k(&gpu_scores, TOP_K).into_iter().collect();
                let mut frz_sorted: Vec<_> = frz.iter().map(|m| (m.score, m.index)).collect();
                frz_sorted.sort_by(|a, b| (b.0, a.1).cmp(&(a.0, b.1)));
                let frz_top: HashSet<u32> = frz_sorted.iter().take(TOP_K).map(|m| m.1).collect();
                println!(
                    "{:<16} {:>10} {:>8} {:>10} {:>10} {:>5}/{:<2} {:>7.1}% {:>9.1}%  frz-matches {:>7}{}",
                    q,
                    fmt(gpu_t),
                    format!("min {}", fmt(gpu_min)),
                    fmt(frz_t),
                    fmt(ref_t),
                    gpu_top.intersection(&frz_top).count(),
                    TOP_K.min(frz_top.len()),
                    100.0 * ref_agree as f64 / paths.len() as f64,
                    if frz.is_empty() {
                        100.0
                    } else {
                        100.0 * frz_agree as f64 / frz.len() as f64
                    },
                    frz.len(),
                    passes,
                );
            }
        }
    }
}

fn timed<T>(reps: usize, f: impl FnMut() -> T) -> (T, Duration) {
    timed_min(reps, f).0
}

// (result, median), min
fn timed_min<T>(reps: usize, mut f: impl FnMut() -> T) -> ((T, Duration), Duration) {
    let mut times = Vec::with_capacity(reps);
    let mut out = None;
    for _ in 0..reps {
        let t = Instant::now();
        out = Some(f());
        times.push(t.elapsed());
    }
    times.sort();
    ((out.unwrap(), times[reps / 2]), times[0])
}

fn top_k(scores: &[u32], k: usize) -> Vec<u32> {
    let mut idx: Vec<u32> = (0..scores.len() as u32)
        .filter(|&i| scores[i as usize] > 0)
        .collect();
    idx.sort_by(|a, b| (scores[*b as usize], *a).cmp(&(scores[*a as usize], *b)));
    idx.truncate(k);
    idx
}

fn walk(root: &str) -> Vec<String> {
    let root_path = std::path::Path::new(root);
    ignore::WalkBuilder::new(root_path)
        .hidden(true)
        .build()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .filter_map(|e| {
            e.path()
                .strip_prefix(root_path)
                .ok()
                .map(|p| p.to_string_lossy().into_owned())
        })
        .filter(|p| p.is_ascii() && p.len() <= 1024)
        .collect()
}

fn scale(base: &[String], target: usize) -> Vec<String> {
    if target == 0 || target <= base.len() {
        return base.to_vec();
    }
    let copies = target.div_ceil(base.len());
    let mut out = Vec::with_capacity(target);
    for c in 0..copies {
        for p in base {
            if out.len() == target {
                break;
            }
            out.push(if c == 0 {
                p.clone()
            } else {
                format!("pkg{c}/{p}")
            });
        }
    }
    out
}

fn avg_len(paths: &[String]) -> f64 {
    paths.iter().map(|p| p.len()).sum::<usize>() as f64 / paths.len().max(1) as f64
}

fn fmt(d: Duration) -> String {
    if d < Duration::from_millis(1) {
        format!("{:.0}µs", d.as_secs_f64() * 1e6)
    } else {
        format!("{:.2}ms", d.as_secs_f64() * 1e3)
    }
}

#[allow(dead_code)]
pub fn debug_mismatches(q: &str, paths: &[String], scoring: &neo_frizbee::Scoring) {
    let config = neo_frizbee::Config {
        max_typos: Some(0),
        sort: false,
        scoring: *scoring,
        ..Default::default()
    };
    let mut shown = 0;
    for p in paths {
        let r = reference::score(q.as_bytes(), p.as_bytes(), scoring, 16);
        let f = neo_frizbee::match_list(q, &[p.as_str()], &config);
        let fs = f.first().map(|m| m.score as u32).unwrap_or(0);
        if r != fs && shown < 8 {
            println!("{q:>10} {p:<50} ref={r} frz={fs}");
            shown += 1;
        }
    }
}
