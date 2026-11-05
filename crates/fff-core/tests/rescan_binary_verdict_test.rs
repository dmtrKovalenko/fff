use std::fs;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use fff_search::file_picker::FilePicker;
use fff_search::{FilePickerOptions, SharedFilePicker, SharedFrecency};

const TIMEOUT: Duration = Duration::from_secs(10);

// Watcher event loss (e.g. macOS FSEvents kernel drops) triggers full rescans, so a rescan
// must not read every text file again just to classify binaries that did not change.
#[test]
fn rescan_rereads_only_changed_files_and_every_binary() {
    let tmp = tempfile::tempdir().unwrap();
    let base = fff_search::path_utils::canonicalize(tmp.path()).unwrap();
    // `.txt` so extension triage cannot decide the verdict
    fs::write(base.join("kept.txt"), "plain text!").unwrap();
    fs::write(base.join("grown.txt"), "plain text").unwrap();
    fs::write(base.join("healed.txt"), b"text\0binary").unwrap();

    let frecency = SharedFrecency::noop();
    let shared = SharedFilePicker::default();
    FilePicker::new_with_shared_state(
        shared.clone(),
        frecency.clone(),
        FilePickerOptions {
            base_path: base.to_string_lossy().into_owned(),
            enable_mmap_cache: false,
            enable_content_indexing: false,
            watch: false,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(shared.wait_for_scan(TIMEOUT), "scan timed out");
    assert!(shared.wait_for_indexing_complete(TIMEOUT));
    assert_eq!(is_binary(&shared, &base, "kept.txt"), Some(false));
    assert_eq!(is_binary(&shared, &base, "grown.txt"), Some(false));
    assert_eq!(is_binary(&shared, &base, "healed.txt"), Some(true));

    // Same size and mtime: a text verdict is trusted, so the NUL byte is never read.
    rewrite_keeping_mtime(&base.join("kept.txt"), b"plain\0text!");
    // A binary verdict is always checked again, so this one heals.
    rewrite_keeping_mtime(&base.join("healed.txt"), b"text binary");
    fs::write(base.join("grown.txt"), b"plain text\0grew").unwrap();

    shared.trigger_full_rescan_async(&frecency).unwrap();
    wait_until("grown.txt to be read again", || {
        is_binary(&shared, &base, "grown.txt") == Some(true)
    });
    assert!(shared.wait_for_indexing_complete(TIMEOUT));
    assert_eq!(is_binary(&shared, &base, "kept.txt"), Some(false));
    assert_eq!(is_binary(&shared, &base, "healed.txt"), Some(false));
}

fn rewrite_keeping_mtime(path: &Path, content: &[u8]) {
    let mtime: SystemTime = fs::metadata(path).unwrap().modified().unwrap();
    assert_eq!(fs::metadata(path).unwrap().len(), content.len() as u64);
    fs::write(path, content).unwrap();
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
}

fn is_binary(shared: &SharedFilePicker, base: &Path, rel: &str) -> Option<bool> {
    let guard = shared.read().unwrap();
    let picker = guard.as_ref()?;
    picker
        .get_file_by_path(base.join(rel))
        .map(|f| f.is_binary())
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(25));
    }
}
