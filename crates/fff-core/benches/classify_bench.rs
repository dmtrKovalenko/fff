use criterion::{Criterion, Throughput, black_box, criterion_group, criterion_main};
use fff_search::grep::{Lang, classify_line};

/// Every line of this repository's Rust and Lua sources: a realistic mix of
/// definitions, calls, comments and blank lines.
fn corpus() -> Vec<(Lang, String)> {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let mut lines = Vec::new();
    let mut stack = vec![
        std::path::PathBuf::from(root).join("crates"),
        std::path::PathBuf::from(root).join("lua"),
    ];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            let name = path.file_name().unwrap().to_string_lossy();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                if name != "target" && !name.starts_with('.') {
                    stack.push(path);
                }
                continue;
            }
            let lang = Lang::from_file_name(&name);
            if !matches!(lang, Lang::Rust | Lang::Lua) {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&path) {
                lines.extend(text.lines().map(|l| (lang, l.to_owned())));
            }
        }
    }
    lines
}

fn bench_classify(c: &mut Criterion) {
    let lines = corpus();
    let bytes: usize = lines.iter().map(|(_, l)| l.len()).sum();

    let mut group = c.benchmark_group("classify_line");
    group.throughput(Throughput::Elements(lines.len() as u64));
    group.bench_function(
        format!("{} lines, {} KiB", lines.len(), bytes / 1024),
        |b| {
            b.iter(|| {
                lines
                    .iter()
                    .filter(|(lang, line)| classify_line(black_box(line), *lang).is_some())
                    .count()
            });
        },
    );
    group.finish();
}

criterion_group!(benches, bench_classify);
criterion_main!(benches);
