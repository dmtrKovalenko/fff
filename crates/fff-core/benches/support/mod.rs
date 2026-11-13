use criterion::{BatchSize, BenchmarkGroup, measurement::WallTime};
use fff_search::{FFFQuery, FilePicker, FilePickerOptions, SearchBuffer};
use std::hash::{DefaultHasher, Hash, Hasher};

pub(super) fn repo_picker() -> Option<FilePicker> {
    let path = std::env::var_os("FFF_BENCH_PATH")?;
    let path = std::fs::canonicalize(path).expect("FFF_BENCH_PATH must exist");
    assert!(path.is_dir(), "FFF_BENCH_PATH must be a directory");
    eprintln!("Indexing {}", path.display());
    let mut picker = FilePicker::new(FilePickerOptions {
        base_path: path.to_str().expect("UTF-8 repository path").into(),
        enable_mmap_cache: false,
        enable_content_indexing: false,
        watch: false,
        ..Default::default()
    })
    .unwrap();
    picker.collect_files().unwrap();
    assert!(
        picker.live_file_count() > 0,
        "repository must contain files"
    );

    let mut files: Vec<_> = picker
        .get_files()
        .iter()
        .map(|file| {
            (
                file.relative_path(&picker),
                file.size,
                file.modified,
                file.git_status.map(|status| status.bits()),
                file.git_recency_score,
            )
        })
        .collect();
    files.sort_unstable();
    let mut fingerprint = DefaultHasher::new();
    files.hash(&mut fingerprint);
    eprintln!(
        "Dataset: files={} fingerprint={:016x}",
        files.len(),
        fingerprint.finish()
    );
    Some(picker)
}

pub(super) fn previous_results<P: Default>(
    group: &mut BenchmarkGroup<'_, WallTime>,
    case: &str,
    prefix: &FFFQuery<'_>,
    query: &FFFQuery<'_>,
    expected: usize,
    search: impl Fn(&FFFQuery<'_>, &mut SearchBuffer<P>, &mut SearchBuffer<P>) -> usize,
) {
    for (suffix, filled) in [("empty_previous", false), ("filled_previous", true)] {
        group.bench_function(format!("{case}/{suffix}"), |b| {
            b.iter_batched_ref(
                || {
                    let [mut previous, mut next] = Default::default();
                    // Warm both slots with the same prefix. Only the previous state differs.
                    search(prefix, &mut previous, &mut next);
                    search(prefix, &mut previous, &mut next);
                    if !filled {
                        previous = SearchBuffer::default();
                    }
                    [previous, next]
                },
                |[previous, next]| assert_eq!(search(query, previous, next), expected),
                BatchSize::PerIteration,
            )
        });
    }
}
