use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use fff_search::simd_string_utils::memmem;
use std::path::Path;

fn load_real_files() -> Vec<(&'static str, Vec<u8>)> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let default_root = manifest_dir.parent().unwrap().parent().unwrap();
    let fixture_root = std::env::var_os("FFF_BENCH_PATH");
    let repo_root = fixture_root.as_deref().map_or(default_root, Path::new);

    let files: &[(&str, &str)] = &[
        ("grep.rs/80KB", "crates/fff-core/src/grep.rs"),
        ("file_picker.rs/53KB", "crates/fff-core/src/file_picker.rs"),
        ("picker_ui.lua/96KB", "lua/fff/picker_ui.lua"),
    ];

    let mut result = Vec::new();
    for &(label, rel_path) in files {
        let full_path = repo_root.join(rel_path);
        if let Ok(data) = std::fs::read(&full_path) {
            result.push((label, data));
        }
    }

    // Also create a large synthetic file by concatenating all three
    if result.len() == 3 {
        let mut combined = Vec::new();
        for (_, data) in &result {
            combined.extend_from_slice(data);
        }
        // Repeat to get ~1MB
        let base = combined.clone();
        while combined.len() < 1024 * 1024 {
            combined.extend_from_slice(&base);
        }
        combined.truncate(1024 * 1024);
        result.push(("combined/1MB", combined));
    }

    result
}

fn bench_memmem(c: &mut Criterion) {
    let mut group = c.benchmark_group("simd_string_utils_memmem");

    let files = load_real_files();
    assert!(!files.is_empty(), "No source files found for benchmarking");

    // Needles chosen to exercise different false-positive rates:
    //
    // "hit" needles: strings that actually appear in these source files.
    // "miss" needles: strings with common first-bytes (lots of false positives
    //   for memchr2) but that don't exist in any of the files.
    let needles: &[(&str, &[u8])] = &[
        // Hits — real identifiers from the codebase
        ("short/hit/fn", b"fn"),
        ("short/hit/self", b"self"),
        ("medium/hit", b"search_file"),
        ("long/hit", b"content_cache_budget"),
        // Misses — common first-bytes, guaranteed not in source
        ("short/miss", b"zqxjv"),
        ("medium/miss", b"fluxcapacitor"),
        ("long/miss", b"quantum_entanglement_resolver"),
    ];

    for (file_label, haystack) in &files {
        for &(needle_label, needle) in needles {
            let needle_lower: Vec<u8> = needle.iter().map(|b| b.to_ascii_lowercase()).collect();
            let id = format!("{file_label}/{needle_label}");

            group.bench_with_input(
                BenchmarkId::new("find", &id),
                &(haystack, &needle_lower),
                |b, &(h, n)| {
                    b.iter(|| black_box(memmem::find(h, n)));
                },
            );
        }
    }

    group.finish();
}

criterion_group!(benches, bench_memmem);
criterion_main!(benches);
