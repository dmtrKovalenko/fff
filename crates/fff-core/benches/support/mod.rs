use fff_search::{FilePicker, FilePickerOptions};
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
