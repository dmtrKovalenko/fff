use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use fff::query_tracker::QueryTracker;
use rand::distributions::Alphanumeric;
use rand::prelude::*;
use std::path::PathBuf;

fn generate_random_string(len: usize) -> String {
    thread_rng()
        .sample_iter(&Alphanumeric)
        .take(len)
        .map(char::from)
        .collect()
}

// Test data structure for benchmarks
struct TestQueryEntry {
    query: String,
    project_path: PathBuf,
    file_path: PathBuf,
    open_count: u32,
}

fn generate_test_data(num_entries: usize) -> Vec<TestQueryEntry> {
    let mut rng = thread_rng();
    let mut entries = Vec::with_capacity(num_entries);

    // Generate some common queries that will be reused
    let common_queries = [
        "main",
        "test",
        "config",
        "utils",
        "lib",
        "mod",
        "index",
        "init",
        "server",
        "client",
        "api",
        "service",
        "controller",
        "model",
        "view",
        "component",
        "handler",
        "middleware",
        "router",
        "database",
        "auth",
    ];

    // Generate some common project paths
    let project_paths = [
        "/home/user/project1",
        "/home/user/project2",
        "/home/user/web-app",
        "/home/user/cli-tool",
        "/home/user/library",
    ];

    for _ in 0..num_entries {
        let query = if rng.gen_bool(0.7) {
            // 70% chance to use common query
            common_queries.choose(&mut rng).unwrap().to_string()
        } else {
            // 30% chance to use random query
            generate_random_string(rng.gen_range(3..15))
        };

        let project_path = project_paths.choose(&mut rng).unwrap();
        let file_name = format!(
            "{}.{}",
            generate_random_string(rng.gen_range(5..20)),
            if rng.gen_bool(0.5) { "rs" } else { "js" }
        );
        let file_path = PathBuf::from(format!("{}/src/{}", project_path, file_name));

        let entry = TestQueryEntry {
            query,
            project_path: PathBuf::from(project_path),
            file_path,
            open_count: rng.gen_range(1..10),
        };

        entries.push(entry);
    }

    entries
}

fn setup_tracker_with_data(entries: &[TestQueryEntry]) -> (QueryTracker, PathBuf) {
    use std::time::{SystemTime, UNIX_EPOCH};
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_dir =
        std::env::temp_dir().join(format!("fff_bench_{}_{}", timestamp, rand::random::<u32>()));
    let mut tracker = QueryTracker::open(temp_dir.to_str().unwrap()).unwrap();

    // Insert all test data
    for entry in entries {
        for _ in 0..entry.open_count {
            tracker
                .track_query_completion(&entry.query, &entry.project_path, &entry.file_path)
                .unwrap();
        }
    }

    (tracker, temp_dir)
}

fn cleanup_tracker_dir(dir: PathBuf) {
    if dir.exists() {
        let _ = std::fs::remove_dir_all(dir);
    }
}

fn bench_track_query_completion(c: &mut Criterion) {
    let mut group = c.benchmark_group("track_query_completion");

    for size in &[100, 1000, 10000] {
        let entries = generate_test_data(*size);
        let (mut tracker, temp_dir) = setup_tracker_with_data(&entries[..*size / 2]); // Pre-populate with half

        group.bench_with_input(BenchmarkId::new("entries", size), size, |b, _| {
            let mut rng = thread_rng();
            b.iter(|| {
                let entry = entries.choose(&mut rng).unwrap();
                tracker
                    .track_query_completion(
                        black_box(&entry.query),
                        black_box(&entry.project_path),
                        black_box(&entry.file_path),
                    )
                    .unwrap();
            });
        });

        drop(tracker);
        cleanup_tracker_dir(temp_dir);
    }

    group.finish();
}

fn bench_realistic_workload(c: &mut Criterion) {
    let mut group = c.benchmark_group("realistic_workload");

    for size in &[1000, 10000] {
        let entries = generate_test_data(*size);
        let (mut tracker, temp_dir) = setup_tracker_with_data(&entries);

        group.bench_with_input(BenchmarkId::new("mixed_operations", size), size, |b, _| {
            let mut rng = thread_rng();
            b.iter(|| {
                let entry = entries.choose(&mut rng).unwrap();

                // Simulate realistic usage: 70% lookups, 25% tracking, 5% history
                match rng.gen_range(0..100) {
                    0..70 => {
                        // Query entry lookup (most common operation)
                        let entry_result = black_box(
                            tracker
                                .get_last_query_entry(
                                    black_box(&entry.query),
                                    black_box(&entry.project_path),
                                    3,
                                )
                                .unwrap(),
                        );
                        black_box(entry_result);
                    }
                    70..95 => {
                        // Track completion (when user opens file)
                        tracker
                            .track_query_completion(
                                black_box(&entry.query),
                                black_box(&entry.project_path),
                                black_box(&entry.file_path),
                            )
                            .unwrap();
                    }
                    95..100 => {
                        // Get historical query (least common)
                        let history = black_box(
                            tracker
                                .get_historical_query(black_box(&entry.project_path), black_box(5))
                                .unwrap(),
                        );
                        black_box(history);
                    }
                    _ => unreachable!(),
                }
            });
        });

        drop(tracker);
        cleanup_tracker_dir(temp_dir);
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_track_query_completion,
    // Commented out - methods removed/changed in refactor:
    // bench_get_query_boost,
    // bench_cleanup_old_entries,
    bench_realistic_workload
);
criterion_main!(benches);
