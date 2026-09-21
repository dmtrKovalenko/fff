use fff_gpu::{GpuMatcher, Kernel};
use std::collections::BinaryHeap;
use std::time::{Duration, Instant};

const TOP_K: usize = 50;
const REPS: usize = 20;

struct Hybrid<'a> {
    paths: &'a [String],
    gpu: GpuMatcher,
    split: usize,
    cpu_threads: usize,
    config: neo_frizbee::Config,
}

impl<'a> Hybrid<'a> {
    fn new(paths: &'a [String], split: usize, cpu_threads: usize) -> Self {
        let scoring = neo_frizbee::Scoring::default();
        let config = neo_frizbee::Config {
            max_typos: Some(0),
            sort: false,
            scoring,
            ..Default::default()
        };
        // Zero-length GPU partitions break bind groups; keep one dummy path.
        let gpu = GpuMatcher::new(&paths[..split.max(1)], Kernel::Scalar);
        gpu.search("warmup", &scoring);
        Self {
            paths,
            gpu,
            split,
            cpu_threads,
            config,
        }
    }

    // GPU partition runs on the calling thread (matcher is !Sync); CPU partition
    // runs in a scoped thread that fans out into frizbee's own pool.
    fn search(&self, needle: &str) -> (Vec<(u32, u32)>, Duration, Duration) {
        let cpu_paths = &self.paths[self.split..];
        let config = &self.config;
        let cpu_threads = self.cpu_threads;
        let split = self.split;
        let mut gpu_top = Vec::new();
        let mut gpu_t = Duration::ZERO;
        let mut cpu_t = Duration::ZERO;
        let mut cpu_top = Vec::new();
        std::thread::scope(|s| {
            let cpu = s.spawn(move || {
                let t = Instant::now();
                let out = if cpu_paths.is_empty() {
                    Vec::new()
                } else {
                    let m =
                        neo_frizbee::match_list_parallel(needle, cpu_paths, config, cpu_threads);
                    top_k_matches(&m, split as u32)
                };
                (out, t.elapsed())
            });
            if self.split > 0 {
                let t = Instant::now();
                let scores = self.gpu.search(needle, &self.config.scoring);
                gpu_top = top_k_scores(&scores);
                gpu_t = t.elapsed();
            }
            (cpu_top, cpu_t) = cpu.join().unwrap();
        });
        let mut merged = gpu_top;
        merged.extend(cpu_top);
        merged.sort_by(|a, b| (b.0, a.1).cmp(&(a.0, b.1)));
        merged.truncate(TOP_K);
        (merged, gpu_t, cpu_t)
    }
}

pub fn run(paths: &[String], queries: &[String], threads: usize) {
    let fractions = [0.0, 0.25, 0.4, 0.5, 0.6, 0.75, 1.0];
    println!(
        "\n=== hybrid split sweep, {} paths, cpu threads {} (gpu-only column: 16 threads unused) ===",
        paths.len(),
        threads - 1
    );
    print!("{:<16}", "query");
    for f in fractions {
        print!("{:>12}", format!("gpu {:.0}%", f * 100.0));
    }
    println!();

    let matchers: Vec<Hybrid> = fractions
        .iter()
        .map(|&f| {
            let split = (paths.len() as f64 * f) as usize;
            let cpu_threads = if f >= 1.0 { threads } else { threads - 1 };
            Hybrid::new(paths, split, cpu_threads)
        })
        .collect();
    let baseline = &matchers[0];

    let mut detail = Vec::new();
    for q in queries {
        let (expected, _, _) = baseline.search(q);
        print!("{:<16}", q);
        for m in &matchers {
            let mut times = Vec::with_capacity(REPS);
            let mut gts = Vec::new();
            let mut cts = Vec::new();
            let mut top = Vec::new();
            for _ in 0..REPS {
                let t = Instant::now();
                let (r, g, c) = m.search(q);
                times.push(t.elapsed());
                gts.push(g);
                cts.push(c);
                top = r;
            }
            times.sort();
            gts.sort();
            cts.sort();
            let ok = top == expected;
            print!("{:>11}{}", fmt(times[REPS / 2]), if ok { " " } else { "!" });
            detail.push((
                q.clone(),
                m.split,
                gts[REPS / 2],
                cts[REPS / 2],
                times[REPS / 2],
            ));
        }
        println!();
    }

    println!("\nper-partition medians (gpu wall / cpu wall / total):");
    for (q, split, g, c, t) in detail {
        println!(
            "{:<16} split {:>8}  gpu {:>8}  cpu {:>8}  total {:>8}",
            q,
            split,
            fmt(g),
            fmt(c),
            fmt(t)
        );
    }

    adaptive(paths, queries, threads);
}

// Rebalances the split after every query from the two partitions' wall times,
// simulating what a picker would do across keystrokes.
fn adaptive(paths: &[String], queries: &[String], threads: usize) {
    println!(
        "\n=== adaptive split (rebuilds GPU index per step; only the search time is measured) ==="
    );
    let mut f = 0.5f64;
    let seq: Vec<&String> = queries.iter().cycle().take(queries.len() * 3).collect();
    for q in seq {
        let split = (paths.len() as f64 * f) as usize;
        let m = Hybrid::new(paths, split, threads - 1);
        let mut times = Vec::new();
        let mut g = Duration::ZERO;
        let mut c = Duration::ZERO;
        for _ in 0..5 {
            let t = Instant::now();
            let (_, gt, ct) = m.search(q);
            times.push(t.elapsed());
            g = gt;
            c = ct;
        }
        times.sort();
        println!(
            "{:<16} gpu share {:>5.1}%  gpu {:>8}  cpu {:>8}  total {:>8}",
            q,
            f * 100.0,
            fmt(g),
            fmt(c),
            fmt(times[2])
        );
        // Per-path throughput of each side, then split so both finish together.
        let gpu_rate = split.max(1) as f64 / g.as_secs_f64().max(1e-6);
        let cpu_rate = (paths.len() - split).max(1) as f64 / c.as_secs_f64().max(1e-6);
        let target = gpu_rate / (gpu_rate + cpu_rate);
        f = (0.5 * f + 0.5 * target).clamp(0.05, 0.95);
    }
}

pub fn top_k_scores(scores: &[u32]) -> Vec<(u32, u32)> {
    let mut heap = BinaryHeap::with_capacity(TOP_K + 1);
    for (i, &s) in scores.iter().enumerate() {
        if s == 0 {
            continue;
        }
        let key = (s, u32::MAX - i as u32);
        if heap.len() < TOP_K {
            heap.push(std::cmp::Reverse(key));
        } else if key > heap.peek().unwrap().0 {
            heap.pop();
            heap.push(std::cmp::Reverse(key));
        }
    }
    heap.into_iter()
        .map(|std::cmp::Reverse((s, i))| (s, u32::MAX - i))
        .collect()
}

pub fn top_k_matches(m: &[neo_frizbee::Match], offset: u32) -> Vec<(u32, u32)> {
    let mut heap = BinaryHeap::with_capacity(TOP_K + 1);
    for x in m {
        let key = (x.score as u32, u32::MAX - (x.index + offset));
        if heap.len() < TOP_K {
            heap.push(std::cmp::Reverse(key));
        } else if key > heap.peek().unwrap().0 {
            heap.pop();
            heap.push(std::cmp::Reverse(key));
        }
    }
    heap.into_iter()
        .map(|std::cmp::Reverse((s, i))| (s, u32::MAX - i))
        .collect()
}

fn fmt(d: Duration) -> String {
    if d < Duration::from_millis(1) {
        format!("{:.0}µs", d.as_secs_f64() * 1e6)
    } else {
        format!("{:.2}ms", d.as_secs_f64() * 1e3)
    }
}
