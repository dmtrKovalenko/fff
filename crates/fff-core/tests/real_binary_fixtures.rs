//! Real-world binary fixture regression.
//!
//! Reproduces the exact bug chain we hit with `codex_view` (4.5 MB ELF, no
//! extension) and `codex_view.codex` (127 KB, unknown extension): both are
//! binary by content but slip past extension-only triage, so a plain grep
//! used to surface their NUL-laden bytes as "text" matches.
//!
//! The fixtures live in `tests/fixtures/binaries/`. `MARKER` is a string that
//! is present (as raw bytes) in BOTH binaries — the test first asserts that,
//! then drops the two binaries plus a single plain-text file containing the
//! same marker into a closed temp dir and greps for it. Only the text file may
//! come back; if binary detection ever regresses, a binary file re-enters the
//! results and this test fails.

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use fff_search::file_picker::{FFFMode, FilePicker};
use fff_search::grep::{GrepMode, GrepSearchOptions, parse_grep_query};
use fff_search::{FilePickerOptions, SharedFilePicker, SharedFrecency};

const MARKER: &str = "__jai_runtime_init";

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/binaries")
}

fn plain_opts() -> GrepSearchOptions {
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

#[test]
fn real_binary_fixtures_are_detected_and_excluded_from_grep() {
    let fixtures = fixtures_dir();
    let large = fixtures.join("codex_view"); // 4.5 MB ELF, no extension (> 2 MB)
    let small = fixtures.join("codex_view.codex"); // 127 KB, unknown extension (< 2 MB)

    assert!(
        large.exists() && small.exists(),
        "missing binary fixtures in {}",
        fixtures.display()
    );

    // Both fixtures must really contain the marker bytes, otherwise the grep
    // exclusion assertion below would be vacuous.
    let large_bytes = fs::read(&large).unwrap();
    let small_bytes = fs::read(&small).unwrap();
    assert!(
        contains_subslice(&large_bytes, MARKER.as_bytes()),
        "fixture codex_view no longer contains the marker {MARKER:?}"
    );
    assert!(
        contains_subslice(&small_bytes, MARKER.as_bytes()),
        "fixture codex_view.codex no longer contains the marker {MARKER:?}"
    );
    // Sanity on the size split that drives the two distinct code paths.
    assert!(
        large_bytes.len() > 2 * 1024 * 1024,
        "codex_view must exceed the 2 MB non-indexable threshold"
    );
    assert!(
        small_bytes.len() < 2 * 1024 * 1024,
        "codex_view.codex must stay under the 2 MB bigram cap"
    );

    // Closed environment: the two real binaries + one plain-text file that
    // legitimately contains the marker.
    let tmp = tempfile::TempDir::new().unwrap();
    let base = tmp.path();
    fs::copy(&large, base.join("codex_view")).unwrap();
    fs::copy(&small, base.join("codex_view.codex")).unwrap();
    fs::write(
        base.join("marker.txt"),
        format!("the only legitimate hit lives here: {MARKER}\n"),
    )
    .unwrap();

    let shared_picker = SharedFilePicker::default();
    let shared_frecency = SharedFrecency::default();
    FilePicker::new_with_shared_state(
        shared_picker.clone(),
        shared_frecency.clone(),
        FilePickerOptions {
            base_path: base.to_string_lossy().to_string(),
            enable_mmap_cache: false,
            enable_content_indexing: true,
            mode: FFFMode::Neovim,
            watch: false,
            ..Default::default()
        },
    )
    .expect("failed to create FilePicker");

    assert!(
        shared_picker.wait_for_indexing_complete(Duration::from_secs(10)),
        "indexing/post-scan did not complete in time — binary classification may not have run yet"
    );

    let guard = shared_picker.read().unwrap();
    let picker = guard.as_ref().unwrap();

    // Both binaries must be classified binary.
    for name in ["codex_view", "codex_view.codex"] {
        let flagged = picker
            .get_files()
            .iter()
            .any(|f| f.relative_path(picker).ends_with(name) && f.is_binary());
        assert!(flagged, "{name} must be flagged is_binary");
    }

    // we need to make sure that marker.txt ONLY can match as we have to match
    // grep as binaries are excluded from the matching process
    let parsed = parse_grep_query(MARKER);
    let result = picker.grep(&parsed, &plain_opts());

    let matched: Vec<String> = result
        .files
        .iter()
        .map(|f| f.relative_path(picker))
        .collect();

    assert_eq!(
        result.files.len(),
        1,
        "exactly one file should match {MARKER:?}, got: {matched:?}"
    );
    assert!(
        matched[0].ends_with("marker.txt"),
        "the only match must be marker.txt, got {:?}",
        matched[0]
    );
}

/// Tiny substring search over raw bytes (the marker may be surrounded by NULs).
fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Deterministic regression for the Windows-CI failure where `codex_view`
/// (a >2 MB no-extension binary) was not flagged `is_binary`. Root cause was a
/// readiness-signal gap: `scanning` was cleared before `post_scan_indexing_active`
/// was set, so `wait_for_indexing_complete` could return before the binary sniff
/// ran. Uses synthetic fixtures (no repo/fixture dependency) covering both the
/// >2 MB non-indexable sniff path and the <2 MB bigram path, repeated to stress
/// the signal ordering. With the fix it must pass every iteration.
#[test]
fn binary_classification_done_before_indexing_wait_returns() {
    const ITERATIONS: usize = 8;
    // NUL bytes => `detect_binary_content` classifies as binary on every path.
    let large = vec![0u8; 3 * 1024 * 1024]; // > 2 MB -> non-indexable sniff
    let small = vec![0u8; 64 * 1024]; // < 2 MB -> bigram path

    for iteration in 0..ITERATIONS {
        let tmp = tempfile::TempDir::new().unwrap();
        let base = tmp.path();
        fs::write(base.join("large_binary_no_ext"), &large).unwrap();
        fs::write(base.join("small.unknownext"), &small).unwrap();
        fs::write(base.join("readme.txt"), "hello world\n").unwrap();

        let shared_picker = SharedFilePicker::default();
        let shared_frecency = SharedFrecency::default();
        FilePicker::new_with_shared_state(
            shared_picker.clone(),
            shared_frecency.clone(),
            FilePickerOptions {
                base_path: base.to_string_lossy().to_string(),
                enable_mmap_cache: false,
                enable_content_indexing: true,
                mode: FFFMode::Neovim,
                watch: false,
                ..Default::default()
            },
        )
        .expect("failed to create FilePicker");

        assert!(
            shared_picker.wait_for_indexing_complete(Duration::from_secs(10)),
            "iteration {iteration}: indexing/post-scan did not complete in time"
        );

        let guard = shared_picker.read().unwrap();
        let picker = guard.as_ref().unwrap();
        for name in ["large_binary_no_ext", "small.unknownext"] {
            let flagged = picker
                .get_files()
                .iter()
                .any(|f| f.relative_path(picker).ends_with(name) && f.is_binary());
            assert!(
                flagged,
                "iteration {iteration}: {name} must be flagged is_binary once \
                 wait_for_indexing_complete returns"
            );
        }
    }
}
