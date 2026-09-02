//! Integration test: verifying that the background watcher dynamically detects
//! newly created directories and picks up files written inside them.
//!
//! This covers the NonRecursive watching behavior where:
//!   1. The watcher starts with watches on directories discovered during the
//!      initial scan.
//!   2. A brand-new subdirectory is created at runtime (after the scan).
//!   3. The watcher's event handler detects the directory Create event,
//!      collects it, and sends it to the owner thread via `watch_tx`.
//!   4. The owner thread adds a NonRecursive watch on the new directory and
//!      walks its subtree (`index_new_directory`) to inject files that
//!      already exist (race-window + burst/mv-in coverage) and to watch
//!      nested subdirectories.
//!   5. Files created *after* the watch is established are picked up via
//!      normal event delivery.
//!
//! The test uses the real `BackgroundWatcher` (via `watch: true`) and polls
//! the picker until the expected files appear or a timeout expires.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};
use tempfile::TempDir;

use fff_search::file_picker::{FFFMode, FilePicker};
use fff_search::grep::{GrepMode, GrepSearchOptions, parse_grep_query};
use fff_search::{
    FilePickerOptions, PaginationArgs, QueryParser, SharedFilePicker, SharedFrecency,
};

// ═══════════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════════

fn git_run(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@test.com")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .output()
        .unwrap_or_else(|e| panic!("git {:?} failed: {}", args, e));
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

fn git_init_and_commit(dir: &Path) {
    git_run(dir, &["init", "-b", "main"]);
    git_run(dir, &["add", "-A"]);
    git_run(dir, &["commit", "-m", "initial"]);
}

fn make_watched_picker(base: &Path) -> (SharedFilePicker, SharedFrecency) {
    let shared_picker = SharedFilePicker::default();
    let shared_frecency = SharedFrecency::noop();

    FilePicker::new_with_shared_state(
        shared_picker.clone(),
        shared_frecency.clone(),
        FilePickerOptions {
            base_path: base.to_string_lossy().to_string(),
            enable_mmap_cache: false,
            mode: FFFMode::Neovim,
            watch: true,
            ..Default::default()
        },
    )
    .expect("Failed to create FilePicker");

    (shared_picker, shared_frecency)
}

/// Wait for the initial scan + watcher to be fully ready.
fn wait_ready(shared_picker: &SharedFilePicker) {
    assert!(
        shared_picker.wait_for_scan(Duration::from_secs(30)),
        "Timed out waiting for initial scan"
    );
    assert!(
        shared_picker.wait_for_watcher(Duration::from_secs(30)),
        "Timed out waiting for watcher"
    );
}

/// Poll the picker until `predicate` returns true or timeout expires.
/// Returns the elapsed duration if successful, panics on timeout.
fn poll_until(
    shared_picker: &SharedFilePicker,
    timeout: Duration,
    description: &str,
    predicate: impl Fn(&FilePicker) -> bool,
) -> Duration {
    let start = Instant::now();
    loop {
        {
            let guard = shared_picker.read().unwrap();
            if let Some(ref picker) = *guard {
                if predicate(picker) {
                    return start.elapsed();
                }
            }
        }
        if start.elapsed() >= timeout {
            // One final attempt to give a useful error message.
            let guard = shared_picker.read().unwrap();
            let picker = guard.as_ref().unwrap();
            let file_count = picker.get_files().len();
            let paths: Vec<String> = picker
                .get_files()
                .iter()
                .map(|f| f.relative_path(picker))
                .collect();
            panic!(
                "Timed out after {:?} waiting for: {}\n\
                 Current file count: {}\n\
                 Current files: {:?}",
                timeout, description, file_count, paths
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn grep_plain_count(picker: &FilePicker, query: &str) -> usize {
    let parsed = parse_grep_query(query);
    let opts = GrepSearchOptions {
        max_file_size: 10 * 1024 * 1024,
        max_matches_per_file: 200,
        smart_case: true,
        file_offset: 0,
        page_limit: 500,
        mode: GrepMode::PlainText,
        time_budget_ms: 0,
        enforce_time_budget: false,
        before_context: 0,
        after_context: 0,
        classify_definitions: false,
        trim_whitespace: false,
        abort_signal: None,
    };
    picker.grep(&parsed, &opts).matches.len()
}

fn fuzzy_search_paths(picker: &FilePicker, query: &str) -> Vec<String> {
    let parser = QueryParser::default();
    let parsed = parser.parse(query);
    let result = picker.fuzzy_search(
        &parsed,
        None,
        fff_search::FuzzySearchOptions {
            max_threads: 1,
            pagination: PaginationArgs {
                offset: 0,
                limit: 200,
            },
            ..Default::default()
        },
    );
    result
        .items
        .iter()
        .map(|f| f.relative_path(picker))
        .collect()
}

/// Debounce timeout in the watcher is 250ms. Events need to propagate through
/// the debouncer, the owner thread park loop (1s), and the picker write lock.
/// We use a generous timeout for CI environments.
const WATCHER_TIMEOUT: Duration = Duration::from_secs(10);

// ═══════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════

/// Create a new directory and immediately write a file inside it.
/// The file is written before the watch is registered, so the flat
/// inject_existing_files scan in the owner thread must catch it.
#[test]
fn new_directory_and_file_detected_by_watcher() {
    let tmp = TempDir::new().unwrap();
    let base = tmp.path().canonicalize().unwrap();

    // Seed the repo with some initial files so the scan has something.
    fs::create_dir_all(base.join("src")).unwrap();
    fs::write(
        base.join("src/main.rs"),
        "fn main() { println!(\"INITIAL_MARKER\"); }\n",
    )
    .unwrap();
    fs::write(base.join("README.md"), "# Test project\n").unwrap();

    git_init_and_commit(&base);

    let (shared_picker, _frecency) = make_watched_picker(&base);
    wait_ready(&shared_picker);

    // Sanity: initial file is indexed.
    poll_until(
        &shared_picker,
        Duration::from_secs(5),
        "initial file src/main.rs indexed",
        |picker| {
            picker
                .get_files()
                .iter()
                .any(|f| f.relative_path(picker).contains("main.rs"))
        },
    );

    // Create a new directory and write a file into it immediately.
    // The file exists before the watch is registered — inject_existing_files
    // in the owner thread catches it via a flat read_dir.
    let new_dir = base.join("src/components");
    fs::create_dir_all(&new_dir).unwrap();
    fs::write(
        new_dir.join("button.rs"),
        "pub struct Button;\nconst TOKEN: &str = \"NEW_DIR_BUTTON_TOKEN\";\n",
    )
    .unwrap();

    // Wait for the watcher to detect the new directory + file.
    let elapsed = poll_until(
        &shared_picker,
        WATCHER_TIMEOUT,
        "file src/components/button.rs in new directory",
        |picker| {
            picker
                .get_files()
                .iter()
                .any(|f| f.relative_path(picker).contains("button.rs"))
        },
    );
    eprintln!(
        "  New directory + file detected in {:.0}ms",
        elapsed.as_secs_f64() * 1000.0
    );

    // Also verify via grep that the content is accessible.
    poll_until(
        &shared_picker,
        WATCHER_TIMEOUT,
        "grep finds NEW_DIR_BUTTON_TOKEN",
        |picker| grep_plain_count(picker, "NEW_DIR_BUTTON_TOKEN") >= 1,
    );

    // And via fuzzy search.
    poll_until(
        &shared_picker,
        WATCHER_TIMEOUT,
        "fuzzy search finds button.rs",
        |picker| {
            let results = fuzzy_search_paths(picker, "button");
            results.iter().any(|p| p.contains("button.rs"))
        },
    );
}

/// Create a new directory, then create files AFTER a delay to ensure the
/// watch was established on the directory.
#[test]
fn file_created_after_directory_watch_established() {
    let tmp = TempDir::new().unwrap();
    let base = tmp.path().canonicalize().unwrap();

    fs::create_dir_all(base.join("lib")).unwrap();
    fs::write(base.join("lib/utils.rs"), "pub fn helper() {}\n").unwrap();

    git_init_and_commit(&base);

    let (shared_picker, _frecency) = make_watched_picker(&base);
    wait_ready(&shared_picker);

    // Create the directory first, wait for the watcher to register it.
    let new_dir = base.join("lib/models");
    fs::create_dir(&new_dir).unwrap();

    // Wait long enough for the debouncer to flush + owner thread to add watch.
    std::thread::sleep(Duration::from_millis(2000));

    // Now write a file into the already-watched directory.
    fs::write(
        new_dir.join("user.rs"),
        "pub struct User { name: String }\nconst TOKEN: &str = \"POST_WATCH_USER_TOKEN\";\n",
    )
    .unwrap();

    let elapsed = poll_until(
        &shared_picker,
        WATCHER_TIMEOUT,
        "file lib/models/user.rs created after directory watch",
        |picker| {
            picker
                .get_files()
                .iter()
                .any(|f| f.relative_path(picker).contains("user.rs"))
        },
    );
    eprintln!(
        "  Post-watch file detected in {:.0}ms",
        elapsed.as_secs_f64() * 1000.0
    );

    // Grep sanity.
    poll_until(
        &shared_picker,
        WATCHER_TIMEOUT,
        "grep finds POST_WATCH_USER_TOKEN",
        |picker| grep_plain_count(picker, "POST_WATCH_USER_TOKEN") >= 1,
    );
}

/// Create a deeply nested directory tree all at once with create_dir_all
/// and write a file at the leaf. The watcher must detect the top-level
/// directory via the parent's watch, inject_existing_files finds the file
/// at the leaf (and intermediate dirs get their own watches from Create
/// events on subsequent levels).
#[test]
fn deeply_nested_new_directories_detected() {
    let tmp = TempDir::new().unwrap();
    let base = tmp.path().canonicalize().unwrap();

    fs::write(base.join("root.txt"), "root file\n").unwrap();

    git_init_and_commit(&base);

    let (shared_picker, _frecency) = make_watched_picker(&base);
    wait_ready(&shared_picker);

    // Create each level one at a time, waiting for each watch to register.
    // inject_existing_files is flat (non-recursive), so deeply nested dirs
    // need each parent to be watched before we can see files at the leaf.
    fs::create_dir(base.join("app")).unwrap();
    std::thread::sleep(Duration::from_millis(2000));

    fs::create_dir(base.join("app/services")).unwrap();
    std::thread::sleep(Duration::from_millis(2000));

    fs::create_dir(base.join("app/services/auth")).unwrap();
    // Write the file immediately — inject_existing_files catches it.
    fs::write(
        base.join("app/services/auth/jwt.rs"),
        "pub fn verify_token() {}\nconst TOKEN: &str = \"DEEP_NESTED_JWT_TOKEN\";\n",
    )
    .unwrap();

    let elapsed = poll_until(
        &shared_picker,
        WATCHER_TIMEOUT,
        "deeply nested file app/services/auth/jwt.rs",
        |picker| {
            picker
                .get_files()
                .iter()
                .any(|f| f.relative_path(picker).contains("jwt.rs"))
        },
    );
    eprintln!(
        "  Deeply nested file detected in {:.0}ms",
        elapsed.as_secs_f64() * 1000.0
    );

    // Verify content is grepable.
    poll_until(
        &shared_picker,
        WATCHER_TIMEOUT,
        "grep finds DEEP_NESTED_JWT_TOKEN",
        |picker| grep_plain_count(picker, "DEEP_NESTED_JWT_TOKEN") >= 1,
    );

    // Now create a sibling at the same depth — the parent (app/services)
    // is already watched, so this just needs the flat inject.
    let sibling_dir = base.join("app/services/database");
    fs::create_dir(&sibling_dir).unwrap();
    fs::write(
        sibling_dir.join("pool.rs"),
        "pub struct ConnectionPool;\nconst TOKEN: &str = \"SIBLING_POOL_TOKEN\";\n",
    )
    .unwrap();

    let elapsed = poll_until(
        &shared_picker,
        WATCHER_TIMEOUT,
        "sibling nested file app/services/database/pool.rs",
        |picker| {
            picker
                .get_files()
                .iter()
                .any(|f| f.relative_path(picker).contains("pool.rs"))
        },
    );
    eprintln!(
        "  Sibling nested file detected in {:.0}ms",
        elapsed.as_secs_f64() * 1000.0
    );

    poll_until(
        &shared_picker,
        WATCHER_TIMEOUT,
        "grep finds SIBLING_POOL_TOKEN",
        |picker| grep_plain_count(picker, "SIBLING_POOL_TOKEN") >= 1,
    );
}

/// Create a new directory and immediately burst-write multiple files.
/// inject_existing_files catches all of them in one flat read_dir.
#[test]
fn burst_file_creation_in_new_directory() {
    let tmp = TempDir::new().unwrap();
    let base = tmp.path().canonicalize().unwrap();

    fs::create_dir_all(base.join("src")).unwrap();
    fs::write(base.join("src/lib.rs"), "// lib\n").unwrap();

    git_init_and_commit(&base);

    let (shared_picker, _frecency) = make_watched_picker(&base);
    wait_ready(&shared_picker);

    // Create a new directory and immediately write 5 files.
    let batch_dir = base.join("src/batch");
    fs::create_dir(&batch_dir).unwrap();

    let file_count = 5;
    for i in 0..file_count {
        fs::write(
            batch_dir.join(format!("item_{i}.rs")),
            format!("pub struct Item{i};\nconst TOKEN: &str = \"BATCH_ITEM_{i}\";\n"),
        )
        .unwrap();
    }

    // Wait for ALL files to appear.
    let elapsed = poll_until(
        &shared_picker,
        WATCHER_TIMEOUT,
        &format!("all {file_count} batch files in src/batch/"),
        |picker| {
            let batch_count = picker
                .get_files()
                .iter()
                .filter(|f| {
                    let p = f.relative_path(picker);
                    p.starts_with("src/batch/") || p.starts_with("src\\batch\\")
                })
                .count();
            batch_count >= file_count
        },
    );
    eprintln!(
        "  All {} burst files detected in {:.0}ms",
        file_count,
        elapsed.as_secs_f64() * 1000.0
    );

    // Verify each file's content is grepable.
    for i in 0..file_count {
        let token = format!("BATCH_ITEM_{i}");
        poll_until(
            &shared_picker,
            WATCHER_TIMEOUT,
            &format!("grep finds {token}"),
            |picker| grep_plain_count(picker, &token) >= 1,
        );
    }
}

/// bug pinning #725: a directory that already exists but is EMPTY at
/// initial scan time is absent from `sync_data.dirs` and missing watch events
#[test]
fn file_created_in_preexisting_empty_directory() {
    let tmp = TempDir::new().unwrap();
    let base = tmp.path().canonicalize().unwrap();

    // `commands/` is empty during the initial scan — only `init.lua` is indexed.
    fs::create_dir_all(base.join("commands")).unwrap();
    fs::write(base.join("init.lua"), "-- init\n").unwrap();

    let (shared_picker, _frecency) = make_watched_picker(&base);
    wait_ready(&shared_picker);

    // Now write a file into the directory that was empty at scan time.
    fs::write(
        base.join("commands/review.md"),
        "# Review\nEMPTY_DIR_REVIEW_TOKEN\n",
    )
    .unwrap();

    let elapsed = poll_until(
        &shared_picker,
        WATCHER_TIMEOUT,
        "file commands/review.md created in a pre-existing empty directory",
        |picker| {
            picker
                .get_files()
                .iter()
                .any(|f| f.relative_path(picker).contains("review.md"))
        },
    );
    eprintln!(
        "  File in pre-existing empty directory detected in {:.0}ms",
        elapsed.as_secs_f64() * 1000.0
    );
}

/// Same as above but with a nested chain of empty directories under an
/// indexed one: every level of the empty subtree must be watched.
#[test]
fn file_created_in_nested_preexisting_empty_directories() {
    let tmp = TempDir::new().unwrap();
    let base = tmp.path().canonicalize().unwrap();

    // `src/` is indexed (has a file); `src/plugins/extra/` is an empty chain.
    fs::create_dir_all(base.join("src/plugins/extra")).unwrap();
    fs::write(base.join("src/main.rs"), "fn main() {}\n").unwrap();

    git_init_and_commit(&base);

    let (shared_picker, _frecency) = make_watched_picker(&base);
    wait_ready(&shared_picker);

    fs::write(
        base.join("src/plugins/extra/loader.rs"),
        "pub fn load() {}\nconst TOKEN: &str = \"NESTED_EMPTY_DIR_TOKEN\";\n",
    )
    .unwrap();

    let elapsed = poll_until(
        &shared_picker,
        WATCHER_TIMEOUT,
        "file src/plugins/extra/loader.rs created in nested empty directories",
        |picker| {
            picker
                .get_files()
                .iter()
                .any(|f| f.relative_path(picker).contains("loader.rs"))
        },
    );
    eprintln!(
        "  File in nested empty directories detected in {:.0}ms",
        elapsed.as_secs_f64() * 1000.0
    );

    poll_until(
        &shared_picker,
        WATCHER_TIMEOUT,
        "grep finds NESTED_EMPTY_DIR_TOKEN",
        |picker| grep_plain_count(picker, "NESTED_EMPTY_DIR_TOKEN") >= 1,
    );
}

#[test]
fn nested_tree_created_in_one_burst_detected() {
    let tmp = TempDir::new().unwrap();
    let base = tmp.path().canonicalize().unwrap();

    fs::write(base.join("root.txt"), "root file\n").unwrap();
    git_init_and_commit(&base);

    let (shared_picker, _frecency) = make_watched_picker(&base);
    wait_ready(&shared_picker);

    // No sleeps between levels: the watcher sees one Create for `pkg` and
    // must index the whole subtree from it.
    fs::create_dir_all(base.join("pkg/src/nested")).unwrap();
    fs::write(base.join("pkg/Cargo.toml"), "[package]\n").unwrap();
    fs::write(
        base.join("pkg/src/lib.rs"),
        "const TOKEN: &str = \"BURST_TREE_LIB_TOKEN\";\n",
    )
    .unwrap();
    fs::write(
        base.join("pkg/src/nested/deep.rs"),
        "const TOKEN: &str = \"BURST_TREE_DEEP_TOKEN\";\n",
    )
    .unwrap();

    for rel in ["pkg/Cargo.toml", "pkg/src/lib.rs", "pkg/src/nested/deep.rs"] {
        let elapsed = poll_until(
            &shared_picker,
            WATCHER_TIMEOUT,
            &format!("burst-created file {rel}"),
            |picker| {
                picker
                    .get_files()
                    .iter()
                    .any(|f| f.relative_path(picker) == rel)
            },
        );
        eprintln!(
            "  Burst file {rel} detected in {:.0}ms",
            elapsed.as_secs_f64() * 1000.0
        );
    }

    // Files created later at the deepest level need the nested watches too.
    fs::write(
        base.join("pkg/src/nested/late.rs"),
        "const TOKEN: &str = \"BURST_TREE_LATE_TOKEN\";\n",
    )
    .unwrap();
    poll_until(
        &shared_picker,
        WATCHER_TIMEOUT,
        "late file in burst-created nested dir",
        |picker| {
            picker
                .get_files()
                .iter()
                .any(|f| f.relative_path(picker).ends_with("late.rs"))
        },
    );

    poll_until(
        &shared_picker,
        WATCHER_TIMEOUT,
        "grep finds BURST_TREE_DEEP_TOKEN",
        |picker| grep_plain_count(picker, "BURST_TREE_DEEP_TOKEN") >= 1,
    );
}

/// Verify that gitignored directories created at runtime are NOT watched
/// and their files do NOT appear in the index.
#[test]
fn gitignored_new_directory_excluded() {
    let tmp = TempDir::new().unwrap();
    let base = tmp.path().canonicalize().unwrap();

    fs::write(base.join("main.rs"), "fn main() {}\n").unwrap();
    // Ignore the build/ directory.
    fs::write(base.join(".gitignore"), "build/\n").unwrap();

    git_init_and_commit(&base);

    let (shared_picker, _frecency) = make_watched_picker(&base);
    wait_ready(&shared_picker);

    // Create a gitignored directory with files.
    let ignored_dir = base.join("build");
    fs::create_dir(&ignored_dir).unwrap();
    fs::write(
        ignored_dir.join("output.rs"),
        "const TOKEN: &str = \"IGNORED_BUILD_TOKEN\";\n",
    )
    .unwrap();

    // Also create a non-ignored directory to confirm the watcher works.
    let good_dir = base.join("src");
    fs::create_dir(&good_dir).unwrap();
    fs::write(
        good_dir.join("app.rs"),
        "const TOKEN: &str = \"GOOD_SRC_TOKEN\";\n",
    )
    .unwrap();

    // Wait for the non-ignored file to appear (proves watcher is working).
    poll_until(
        &shared_picker,
        WATCHER_TIMEOUT,
        "non-ignored file src/app.rs appears",
        |picker| {
            picker
                .get_files()
                .iter()
                .any(|f| f.relative_path(picker).contains("app.rs"))
        },
    );

    // Give extra time for any straggler events from the ignored dir.
    std::thread::sleep(Duration::from_secs(2));

    // The gitignored file must NOT be in the index.
    {
        let guard = shared_picker.read().unwrap();
        let picker = guard.as_ref().unwrap();

        let has_ignored = picker
            .get_files()
            .iter()
            .any(|f| f.relative_path(picker).contains("output.rs"));
        assert!(
            !has_ignored,
            "Gitignored file build/output.rs should NOT be in the index"
        );

        let grep_count = grep_plain_count(picker, "IGNORED_BUILD_TOKEN");
        assert_eq!(grep_count, 0, "Gitignored content should NOT be grepable");
    }
}
