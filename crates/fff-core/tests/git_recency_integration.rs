use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use fff_search::file_picker::FilePicker;
use fff_search::{FilePickerOptions, SharedFilePicker, SharedFrecency};
use tempfile::TempDir;

#[test]
fn collect_files_applies_recency_scores() {
    let (_tmp, base) = init_repo();
    commit_file(&base, "hot.rs", "1", "c1");
    commit_file(&base, "cold.rs", "1", "c2");
    commit_file(&base, "hot.rs", "2", "c3");
    commit_file(&base, "src/hot_nested.rs", "1", "c4");

    let mut picker = FilePicker::new(FilePickerOptions {
        base_path: base.to_string_lossy().into_owned(),
        watch: false,
        ..Default::default()
    })
    .unwrap();
    picker.collect_files().unwrap();

    assert_eq!(recency_score(&picker, &base, "hot.rs"), 2);
    assert_eq!(recency_score(&picker, &base, "cold.rs"), 1);
    assert_eq!(recency_score(&picker, &base, "src/hot_nested.rs"), 1);
}

/// Git reports repo-relative paths while the index is relative to base_path,
/// so a picker rooted below the repo root has to strip the subdirectory.
#[test]
fn picker_rooted_in_a_subdirectory_scores_by_index_relative_path() {
    let (_tmp, repo) = init_repo();
    commit_file(&repo, "sub/hot.rs", "1", "c1");
    commit_file(&repo, "outside.rs", "1", "c2");
    commit_file(&repo, "sub/hot.rs", "2", "c3");
    commit_file(&repo, "sub/nested/cold.rs", "1", "c4");

    let sub = repo.join("sub");
    let mut picker = FilePicker::new(FilePickerOptions {
        base_path: sub.to_string_lossy().into_owned(),
        watch: false,
        ..Default::default()
    })
    .unwrap();
    picker.collect_files().unwrap();

    assert_eq!(recency_score(&picker, &sub, "hot.rs"), 2);
    assert_eq!(recency_score(&picker, &sub, "nested/cold.rs"), 1);
    assert!(
        picker.get_file_by_path(repo.join("outside.rs")).is_none(),
        "files above base_path are not indexed and must not be scored"
    );
}

// End-to-end through the runtime path: the background scan populates scores
// via the git-status worker; refresh_git_status re-applies on git changes.
#[test]
fn refresh_git_status_tracks_commits_and_branch_switches() {
    let (_tmp, base) = init_repo();
    commit_file(&base, "main_a.rs", "1", "m1");
    commit_file(&base, "main_b.rs", "1", "m2");
    // Present (untracked) at scan time so it is indexed without a watcher;
    // committed only later on the feature branch.
    fs::write(base.join("feat.rs"), "0").unwrap();

    let shared = SharedFilePicker::default();
    let frecency = SharedFrecency::default();

    FilePicker::new_with_shared_state(
        shared.clone(),
        frecency.clone(),
        FilePickerOptions {
            base_path: base.to_string_lossy().into_owned(),
            watch: false,
            ..Default::default()
        },
    )
    .unwrap();

    assert!(
        shared.wait_for_scan(Duration::from_secs(20)),
        "scan timed out"
    );

    // The git-status worker applies initial recency scores asynchronously.
    wait_for_score(&shared, &base, "main_a.rs", 1);
    wait_for_score(&shared, &base, "main_b.rs", 1);

    // New commits on a feature branch: only branch commits count and files
    // that fell out of the window are reset.
    git(&base, &["checkout", "-b", "feature"]);
    commit_file(&base, "feat.rs", "1", "f1");
    commit_file(&base, "feat.rs", "2", "f2");

    shared.refresh_git_status(&frecency).unwrap();

    let guard = shared.read().unwrap();
    let picker = guard.as_ref().unwrap();
    assert_eq!(recency_score(picker, &base, "feat.rs"), 2);
    assert_eq!(
        recency_score(picker, &base, "main_a.rs"),
        0,
        "base-branch files must be reset after switching to a feature branch"
    );
    drop(guard);

    // A second refresh recomputes from scratch and lands on the same scores.
    shared.refresh_git_status(&frecency).unwrap();
    let guard = shared.read().unwrap();
    let picker = guard.as_ref().unwrap();
    assert_eq!(recency_score(picker, &base, "feat.rs"), 2);
}

fn wait_for_score(shared: &SharedFilePicker, base: &Path, rel: &str, expected: i16) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        {
            let guard = shared.read().unwrap();
            if let Some(picker) = guard.as_ref()
                && picker
                    .get_file_by_path(base.join(rel))
                    .is_some_and(|f| f.git_recency_score == expected)
            {
                return;
            }
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {rel} to reach recency score {expected}"
        );
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

fn commit_file(dir: &Path, rel: &str, content: &str, message: &str) {
    let path = dir.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, content).unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-m", message, "--no-gpg-sign"]);
}

fn init_repo() -> (TempDir, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let base = fff_search::path_utils::canonicalize(tmp.path()).unwrap();
    git(&base, &["init", "-b", "main"]);
    (tmp, base)
}

fn recency_score(picker: &FilePicker, base: &Path, rel: &str) -> i16 {
    picker
        .get_file_by_path(base.join(rel))
        .map(|f| f.git_recency_score)
        .unwrap_or_else(|| panic!("{rel} not indexed"))
}
