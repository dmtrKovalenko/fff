// Regression pinning: dropping a picker during poset scan off-lock time
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tempfile::TempDir;

use fff_search::file_picker::{FFFMode, FilePicker, FuzzySearchOptions};
use fff_search::{FilePickerOptions, QueryParser, SharedFilePicker, SharedFrecency};

fn seed_files(dir: &Path, count: usize) {
    for i in 0..count {
        let subdir = dir.join(format!("dir_{}", i / 20));
        fs::create_dir_all(&subdir).unwrap();
        fs::write(
            subdir.join(format!("file_{i}.rs")),
            format!("pub fn func_{i}() {{ /* token_{i} */ }}\n"),
        )
        .unwrap();
    }
}

fn git_init(dir: &Path) {
    let run = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
    };
    run(&["init"]);
    run(&["add", "-A"]);
    run(&["commit", "-m", "init"]);
}

fn make_picker(base: &Path) -> (SharedFilePicker, SharedFrecency) {
    let sp = SharedFilePicker::default();
    let sf = SharedFrecency::default();
    FilePicker::new_with_shared_state(
        sp.clone(),
        sf.clone(),
        FilePickerOptions {
            base_path: base.to_string_lossy().to_string(),
            enable_mmap_cache: true,
            enable_content_indexing: true,
            mode: FFFMode::Neovim,
            watch: false,
            ..Default::default()
        },
    )
    .expect("init");
    (sp, sf)
}

/// Drop picker immediately after scan starts — scan thread will find
/// the picker gone and exit cleanly.
#[test]
fn drop_picker_during_walk_no_segfault() {
    let tmp = TempDir::new().unwrap();
    seed_files(tmp.path(), 500);
    git_init(tmp.path());

    let (sp, _sf) = make_picker(tmp.path());
    // Don't wait — drop immediately while walk is likely in progress
    drop(sp);

    // If we get here without SIGSEGV, the test passes.
    std::thread::sleep(Duration::from_millis(200));
}

/// Drop picker while post-scan indexing is running. The snapshot holds
/// Arc clones that keep the buffers alive.
#[test]
fn drop_picker_during_post_scan_no_segfault() {
    let tmp = TempDir::new().unwrap();
    seed_files(tmp.path(), 500);
    git_init(tmp.path());

    let (sp, _sf) = make_picker(tmp.path());

    // Wait for walk to finish (files are searchable) but post-scan is
    // still running (bigram not yet built).
    sp.wait_for_scan(Duration::from_secs(10));

    // At this point post_scan_indexing_active is likely true.
    // Drop the picker — this releases the picker's Arc clones, but the
    // post-scan snapshot's clones keep the buffers alive.
    if let Ok(mut guard) = sp.write() {
        guard.take(); // drop the FilePicker
    }

    // Give post-scan threads time to run against the "dead" picker.
    // They must not segfault.
    std::thread::sleep(Duration::from_secs(2));
}

/// Drop picker from a second thread while the first thread is doing
/// fuzzy searches. Verifies no segfault from interleaved access.
#[test]
fn drop_picker_concurrent_with_search_no_segfault() {
    let tmp = TempDir::new().unwrap();
    seed_files(tmp.path(), 500);
    git_init(tmp.path());

    let (sp, _sf) = make_picker(tmp.path());
    sp.wait_for_scan(Duration::from_secs(10));

    let sp_clone = sp.clone();
    let running = Arc::new(AtomicBool::new(true));
    let running_clone = running.clone();

    // Searcher thread: continuously queries while the picker lives
    let searcher = std::thread::spawn(move || {
        let parser = QueryParser::default();
        while running_clone.load(Ordering::Relaxed) {
            if let Ok(guard) = sp_clone.read()
                && let Some(picker) = guard.as_ref()
            {
                let query = parser.parse("func");
                let _ = picker.fuzzy_search(&query, None, FuzzySearchOptions::default());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    });

    // Let searches run for a bit, then drop
    std::thread::sleep(Duration::from_millis(100));
    if let Ok(mut guard) = sp.write() {
        guard.take();
    }
    std::thread::sleep(Duration::from_millis(100));

    running.store(false, Ordering::Relaxed);
    searcher.join().unwrap();
}

/// Repeated init + wait + clean-drop cycle. This is the pattern that
/// SIGSEGV'd on the pre-refactor code in the benchmark.
#[test]
fn repeated_init_and_drop_no_segfault() {
    let tmp = TempDir::new().unwrap();
    seed_files(tmp.path(), 200);
    git_init(tmp.path());

    for _ in 0..5 {
        let (sp, _sf) = make_picker(tmp.path());
        sp.wait_for_scan(Duration::from_secs(10));
        sp.wait_for_indexing_complete(Duration::from_secs(30));
        if let Ok(mut guard) = sp.write()
            && let Some(mut picker) = guard.take()
        {
            picker.stop_background_monitor();
        }
    }
}
