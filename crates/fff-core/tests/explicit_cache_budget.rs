//! Regression test for https://github.com/dmtrKovalenko/fff/issues/847
//!
//! An explicit `--max-cached-files` cap must reach the picker verbatim and
//! survive the initial scan, which otherwise auto-sizes the budget.

use std::fs;

use fff_search::file_picker::FilePicker;
use fff_search::{ContentCacheBudget, FilePickerOptions};
use tempfile::TempDir;

#[test]
fn explicit_cap_reaches_the_picker_and_survives_the_scan() {
    let dir = TempDir::new().unwrap();
    for i in 0..8 {
        fs::write(dir.path().join(format!("f{i}.txt")), "x".repeat(32 * 1024)).unwrap();
    }

    let mut picker = FilePicker::new(FilePickerOptions {
        base_path: dir.path().to_string_lossy().to_string(),
        watch: false,
        cache_budget: Some(ContentCacheBudget::with_max_files(2)),
        ..Default::default()
    })
    .expect("failed to create FilePicker");

    assert!(picker.has_explicit_cache_budget());
    assert_eq!(picker.cache_budget().max_files, 2);

    picker.collect_files().expect("failed to collect files");

    // 8 files would otherwise bucket into the 30_000 heuristic
    assert_eq!(picker.cache_budget().max_files, 2);
    assert_eq!(
        picker.cache_budget().max_file_size,
        ContentCacheBudget::default().max_file_size
    );
}

#[test]
fn zero_cap_keeps_the_budget_exhausted_after_the_scan() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("a.txt"), "x".repeat(32 * 1024)).unwrap();

    let mut picker = FilePicker::new(FilePickerOptions {
        base_path: dir.path().to_string_lossy().to_string(),
        watch: false,
        cache_budget: Some(ContentCacheBudget::with_max_files(0)),
        ..Default::default()
    })
    .expect("failed to create FilePicker");
    picker.collect_files().expect("failed to collect files");

    assert_eq!(picker.cache_budget().max_files, 0);
    assert!(picker.cache_budget().is_exhausted());
}
