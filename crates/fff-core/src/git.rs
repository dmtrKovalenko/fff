use crate::error::Result;
use ahash::AHashMap;
use git2::{Repository, Status, StatusOptions};
use std::{
    fmt::Debug,
    path::{Path, PathBuf},
};

/// Process-wide libgit2 tuning for read-heavy local use. By default libgit2
/// never caches trees over 4KB (large repos re-inflate every directory tree on
/// every diff) and SHA-verifies each object read; measured on chromium this
/// takes a 10-commit recency walk from 1.4s down to 0.56s.
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

#[derive(Debug, Clone, Default)]
pub(crate) struct GitStatusCache(AHashMap<PathBuf, Status>);

impl IntoIterator for GitStatusCache {
    type Item = (PathBuf, Status);
    type IntoIter = <AHashMap<PathBuf, Status> as IntoIterator>::IntoIter;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl GitStatusCache {
    pub fn statuses_len(&self) -> usize {
        self.0.len()
    }

    #[inline]
    pub fn lookup_status(&self, full_path: &Path) -> Option<Status> {
        self.0.get(full_path).copied()
    }

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

    pub fn read_git_status(
        git_workdir: Option<&Path>,
        status_options: &mut StatusOptions,
    ) -> Option<Self> {
        let git_workdir = git_workdir.as_ref()?;
        let repository = Repository::open(git_workdir).ok()?;

        let status = Self::read_status(&repository, status_options);

        match status {
            Ok(status) => Some(status),
            Err(e) => {
                tracing::error!(?e, "Failed to read git status");

                None
            }
        }
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

        let mut status_options = default_status_options();
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
            } else if status.contains(Status::CURRENT) || status.is_empty() {
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
        let cache = GitStatusCache::git_status_for_paths(&repo, &paths).unwrap();

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
}
