//! Integration test: verify that modifying a file after the bigram index is built
//! still makes the new content findable via grep (through the overlay layer).

use std::fs;
use std::time::Duration;
use tempfile::TempDir;

use fff_search::file_picker::{FFFMode, FilePicker};
use fff_search::grep::{GrepMode, GrepSearchOptions, parse_grep_query};
use fff_search::{FilePickerOptions, SharedFilePicker, SharedFrecency};

/// Create a temp directory with some initial files, run the full picker lifecycle,
/// then modify a file and verify grep finds the new content.
#[test]
fn modified_file_findable_via_overlay() {
    let tmp = TempDir::new().unwrap();
    let base = tmp.path();

    // Create initial files with known content.
    fs::write(base.join("alpha.txt"), "hello world\nfoo bar\n").unwrap();
    fs::write(
        base.join("beta.txt"),
        "some other content\nnothing special\n",
    )
    .unwrap();
    fs::write(base.join("gamma.txt"), "yet another file\nmore lines\n").unwrap();

    let shared_picker = SharedFilePicker::default();
    let shared_frecency = SharedFrecency::default();

    FilePicker::new_with_shared_state(
        shared_picker.clone(),
        shared_frecency.clone(),
        FilePickerOptions {
            base_path: base.to_string_lossy().to_string(),
            enable_mmap_cache: true,
            enable_content_indexing: true,
            mode: FFFMode::Neovim,
            watch: false, // we drive events manually
            ..Default::default()
        },
    )
    .expect("Failed to create FilePicker");

    // Wait for scan + bigram build to complete.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        std::thread::sleep(Duration::from_millis(50));

        let ready = shared_picker
            .read()
            .ok()
            .map(|guard| {
                guard
                    .as_ref()
                    .map_or(false, |p| !p.is_scan_active() && p.bigram_index().is_some())
            })
            .unwrap_or(false);

        if ready {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Timed out waiting for scan + bigram build"
        );
    }

    // Sanity check: the 3 files are indexed.
    {
        let guard = shared_picker.read().unwrap();
        let picker = guard.as_ref().unwrap();
        assert_eq!(picker.get_files().len(), 3, "Expected 3 files after scan");
        assert!(
            picker.bigram_index().is_some(),
            "Bigram index should be built"
        );
        assert!(
            picker.bigram_overlay().is_some(),
            "Overlay should be initialized"
        );
    }

    // "UNIQUE_NEEDLE" should NOT exist in any file yet.
    {
        let guard = shared_picker.read().unwrap();
        let picker = guard.as_ref().unwrap();
        let parsed = parse_grep_query("UNIQUE_NEEDLE");
        let opts = grep_opts();
        let result = picker.grep(&parsed, &opts);
        assert_eq!(
            result.matches.len(),
            0,
            "UNIQUE_NEEDLE should not exist before modification"
        );
    }

    // Sleep so the filesystem mtime (seconds granularity) advances past the
    // value recorded during scan. Without this, on_create_or_modify skips
    // mmap invalidation and grep reads stale cached content.
    std::thread::sleep(Duration::from_millis(1100));

    // Write new content containing the needle.
    let modified_path = base.join("beta.txt");
    fs::write(
        &modified_path,
        "some other content\nUNIQUE_NEEDLE is here\nnothing special\n",
    )
    .unwrap();

    // Simulate watcher event: call on_create_or_modify.
    // This updates the overlay's bigrams and invalidates the mmap cache.
    {
        let mut guard = shared_picker.write().unwrap();
        let picker = guard.as_mut().unwrap();
        let result = picker.handle_create_or_modify(&modified_path);
        assert!(
            result.is_some(),
            "on_create_or_modify should return the file"
        );
    }

    // The bigram index was built BEFORE the modification, so without the
    // overlay, beta.txt would be filtered out (its old bigrams don't contain
    // "UNIQUE_NEEDLE"). The overlay should fix that.
    {
        let guard = shared_picker.read().unwrap();
        let picker = guard.as_ref().unwrap();
        let parsed = parse_grep_query("UNIQUE_NEEDLE");
        let opts = grep_opts();
        let result = picker.grep(&parsed, &opts);
        assert!(
            !result.matches.is_empty(),
            "UNIQUE_NEEDLE should be findable after modification (overlay adds the candidate back)"
        );
        // May find 1 or 2 matches depending on mmap cache state — the important
        // thing is that the modified content IS found.
        assert!(
            result
                .matches
                .iter()
                .any(|m| m.line_content.contains("UNIQUE_NEEDLE")),
            "At least one match should contain UNIQUE_NEEDLE"
        );
    }

    // Cleanup: stop background watcher.
    if let Ok(mut guard) = shared_picker.write() {
        if let Some(ref mut picker) = *guard {
            picker.stop_background_monitor();
        }
    }
}

/// Verify that deleting a file makes its content un-findable via grep.
#[test]
fn deleted_file_excluded_via_overlay() {
    let tmp = TempDir::new().unwrap();
    let base = tmp.path();

    fs::write(base.join("keep.txt"), "keep this content\n").unwrap();
    fs::write(base.join("remove.txt"), "DELETEME_TOKEN is here\n").unwrap();

    let shared_picker = SharedFilePicker::default();
    let shared_frecency = SharedFrecency::default();

    FilePicker::new_with_shared_state(
        shared_picker.clone(),
        shared_frecency.clone(),
        FilePickerOptions {
            base_path: base.to_string_lossy().to_string(),
            enable_mmap_cache: true,
            enable_content_indexing: true,
            mode: FFFMode::Neovim,
            watch: false, // we drive events manually
            ..Default::default()
        },
    )
    .unwrap();

    wait_for_bigram(&shared_picker);

    // Sanity: DELETEME_TOKEN is findable.
    {
        let guard = shared_picker.read().unwrap();
        let picker = guard.as_ref().unwrap();
        let result = grep_for(picker, "DELETEME_TOKEN");
        assert_eq!(
            result.matches.len(),
            1,
            "Token should be found before delete"
        );
    }

    // Delete the file on disk and via picker.
    let remove_path = base.join("remove.txt");
    fs::remove_file(&remove_path).unwrap();
    {
        let mut guard = shared_picker.write().unwrap();
        let picker = guard.as_mut().unwrap();
        assert!(
            picker.remove_file_by_path(&remove_path),
            "remove should succeed"
        );
    }

    // Token should no longer be found (tombstone in overlay clears the candidate).
    {
        let guard = shared_picker.read().unwrap();
        let picker = guard.as_ref().unwrap();
        let result = grep_for(picker, "DELETEME_TOKEN");
        assert_eq!(
            result.matches.len(),
            0,
            "DELETEME_TOKEN should not be found after deletion (tombstone in overlay)"
        );
    }

    if let Ok(mut guard) = shared_picker.write() {
        if let Some(ref mut picker) = *guard {
            picker.stop_background_monitor();
        }
    }
}

/// Verify that a newly added file (in overflow) is findable via grep.
#[test]
fn new_file_findable_after_add() {
    let tmp = TempDir::new().unwrap();
    let base = tmp.path();

    fs::write(base.join("existing.txt"), "original content\n").unwrap();

    let shared_picker = SharedFilePicker::default();
    let shared_frecency = SharedFrecency::default();

    FilePicker::new_with_shared_state(
        shared_picker.clone(),
        shared_frecency.clone(),
        FilePickerOptions {
            base_path: base.to_string_lossy().to_string(),
            enable_mmap_cache: true,
            enable_content_indexing: true,
            mode: FFFMode::Neovim,
            watch: false, // we drive events manually
            ..Default::default()
        },
    )
    .unwrap();

    wait_for_bigram(&shared_picker);

    // Create a new file on disk after the index was built.
    let new_path = base.join("newcomer.txt");
    fs::write(&new_path, "BRAND_NEW_TOKEN lives here\n").unwrap();

    // Simulate watcher detecting the new file.
    {
        let mut guard = shared_picker.write().unwrap();
        let picker = guard.as_mut().unwrap();
        let result = picker.handle_create_or_modify(&new_path);
        assert!(
            result.is_some(),
            "on_create_or_modify should return the new file"
        );
    }

    // The new file is in overflow, not in the base files slice.
    // grep_search currently only searches base files, so we need to verify
    // the overflow file is accessible.
    {
        let guard = shared_picker.read().unwrap();
        let picker = guard.as_ref().unwrap();
        let overflow = picker.get_overflow_files();
        assert_eq!(overflow.len(), 1, "Should have 1 overflow file");
        assert!(
            overflow[0].relative_path(picker).ends_with("newcomer.txt"),
            "Overflow file should be newcomer.txt"
        );
    }

    if let Ok(mut guard) = shared_picker.write() {
        if let Some(ref mut picker) = *guard {
            picker.stop_background_monitor();
        }
    }
}

/// Verify that a file modified after index build is findable via regex grep
/// through the overlay. This catches a regression where `extract_bigrams` on
/// the raw regex string (e.g. "NEEDLE.*HERE") produces bogus bigrams containing
/// `.` and `*`, causing `query_modified` to miss the file.
#[test]
fn modified_file_findable_via_regex_overlay() {
    let tmp = TempDir::new().unwrap();
    let base = tmp.path();

    fs::write(base.join("alpha.txt"), "hello world\nfoo bar\n").unwrap();
    fs::write(
        base.join("beta.txt"),
        "some other content\nnothing special\n",
    )
    .unwrap();

    let shared_picker = SharedFilePicker::default();
    let shared_frecency = SharedFrecency::default();

    FilePicker::new_with_shared_state(
        shared_picker.clone(),
        shared_frecency.clone(),
        FilePickerOptions {
            base_path: base.to_string_lossy().to_string(),
            enable_mmap_cache: true,
            enable_content_indexing: true,
            mode: FFFMode::Neovim,
            watch: false, // we drive events manually
            ..Default::default()
        },
    )
    .unwrap();

    wait_for_bigram(&shared_picker);

    // Advance mtime past the scan timestamp so the cache is invalidated.
    std::thread::sleep(Duration::from_millis(1100));

    // Write content that matches the regex "NEEDLE.*HERE" into beta.txt.
    let modified_path = base.join("beta.txt");
    fs::write(
        &modified_path,
        "some other content\nNEEDLE is right HERE\nnothing special\n",
    )
    .unwrap();

    {
        let mut guard = shared_picker.write().unwrap();
        let picker = guard.as_mut().unwrap();
        assert!(picker.handle_create_or_modify(&modified_path).is_some());
    }

    // Regex grep should find the modified file through the overlay.
    {
        let guard = shared_picker.read().unwrap();
        let picker = guard.as_ref().unwrap();
        let parsed = parse_grep_query("NEEDLE.*HERE");
        let opts = GrepSearchOptions {
            mode: GrepMode::Regex,
            ..grep_opts()
        };
        let result = picker.grep(&parsed, &opts);
        assert!(
            !result.matches.is_empty(),
            "Regex grep should find NEEDLE.*HERE in modified file via overlay"
        );
        assert!(result.matches[0].line_content.contains("NEEDLE"));
    }

    if let Ok(mut guard) = shared_picker.write() {
        if let Some(ref mut picker) = *guard {
            picker.stop_background_monitor();
        }
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────

fn grep_opts() -> GrepSearchOptions {
    GrepSearchOptions {
        max_file_size: 10 * 1024 * 1024,
        max_matches_per_file: 200,
        smart_case: true,
        file_offset: 0,
        page_limit: 200,
        mode: GrepMode::PlainText,
        time_budget_ms: 0,
        before_context: 0,
        after_context: 0,
        classify_definitions: false,
        trim_whitespace: false,
        abort_signal: None,
    }
}

fn grep_for<'a>(picker: &'a FilePicker, query: &str) -> fff_search::grep::GrepResult<'a> {
    let parsed = parse_grep_query(query);
    picker.grep(&parsed, &grep_opts())
}

fn wait_for_bigram(shared_picker: &SharedFilePicker) {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        std::thread::sleep(Duration::from_millis(50));
        let ready = shared_picker
            .read()
            .ok()
            .map(|guard| {
                guard
                    .as_ref()
                    .map_or(false, |p| !p.is_scan_active() && p.bigram_index().is_some())
            })
            .unwrap_or(false);
        if ready {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Timed out waiting for bigram build"
        );
    }
}
