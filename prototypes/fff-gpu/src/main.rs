mod gpu;
mod reference;
mod subgroup;

use std::collections::HashSet;
use std::time::{Duration, Instant};

const TOP_K: usize = 50;
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
    let mut sizes: Vec<usize> = vec![0];
    let mut queries: Vec<String> = DEFAULT_QUERIES.iter().map(|s| s.to_string()).collect();
    let mut threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let mut kernels = vec![
        gpu::Kernel::ThreadPerItem,
        gpu::Kernel::LanePerThread,
        gpu::Kernel::Subgroup,
    ];
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
            "--threads" => threads = args.next().unwrap().parse().unwrap(),
            "--kernel" => {
                kernels = vec![match args.next().unwrap().as_str() {
                    "item" => gpu::Kernel::ThreadPerItem,
                    "lanes" => gpu::Kernel::LanePerThread,
                    _ => gpu::Kernel::Subgroup,
                }]
            }
            _ => panic!("unknown arg {a}"),
        }
    }

    let base = walk(&root);
    if std::env::var("DBG").is_ok() {
        for q in &queries {
            debug_mismatches(q, &base, &neo_frizbee::Scoring::default());
        }
        return;
    }
    println!("walked {} paths from {root}", base.len());
    let scoring = neo_frizbee::Scoring::default();
    let config = neo_frizbee::Config {
        max_typos: Some(0),
        sort: false,
        scoring,
        ..Default::default()
    };

    for size in sizes {
        let paths = scale(&base, size);
        println!(
            "\n=== {} paths, avg len {:.1} ===",
            paths.len(),
            avg_len(&paths)
        );
        for &kernel in &kernels {
            let t = Instant::now();
            let gpu = gpu::GpuMatcher::new(&paths, kernel);
            println!(
                "gpu: {} | kernel {kernel:?} | index upload {:?}",
                gpu.adapter_name,
                t.elapsed()
            );
            gpu.search("warmup", &scoring);

            println!(
                "{:<16} {:>10} {:>10} {:>10} {:>8} {:>8} {:>10}",
                "query", "gpu", "cpu-frz", "cpu-ref", "top50∩", "ref=gpu", "frz≈gpu"
            );
            for q in &queries {
                gpu.search(q, &scoring);
                let (gpu_scores, gpu_t) = timed(REPS, || gpu.search(q, &scoring));
                let (frz, frz_t) = timed(REPS, || {
                    neo_frizbee::match_list_parallel(q, &paths, &config, threads)
                });
                let (reference, ref_t) = timed(1, || {
                    paths
                        .iter()
                        .map(|p| reference::score(q.as_bytes(), p.as_bytes(), &scoring))
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
                    "{:<16} {:>10} {:>10} {:>10} {:>5}/{:<2} {:>7.1}% {:>9.1}%",
                    q,
                    fmt(gpu_t),
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
                );
            }
        }
    }
}

fn timed<T>(reps: usize, mut f: impl FnMut() -> T) -> (T, Duration) {
    let mut times = Vec::with_capacity(reps);
    let mut out = None;
    for _ in 0..reps {
        let t = Instant::now();
        out = Some(f());
        times.push(t.elapsed());
    }
    times.sort();
    (out.unwrap(), times[reps / 2])
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
        let r = reference::score(q.as_bytes(), p.as_bytes(), scoring);
        let f = neo_frizbee::match_list(q, &[p.as_str()], &config);
        let fs = f.first().map(|m| m.score as u32).unwrap_or(0);
        if r != fs && shown < 8 {
            println!("{q:>10} {p:<50} ref={r} frz={fs}");
            shown += 1;
        }
    }
}
