#[cfg(feature = "libgit2")]
pub(super) mod backend;
#[cfg(feature = "gitoxide")]
pub(crate) mod gitoxide;
#[cfg(feature = "libgit2")]
pub(super) mod libgit2_status;
pub mod recency;
pub(super) mod worker;
#[cfg(any(feature = "gitoxide", feature = "libgit2"))]
use crate::scm::ScmStatusCache;
#[cfg(feature = "libgit2")]
pub use git2::Status;
#[cfg(not(feature = "libgit2"))]
mod status;
#[cfg(not(feature = "libgit2"))]
pub use status::Status;

use std::path::{Path, PathBuf};
#[cfg(any(feature = "gitoxide", feature = "libgit2"))]
use std::time::{Duration, Instant};

/// Returns the configured backend and the working tree containing `path`, when present.
pub fn backend_info(path: &Path) -> (Option<&'static str>, Option<PathBuf>) {
    let backend = crate::scm::ScmBackend::configured();
    (
        backend.name(),
        backend.discover(path).map(|workspace| workspace.workdir),
    )
}

#[inline]
pub fn is_modified_status(status: Status) -> bool {
    status.intersects(
        Status::WT_MODIFIED
            | Status::INDEX_MODIFIED
            | Status::WT_NEW
            | Status::INDEX_NEW
            | Status::WT_RENAMED,
    )
}

pub fn format_git_status_opt(status: Option<Status>) -> Option<&'static str> {
    match status {
        None => Some("clean"),
        Some(status) => {
            if status.contains(Status::WT_NEW) {
                Some("untracked")
            } else if status.contains(Status::WT_MODIFIED) {
                Some("modified")
            } else if status.contains(Status::WT_DELETED) {
                Some("deleted")
            } else if status.contains(Status::WT_RENAMED) {
                Some("renamed")
            } else if status.contains(Status::INDEX_NEW) {
                Some("staged_new")
            } else if status.contains(Status::INDEX_MODIFIED) {
                Some("staged_modified")
            } else if status.contains(Status::INDEX_DELETED) {
                Some("staged_deleted")
            } else if status.contains(Status::IGNORED) {
                Some("ignored")
            // `Status::CURRENT` is 0, so `contains` is true for every status:
            // testing it here labelled conflicted/typechanged files "clean".
            } else if status.is_empty() {
                Some("clean")
            } else {
                None
            }
        }
    }
}

pub fn format_git_status(status: Option<Status>) -> &'static str {
    format_git_status_opt(status).unwrap_or("unknown")
}

// Same as git's main worktree: the common dir without its `.git` suffix, so bare
// and separate git dirs key the shared history by the common dir itself.
#[cfg(any(feature = "gitoxide", feature = "libgit2"))]
fn main_checkout_root(common_dir: &Path) -> PathBuf {
    let common_dir: PathBuf = common_dir.components().collect();
    match common_dir.parent() {
        Some(main_root) if common_dir.file_name().is_some_and(|name| name == ".git") => {
            main_root.to_path_buf()
        }
        _ => common_dir,
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "libgit2")]
    use super::libgit2_status::default_status_options;
    #[cfg(all(feature = "libgit2", feature = "gitoxide"))]
    use super::libgit2_status::initial_scan_status_options;
    use super::*;
    #[cfg(any(feature = "libgit2", feature = "gitoxide"))]
    fn status_map(rows: ScmStatusCache) -> ahash::AHashMap<String, Status> {
        use crate::scm::StatusMap;
        let mut map = ahash::AHashMap::new();
        for (path, status) in rows.iter() {
            assert!(
                map.insert(path.to_owned(), status).is_none(),
                "duplicate {path}"
            );
        }
        map
    }
    #[cfg(feature = "libgit2")]
    use git2::Repository;
    #[cfg(any(feature = "libgit2", feature = "gitoxide"))]
    use std::fs;
    #[cfg(any(feature = "libgit2", feature = "gitoxide"))]
    use std::process::Command;
    #[cfg(any(feature = "libgit2", feature = "gitoxide"))]
    use tempfile::TempDir;

    #[cfg(any(feature = "libgit2", feature = "gitoxide"))]
    fn git(dir: &Path, args: &[&str]) {
        assert!(git_status(dir, args), "git {args:?} failed");
    }

    #[cfg(any(feature = "libgit2", feature = "gitoxide"))]
    fn git_status(dir: &Path, args: &[&str]) -> bool {
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap()
            .status
            .success()
    }

    #[test]
    #[cfg(feature = "libgit2")]
    fn index_and_changed_entries_match_the_full_refresh() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        git(root, &["init", "-b", "main"]);
        git(root, &["config", "user.name", "FFF Test"]);
        git(root, &["config", "user.email", "test@example.invalid"]);
        for path in [
            "clean",
            "modified",
            "deleted",
            "old",
            "staged",
            "conflicted",
        ] {
            fs::write(root.join(path), path).unwrap();
        }
        fs::write(root.join(".gitignore"), "ignored\n").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-m", "seed", "--no-gpg-sign"]);
        git(root, &["checkout", "-b", "theirs"]);
        fs::write(root.join("conflicted"), "theirs").unwrap();
        git(root, &["commit", "-am", "theirs", "--no-gpg-sign"]);
        git(root, &["checkout", "main"]);
        fs::write(root.join("conflicted"), "ours").unwrap();
        git(root, &["commit", "-am", "ours", "--no-gpg-sign"]);
        assert!(
            !git_status(root, &["merge", "theirs"]),
            "merge must conflict"
        );
        fs::write(root.join("intent"), "intent").unwrap();
        git(root, &["add", "-N", "intent"]);
        fs::write(root.join("modified"), "changed").unwrap();
        fs::remove_file(root.join("deleted")).unwrap();
        fs::write(root.join("untracked"), "new").unwrap();
        fs::write(root.join("ignored"), "ignored").unwrap();
        fs::write(root.join("staged"), "changed").unwrap();
        git(root, &["add", "staged"]);
        git(root, &["mv", "old", "renamed"]);
        fs::write(root.join("staged"), "changed again").unwrap();
        fs::create_dir_all(root.join("new/nested")).unwrap();
        fs::write(root.join("new/nested/file"), "new").unwrap();
        git(root, &["config", "status.showUntrackedFiles", "no"]);
        let repo = Repository::open(root).unwrap();
        #[cfg(feature = "gitoxide")]
        for full in [false, true] {
            let mut opts = if full {
                default_status_options()
            } else {
                initial_scan_status_options()
            };
            let expected = status_map(ScmStatusCache::read_status(&repo, &mut opts).unwrap());
            let statuses: Vec<_> = expected.values().copied().collect();
            assert!(statuses.contains(&Status::CONFLICTED));
            assert!(statuses.contains(&(Status::INDEX_NEW | Status::WT_MODIFIED)));
            let gix = gitoxide::open(root).unwrap();
            assert_eq!(
                status_map(gitoxide::read_status(&gix, full, &[]).unwrap()),
                expected
            );
            for backend in [
                crate::scm::ScmBackend::LibGit2,
                crate::scm::ScmBackend::Gitoxide,
            ] {
                let workspace = backend.discover(root).unwrap();
                let session = workspace.open().unwrap();
                assert_eq!(status_map(session.status(full).unwrap()), expected);
            }
        }
        let expected =
            status_map(ScmStatusCache::read_status(&repo, &mut default_status_options()).unwrap());
        let actual = ScmStatusCache::read_status_from_index(&repo, &repo.index().unwrap()).unwrap();
        assert_eq!(status_map(actual), expected);
    }

    #[cfg(feature = "gitoxide")]
    #[test]
    fn gitoxide_preserves_linked_worktree_history_and_ignores() {
        let tmp = TempDir::new().unwrap();
        let root = crate::path_utils::canonicalize(tmp.path()).unwrap();
        let main = root.join("main");
        let linked = root.join("linked");
        git(&root, &["init", "-b", "main", "main"]);
        fs::write(main.join("original"), "main").unwrap();
        fs::write(main.join(".gitignore"), "ignored/\n").unwrap();
        git(&main, &["add", "."]);
        git(&main, &["commit", "-m", "seed", "--no-gpg-sign"]);
        git(&main, &["worktree", "add", "-b", "feature", "../linked"]);
        fs::create_dir(linked.join("src")).unwrap();
        fs::write(linked.join("src/added"), "feature").unwrap();
        git(&linked, &["add", "."]);
        git(&linked, &["commit", "-m", "feature", "--no-gpg-sign"]);
        fs::create_dir(linked.join("ignored")).unwrap();
        let backend = crate::scm::ScmBackend::Gitoxide;
        let workspace = backend.discover(&linked.join("src")).unwrap();
        assert_eq!(workspace.workdir, linked);
        assert_eq!(
            workspace.shared_base(&linked.join("src")),
            Some(main.join("src"))
        );
        let repo = workspace.open().unwrap();
        assert!(repo.is_ignored(&linked.join("ignored/file")));
        assert!(!repo.is_ignored(&linked.join("src/added")));
        let scores = repo.recency(&Default::default(), &linked).unwrap();
        assert_eq!(scores.len(), 1);
        assert_eq!(scores["src/added"], 1);
        let scores = repo
            .recency(&Default::default(), &linked.join("src"))
            .unwrap();
        assert_eq!(scores["added"], 1);
    }

    #[test]
    fn unclassified_status_is_not_reported_as_clean() {
        // A merge-conflicted file must not claim to be clean.
        assert_eq!(format_git_status_opt(Some(Status::CONFLICTED)), None);
        assert_eq!(format_git_status(Some(Status::CONFLICTED)), "unknown");
        assert_eq!(format_git_status(Some(Status::WT_TYPECHANGE)), "unknown");

        // Every already-classified state keeps its label.
        assert_eq!(format_git_status_opt(None), Some("clean"));
        assert_eq!(format_git_status_opt(Some(Status::empty())), Some("clean"));
        assert_eq!(format_git_status_opt(Some(Status::CURRENT)), Some("clean"));
        assert_eq!(
            format_git_status_opt(Some(Status::WT_NEW)),
            Some("untracked")
        );
        assert_eq!(
            format_git_status_opt(Some(Status::WT_MODIFIED)),
            Some("modified")
        );
        assert_eq!(
            format_git_status_opt(Some(Status::INDEX_MODIFIED)),
            Some("staged_modified")
        );
        assert_eq!(
            format_git_status_opt(Some(Status::IGNORED)),
            Some("ignored")
        );
    }

    #[test]
    #[cfg(any(feature = "libgit2", feature = "gitoxide"))]
    fn lookup_is_case_exact() {
        let tmp = TempDir::new().unwrap();
        // `std::fs::canonicalize` on Windows adds a `\\?\` UNC prefix that
        // libgit2's workdir string lacks. Use dunce so both sides match.
        let base = crate::path_utils::canonicalize(tmp.path()).unwrap();

        // Mixed-case names that sort differently under byte-wise vs
        // case-insensitive comparators.
        let names = [
            "README.md",
            "a_lower.rs",
            "Z_upper.rs",
            "mixed_Case.txt",
            "nested/Inner_File.rs",
        ];
        for n in &names {
            let p = base.join(n);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, format!("// {n}\n")).unwrap();
        }

        git(&base, &["init", "-b", "main"]);
        git(&base, &["add", "-A"]);
        git(&base, &["commit", "-m", "seed", "--no-gpg-sign"]);

        // Modify every file so they all end up in the status output as
        // WT_MODIFIED — guarantees a non-trivial map we have to look up.
        for n in &names {
            let p = base.join(n);
            fs::write(&p, format!("// {n}\n// edit\n")).unwrap();
        }

        let repo = crate::scm::ScmBackend::configured().open(&base).unwrap();
        let paths: Vec<PathBuf> = names.iter().map(|n| base.join(n)).collect();
        let cache = status_map(repo.status_for_paths(&paths).unwrap());

        for n in names {
            let status = cache.get(n).copied();
            assert!(
                status.is_some(),
                "lookup for {n} returned None; cache holds {} entries",
                cache.len(),
            );
            assert!(
                status.unwrap().contains(Status::WT_MODIFIED),
                "expected WT_MODIFIED for {n}, got {:?}",
                status
            );
        }
    }

    // Paths are literal: `[id]` must not glob-match `i`/`d`, and new/deleted files still resolve.
    #[test]
    #[cfg(any(feature = "libgit2", feature = "gitoxide"))]
    fn status_for_paths_treats_paths_literally() {
        let tmp = TempDir::new().unwrap();
        let base = crate::path_utils::canonicalize(tmp.path()).unwrap();

        for n in ["app/[id]/page.tsx", "app/i/page.tsx", "gone.rs"] {
            let p = base.join(n);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, "x\n").unwrap();
        }
        git(&base, &["init", "-b", "main"]);
        git(&base, &["add", "-A"]);
        git(&base, &["commit", "-m", "seed", "--no-gpg-sign"]);

        fs::write(base.join("app/[id]/page.tsx"), "y\n").unwrap();
        fs::remove_file(base.join("gone.rs")).unwrap();
        fs::create_dir_all(base.join("fresh/dir")).unwrap();
        fs::write(base.join("fresh/dir/new.rs"), "z\n").unwrap();

        let repo = crate::scm::ScmBackend::configured().open(&base).unwrap();
        let paths: Vec<PathBuf> = ["app/[id]/page.tsx", "gone.rs", "fresh/dir/new.rs"]
            .iter()
            .map(|n| base.join(n))
            .collect();
        let cache = status_map(repo.status_for_paths(&paths).unwrap());

        let status = |n: &str| cache.get(n).copied();
        assert!(status("app/[id]/page.tsx").is_some_and(|s| s.contains(Status::WT_MODIFIED)));
        assert!(status("gone.rs").is_some_and(|s| s.contains(Status::WT_DELETED)));
        assert!(status("fresh/dir/new.rs").is_some_and(|s| s.contains(Status::WT_NEW)));
        assert_eq!(status("app/i/page.tsx"), None);
        fs::write(base.join("app/[id]/page.tsx"), "x\n").unwrap();
        let clean = status_map(repo.status_for_paths(&paths[..1]).unwrap());
        assert_eq!(
            clean.get("app/[id]/page.tsx").copied(),
            Some(Status::CURRENT)
        );

        assert_eq!(cache.len(), 3);
    }
}

#[cfg(any(feature = "gitoxide", feature = "libgit2"))]
fn wait_for_git_index_lock_release(git_dir: &Path) {
    const GIT_LOCK_POLL: Duration = Duration::from_millis(10);
    const GIT_LOCK_MAX_WAIT: Duration = Duration::from_millis(500);

    let lock = git_dir.join("index.lock");
    // Fast path: no lock present.
    if !lock.exists() {
        return;
    }
    let deadline = Instant::now() + GIT_LOCK_MAX_WAIT;
    while lock.exists() && Instant::now() < deadline {
        std::thread::sleep(GIT_LOCK_POLL);
    }
    if lock.exists() {
        tracing::warn!(
            "Proceeding with git status refresh despite lingering \
             .git/index.lock at {} — will retry once it clears",
            lock.display()
        );
    }
}
