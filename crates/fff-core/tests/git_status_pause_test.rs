use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use fff_search::file_picker::FilePicker;
use fff_search::{FilePickerOptions, SharedFilePicker, SharedFrecency};
use git2::Status;
use tempfile::TempDir;

const TIMEOUT: Duration = Duration::from_secs(30);

#[test]
fn paused_git_status_ignores_watcher_events_until_resume() {
    let (_tmp, base) = init_repo();
    fs::write(base.join("tracked.rs"), "1").unwrap();
    git(&base, &["add", "-A"]);
    git(&base, &["commit", "-m", "init", "--no-gpg-sign"]);

    let shared = start_picker(&base, true);
    assert!(shared.wait_for_watcher(TIMEOUT), "watcher timed out");
    assert!(
        shared.wait_for_git_status(TIMEOUT),
        "initial status timed out"
    );
    assert!(!is_modified(status_of(&shared, &base, "tracked.rs")));

    shared.pause_git_status();
    assert!(shared.is_git_status_paused());

    fs::write(base.join("tracked.rs"), "2").unwrap();
    fs::write(base.join("new.rs"), "1").unwrap();

    // the index keeps updating while git status is paused
    wait_until("new.rs indexed", || indexed(&shared, &base, "new.rs"));
    std::thread::sleep(Duration::from_millis(300));
    assert!(shared.wait_for_git_status(TIMEOUT));
    assert!(!is_modified(status_of(&shared, &base, "tracked.rs")));
    assert_eq!(status_of(&shared, &base, "new.rs"), None);

    shared.resume_git_status();
    assert!(!shared.is_git_status_paused());
    assert!(
        shared.wait_for_git_status(TIMEOUT),
        "resume refresh timed out"
    );
    assert!(is_modified(status_of(&shared, &base, "tracked.rs")));
    assert_eq!(status_of(&shared, &base, "new.rs"), Some(Status::WT_NEW));
}

#[test]
fn paused_before_init_only_updates_on_manual_refresh() {
    let (_tmp, base) = init_repo();
    fs::write(base.join("committed.rs"), "1").unwrap();
    git(&base, &["add", "-A"]);
    git(&base, &["commit", "-m", "init", "--no-gpg-sign"]);
    fs::write(base.join("untracked.rs"), "1").unwrap();

    let frecency = SharedFrecency::noop();
    let shared = SharedFilePicker::default();
    shared.pause_git_status();
    let shared = start_picker_on(shared, &base, false);
    assert!(shared.wait_for_git_status(TIMEOUT));
    assert_eq!(status_of(&shared, &base, "untracked.rs"), None);

    shared.refresh_git_status(&frecency).unwrap();
    assert!(shared.is_git_status_paused());
    assert_eq!(
        status_of(&shared, &base, "untracked.rs"),
        Some(Status::WT_NEW)
    );

    // re-indexing keeps the pause
    shared.trigger_full_rescan_async(&frecency).unwrap();
    assert!(shared.wait_for_scan(TIMEOUT), "rescan timed out");
    assert!(shared.is_git_status_paused());
}

#[test]
fn resume_refreshes_a_picker_created_while_paused() {
    let (_tmp, base) = init_repo();
    fs::write(base.join("committed.rs"), "1").unwrap();
    git(&base, &["add", "-A"]);
    git(&base, &["commit", "-m", "init", "--no-gpg-sign"]);
    fs::write(base.join("untracked.rs"), "1").unwrap();

    let shared = SharedFilePicker::default();
    shared.pause_git_status();
    let shared = start_picker_on(shared, &base, false);
    assert!(shared.wait_for_scan(TIMEOUT), "scan timed out");
    assert_eq!(status_of(&shared, &base, "untracked.rs"), None);

    shared.resume_git_status();
    assert!(
        shared.wait_for_git_status(TIMEOUT),
        "resume refresh timed out"
    );
    assert_eq!(
        status_of(&shared, &base, "untracked.rs"),
        Some(Status::WT_NEW)
    );
}

fn start_picker(base: &Path, watch: bool) -> SharedFilePicker {
    start_picker_on(SharedFilePicker::default(), base, watch)
}

fn start_picker_on(shared: SharedFilePicker, base: &Path, watch: bool) -> SharedFilePicker {
    FilePicker::new_with_shared_state(
        shared.clone(),
        SharedFrecency::noop(),
        FilePickerOptions {
            base_path: base.to_string_lossy().into_owned(),
            watch,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(shared.wait_for_scan(TIMEOUT), "scan timed out");
    shared
}

fn indexed(shared: &SharedFilePicker, base: &Path, rel: &str) -> bool {
    let guard = shared.read().unwrap();
    guard
        .as_ref()
        .is_some_and(|picker| picker.get_file_by_path(base.join(rel)).is_some())
}

fn status_of(shared: &SharedFilePicker, base: &Path, rel: &str) -> Option<Status> {
    let guard = shared.read().unwrap();
    let picker = guard.as_ref().expect("picker missing");
    picker
        .get_file_by_path(base.join(rel))
        .unwrap_or_else(|| panic!("{rel} not indexed"))
        .git_status
}

fn is_modified(status: Option<Status>) -> bool {
    status.is_some_and(|s| s.contains(Status::WT_MODIFIED))
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn init_repo() -> (TempDir, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let base = fff_search::path_utils::canonicalize(tmp.path()).unwrap();
    git(&base, &["init", "-b", "main"]);
    (tmp, base)
}
