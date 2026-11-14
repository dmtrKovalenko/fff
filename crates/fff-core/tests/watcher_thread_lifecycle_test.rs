#![cfg(target_os = "linux")]

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use tempfile::TempDir;

use fff_search::file_picker::{FFFMode, FilePicker};
use fff_search::{FilePickerOptions, SharedFilePicker, SharedFrecency};

/// Thread comm names Linux exposes via `/proc/self/task/*/comm` are
/// capped at `TASK_COMM_LEN - 1 = 15` bytes. Our owner thread is named
/// `"fff-watcher-owner"` (17 bytes), so what actually appears in
/// `/proc` is the 15-byte truncation below.
const WATCHER_OWNER_THREAD_NAME: &str = "fff-watcher-own";

/// Walk `/proc/self/task/*/comm` and return how many live threads
/// carry `name` as their `comm`.
fn count_live_threads_named(name: &str) -> usize {
    let Ok(dir) = fs::read_dir("/proc/self/task") else {
        return 0;
    };
    let mut count = 0usize;
    for entry in dir.flatten() {
        let comm_path = entry.path().join("comm");
        if let Ok(content) = fs::read_to_string(&comm_path)
            && content.trim_end() == name
        {
            count += 1;
        }
    }
    count
}

/// Poll until the thread count matches `expected` or we hit `timeout`.
fn wait_for_thread_count(name: &str, expected: usize, timeout: Duration) -> usize {
    let deadline = Instant::now() + timeout;
    loop {
        let count = count_live_threads_named(name);
        if count == expected {
            return count;
        }
        if Instant::now() >= deadline {
            return count;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn seed_repo(base: &std::path::Path) {
    fs::create_dir_all(base.join("src")).unwrap();
    fs::write(base.join("README.md"), "# seed\n").unwrap();
    fs::write(base.join("src/main.rs"), "fn main() {}\n").unwrap();
    fs::write(base.join("src/lib.rs"), "// lib\n").unwrap();

    let _ = std::process::Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(base)
        .output();
}

fn spawn_watched_picker(base: PathBuf) -> (SharedFilePicker, SharedFrecency) {
    let shared_picker = SharedFilePicker::default();
    let shared_frecency = SharedFrecency::default();

    FilePicker::new_with_shared_state(
        shared_picker.clone(),
        shared_frecency.clone(),
        FilePickerOptions {
            base_path: base.to_string_lossy().to_string(),
            enable_mmap_cache: false,
            enable_content_indexing: false,
            mode: FFFMode::Neovim,
            watch: true,
            ..Default::default()
        },
    )
    .expect("FilePicker::new_with_shared_state");

    assert!(
        shared_picker.wait_for_scan(Duration::from_secs(10)),
        "initial scan did not complete"
    );
    assert!(
        shared_picker.wait_for_watcher(Duration::from_secs(10)),
        "watcher did not install"
    );

    (shared_picker, shared_frecency)
}

#[test]
fn watcher_threads_do_not_leak_across_picker_lifetimes() {
    // this is needed because I run this within neovim with it's own fff owner thread lmao
    let baseline = count_live_threads_named(WATCHER_OWNER_THREAD_NAME);

    const PICKER_COUNT: usize = 4;

    let mut tmpdirs: Vec<TempDir> = (0..PICKER_COUNT)
        .map(|_| TempDir::new().expect("mktemp"))
        .collect();
    for td in &tmpdirs {
        seed_repo(td.path());
    }

    let mut pickers: Vec<(SharedFilePicker, SharedFrecency)> = tmpdirs
        .iter()
        .map(|td| spawn_watched_picker(td.path().canonicalize().expect("canonicalize tmp")))
        .collect();

    let peak = wait_for_thread_count(
        WATCHER_OWNER_THREAD_NAME,
        baseline + PICKER_COUNT,
        Duration::from_secs(5),
    );
    assert_eq!(
        peak,
        baseline + PICKER_COUNT,
        "expected {} watcher-owner threads alive (baseline {} + {} pickers), saw {}",
        baseline + PICKER_COUNT,
        baseline,
        PICKER_COUNT,
        peak,
    );

    for i in 0..PICKER_COUNT {
        let expected_remaining = baseline + PICKER_COUNT - (i + 1);
        let (sp, sf) = pickers.remove(0);
        drop(sp);
        drop(sf);
        let count = wait_for_thread_count(
            WATCHER_OWNER_THREAD_NAME,
            expected_remaining,
            Duration::from_secs(5),
        );
        assert_eq!(
            count,
            expected_remaining,
            "after dropping picker {}/{}: expected {} owner threads, saw {}",
            i + 1,
            PICKER_COUNT,
            expected_remaining,
            count,
        );
    }
    tmpdirs.clear();

    let after_stage1 = count_live_threads_named(WATCHER_OWNER_THREAD_NAME);
    assert_eq!(
        after_stage1, baseline,
        "stage 1 leaked watcher-owner threads: baseline {}, observed {}",
        baseline, after_stage1,
    );

    const ROUNDS: usize = 3;

    for round in 0..ROUNDS {
        let tmp = TempDir::new().expect("mktemp");
        seed_repo(tmp.path());
        let base = tmp.path().canonicalize().expect("canonicalize tmp");

        let (sp, sf) = spawn_watched_picker(base);

        let during = wait_for_thread_count(
            WATCHER_OWNER_THREAD_NAME,
            baseline + 1,
            Duration::from_secs(5),
        );
        assert_eq!(
            during,
            baseline + 1,
            "round {round}: expected 1 owner thread during run, saw {during} \
             (baseline {baseline})",
        );

        drop(sp);
        drop(sf);
        drop(tmp);

        let after =
            wait_for_thread_count(WATCHER_OWNER_THREAD_NAME, baseline, Duration::from_secs(5));
        assert_eq!(
            after, baseline,
            "round {round}: owner thread leaked after teardown \
             (baseline {baseline}, observed {after})",
        );
    }

    let final_count = count_live_threads_named(WATCHER_OWNER_THREAD_NAME);
    assert_eq!(
        final_count, baseline,
        "watcher-owner threads leaked past the end of the test \
         (baseline {}, final {})",
        baseline, final_count,
    );
}
