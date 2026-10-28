use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use fff_search::FileItem;
use std::hint::black_box;
use std::path::Path;
use std::time::Duration;

struct Case {
    name: &'static str,
    line: &'static str,
    ranges: Vec<(u32, u32)>,
}

fn bench_definition(c: &mut Criterion) {
    let languages = [
        (
            "rust",
            "bench.rs",
            &[
                ("fn", "fn user_file() {}", true),
                (
                    "qualified_fn",
                    "pub(crate) async fn user_file(path: &Path) -> Result<()> {",
                    true,
                ),
                ("struct", "pub struct user_file<T> {", true),
                ("enum", "pub enum user_file {", true),
                ("attribute", "#[derive(Clone)] pub struct user_file;", true),
                ("impl_reference", "impl<T> user_file<T> {", false),
                (
                    "parameter_reference",
                    "fn other(value: user_file) {}",
                    false,
                ),
                ("call_reference", "    user_file(&path);", false),
            ][..],
        ),
        (
            "javascript",
            "bench.js",
            &[
                (
                    "function",
                    "function user_file(value) { return value; }",
                    true,
                ),
                (
                    "async_function",
                    "export async function user_file(value) {",
                    true,
                ),
                ("arrow", "const user_file = (value) => value;", true),
                (
                    "async_arrow",
                    "export const user_file = async (value) => value;",
                    true,
                ),
                ("class", "class user_file extends Base {", true),
                ("method", "  user_file(value) { return value; }", true),
                (
                    "parameter_reference",
                    "function other(value = user_file()) {",
                    false,
                ),
                ("call_reference", "return user_file(value);", false),
            ][..],
        ),
        (
            "python",
            "bench.py",
            &[
                ("def", "def user_file(path):", true),
                ("async_def", "    async def user_file(self, path):", true),
                ("class", "class user_file(Base):", true),
                (
                    "annotation_reference",
                    "def other(value: user_file) -> None:",
                    false,
                ),
                ("call_reference", "    user_file(path)", false),
                (
                    "multiple_references",
                    "result = user_file(first) + user_file(second)",
                    false,
                ),
            ][..],
        ),
    ];

    for (language, filename, examples) in languages {
        let file =
            FileItem::new_from_walk_parts(Path::new(filename), Path::new("."), None, 1024, 0).0;
        let cases: Vec<_> = examples
            .iter()
            .map(|&(name, line, expected)| {
                let ranges: Vec<_> = line
                    .match_indices("user_file")
                    .map(|(start, needle)| (start as u32, (start + needle.len()) as u32))
                    .collect();
                assert!(!ranges.is_empty(), "{language}/{name}");
                assert_eq!(
                    file.is_definition_match(line, &ranges),
                    expected,
                    "{language}/{name}"
                );
                Case { name, line, ranges }
            })
            .collect();
        let mut group = c.benchmark_group(format!("definition/{language}"));
        group.throughput(Throughput::Elements(1));
        for case in &cases {
            group.bench_function(case.name, |b| {
                b.iter(|| {
                    black_box(&file)
                        .is_definition_match(black_box(case.line), black_box(&case.ranges))
                });
            });
        }

        // Shuffle once outside timing so the batch mixes declaration and reference branches.
        let mut order: Vec<_> = (0..256).map(|i| i % cases.len()).collect();
        let mut random = 0xdecafbad_u64;
        for i in (1..order.len()).rev() {
            random ^= random << 13;
            random ^= random >> 7;
            random ^= random << 17;
            order.swap(i, random as usize % (i + 1));
        }
        group.throughput(Throughput::Elements(order.len() as u64));
        group.bench_function("mixed_256", |b| {
            b.iter(|| {
                let mut definitions = 0;
                for &idx in black_box(&order) {
                    let case = &cases[idx];
                    definitions += black_box(&file)
                        .is_definition_match(black_box(case.line), black_box(&case.ranges))
                        as usize;
                }
                definitions
            });
        });
        group.throughput(Throughput::Elements(1));
        for length in [16, 32, 96] {
            let name = format!("{}user_file", "x".repeat(length - 9));
            let line = examples[0].1.replace("user_file", &name);
            let start = line.find("user_file").unwrap() as u32;
            let ranges = [(start, start + 9)];
            assert!(file.is_definition_match(&line, &ranges));
            group.bench_function(format!("identifier_{length}_bytes"), |b| {
                b.iter(|| {
                    black_box(&file).is_definition_match(black_box(&line), black_box(&ranges))
                });
            });
        }
        for length in [255, 256, 257, 1024, 16384] {
            let line = format!("{:>length$}", examples[0].1);
            let start = line.find("user_file").unwrap() as u32;
            let ranges = [(start, start + 9)];
            group.bench_function(format!("indented_{length}_bytes"), |b| {
                b.iter(|| {
                    black_box(&file).is_definition_match(black_box(&line), black_box(&ranges))
                });
            });
        }
        group.finish();
    }
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .sample_size(100)
        .warm_up_time(Duration::from_millis(300))
        .measurement_time(Duration::from_secs(1));
    targets = bench_definition
}
criterion_main!(benches);
