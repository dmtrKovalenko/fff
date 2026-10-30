mod backend;
#[cfg(feature = "gitoxide")]
pub(crate) mod gitoxide;
pub mod recency;
mod worker;
use crate::scm::ScmStatusCache;
pub(crate) use backend::LibGit2;

use crate::error::Result;
use ahash::AHashMap;
use git2::{Repository, Status, StatusOptions};
use std::{
    fmt::Debug,
    path::{Path, PathBuf},
};

/// Process-wide libgit2 tuning for read-heavy local use. By default libgit2
/// never caches trees over 4KB and additionally verifies every object. This roughly costs 150% of
/// its valuable runtime
pub(crate) fn tune_libgit2_for_local_reads() {
    static TUNE: std::sync::Once = std::sync::Once::new();
    TUNE.call_once(|| {
        // Same tradeoff cargo makes: local objects are trusted, skip hashing.
        git2::opts::strict_hash_verification(false);
        // SAFETY: plain process-global size limit; total memory stays bounded
        // by libgit2's 256MB odb cache.
        let _ =
            unsafe { git2::opts::set_cache_object_limit(git2::ObjectType::Tree, 8 * 1024 * 1024) };
    });
}

pub(crate) fn default_status_options() -> StatusOptions {
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_unmodified(true)
        .exclude_submodules(true);
    opts
}

/// Status options for the initial scan / rescan.
///
/// Skips `include_unmodified` because every `FileItem` starts with
/// `git_status: None` (== clean), so a missing cache entry already means
/// "clean" — no need to ask libgit2 to enumerate every tracked path.
/// Saves seconds on huge dirty trees (e.g. chromium with 400k+ entries).
pub(crate) fn initial_scan_status_options() -> StatusOptions {
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .exclude_submodules(true);
    opts
}

impl ScmStatusCache {
    #[tracing::instrument(skip(repo, status_options))]
    pub(crate) fn read_status(
        repo: &Repository,
        status_options: &mut StatusOptions,
    ) -> Result<Self> {
        let statuses = repo.statuses(Some(status_options))?;
        let Some(repo_path) = repo.workdir() else {
            return Ok(Self(AHashMap::new())); // repo is bare
        };

        let repo_path = crate::path_utils::normalize(repo_path.to_path_buf());

        let mut entries = AHashMap::with_capacity(statuses.len());
        for entry in &statuses {
            if let Ok(entry_path) = entry.path() {
                // libgit2 returns entry paths with forward slashes on every platform
                // fff stores native paths - meaning we have forward slash issue on windows
                let full_path = crate::path_utils::normalize(repo_path.join(entry_path));
                entries.insert(full_path, entry.status());
            }
        }

        Ok(Self(entries))
    }

    /// Keep clean entries out of libgit2's two diff lists on large, case-sensitive
    /// working trees. Status flags for changes still come directly from libgit2.
    pub(crate) fn read_full_status(repo: &Repository) -> Result<Self> {
        let full = || Self::read_status(repo, &mut default_status_options());
        let Some(root) = repo.workdir() else {
            return full();
        };
        let Ok(index) = repo.index() else {
            return full();
        };
        if !cfg!(target_os = "linux")
            || index.len() < 16_384
            || index.has_conflicts()
            || repo.config()?.get_bool("core.ignorecase").unwrap_or(false)
        {
            return full();
        }
        Self::read_status_from_index(repo, &index, root)
    }

    fn read_status_from_index(repo: &Repository, index: &git2::Index, root: &Path) -> Result<Self> {
        let mut cache = Self::read_status(repo, &mut initial_scan_status_options())?;
        let additional = index.len().saturating_sub(cache.0.len());
        cache.0.reserve(additional);
        let root = crate::path_utils::normalize(root.to_owned());
        for entry in index.iter() {
            // A gitlink is a submodule. The full refresh excludes submodules.
            if entry.mode != 0o160000
                && let Ok(path) = std::str::from_utf8(&entry.path)
            {
                let path = crate::path_utils::normalize(root.join(path));
                cache.0.entry(path).or_insert(Status::CURRENT);
            }
        }
        Ok(cache)
    }

    #[tracing::instrument(skip(repo), fields(paths_count = paths.len()), level = tracing::Level::DEBUG)]
    pub fn git_status_for_paths<TPath: AsRef<Path> + Debug>(
        repo: &Repository,
        paths: &[TPath],
    ) -> Result<Self> {
        if paths.is_empty() {
            return Ok(Self(AHashMap::new()));
        }

        let Some(workdir) = repo.workdir() else {
            return Ok(Self(AHashMap::new()));
        };
        let workdir = crate::path_utils::normalize(workdir.to_path_buf());

        // git pathspec is pretty slow and requires to walk the whole directory
        // so for a single file which is the most general use case we query directly the file
        if paths.len() == 1 {
            let full_path = paths[0].as_ref();
            let relative_path = full_path.strip_prefix(&workdir)?;
            let status = repo.status_file(relative_path)?;

            let mut map = AHashMap::with_capacity(1);
            map.insert(full_path.to_path_buf(), status);
            return Ok(Self(map));
        }

        // literal paths make libgit2 use a sorted pathlist instead of wildmatching
        // every entry against every pathspec (O(entries * paths), minutes on 80k paths)
        let mut status_options = default_status_options();
        status_options.disable_pathspec_match(true);
        for path in paths {
            status_options.pathspec(path.as_ref().strip_prefix(&workdir)?);
        }

        let git_status_cache = Self::read_status(repo, &mut status_options)?;
        Ok(git_status_cache)
    }
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
fn main_checkout_root(repo: &Repository) -> PathBuf {
    let common_dir: PathBuf = repo.commondir().components().collect();
    match common_dir.parent() {
        Some(main_root) if common_dir.file_name().is_some_and(|name| name == ".git") => {
            main_root.to_path_buf()
        }
        _ => common_dir,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use tempfile::TempDir;

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
        assert!(out.status.success(), "git {args:?} failed");
    }

    #[test]
    fn index_and_changed_entries_match_the_full_refresh() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        git(root, &["init"]);
        git(root, &["config", "user.name", "FFF Test"]);
        git(root, &["config", "user.email", "test@example.invalid"]);
        for path in ["clean", "modified", "deleted", "old", "staged"] {
            fs::write(root.join(path), path).unwrap();
        }
        fs::write(root.join(".gitignore"), "ignored\n").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-m", "seed", "--no-gpg-sign"]);
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
            let expected = ScmStatusCache::read_status(&repo, &mut opts).unwrap();
            let actual = gitoxide::scan(&repo, full).expect("fixture must use gitoxide");
            for name in ["libgit2", "gitoxide"] {
                let workspace = crate::scm::select_backend(Some(name))
                    .discover(root)
                    .unwrap();
                let session = workspace.open().unwrap();
                assert_eq!(session.status(full).unwrap().0, expected.0);
            }
            assert_eq!(actual.0, expected.0);
        }
        let expected = ScmStatusCache::read_status(&repo, &mut default_status_options()).unwrap();
        let actual = ScmStatusCache::read_status_from_index(
            &repo,
            &repo.index().unwrap(),
            repo.workdir().unwrap(),
        )
        .unwrap();
        assert_eq!(actual.0, expected.0);
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

    /// Regression: on case-insensitive filesystems libgit2 returns
    /// statuses in a case-insensitive order. Our previous sorted-`Vec` +
    /// `binary_search_by(Path::cmp)` lookup silently missed entries
    /// because `Path::cmp` is byte-wise.
    ///
    /// This test uses deliberately mixed-case filenames so the two
    /// orderings disagree, then checks every lookup succeeds.
    #[test]
    fn lookup_is_case_exact_regardless_of_libgit2_sort_order() {
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

        let repo = Repository::open(&base).unwrap();
        let paths: Vec<PathBuf> = names.iter().map(|n| base.join(n)).collect();
        let cache = ScmStatusCache::git_status_for_paths(&repo, &paths).unwrap();

        for (n, abs) in names.iter().zip(paths.iter()) {
            let status = cache.lookup_status(abs);
            assert!(
                status.is_some(),
                "lookup for {n} returned None; cache holds {} entries",
                cache.statuses_len(),
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

        let repo = Repository::open(&base).unwrap();
        let paths: Vec<PathBuf> = ["app/[id]/page.tsx", "gone.rs", "fresh/dir/new.rs"]
            .iter()
            .map(|n| base.join(n))
            .collect();
        let cache = ScmStatusCache::git_status_for_paths(&repo, &paths).unwrap();

        let status = |n: &str| cache.lookup_status(&base.join(n));
        assert!(status("app/[id]/page.tsx").is_some_and(|s| s.contains(Status::WT_MODIFIED)));
        assert!(status("gone.rs").is_some_and(|s| s.contains(Status::WT_DELETED)));
        assert!(status("fresh/dir/new.rs").is_some_and(|s| s.contains(Status::WT_NEW)));
        assert_eq!(status("app/i/page.tsx"), None);
        assert_eq!(cache.statuses_len(), 3);
    }
}
