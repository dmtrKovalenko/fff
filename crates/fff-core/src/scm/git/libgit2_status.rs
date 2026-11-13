use super::{ScmStatusCache, Status};
use crate::error::Result;
use ahash::AHashMap;
use git2::{Repository, StatusOptions};
use std::{fmt::Debug, path::Path};

pub(crate) fn tune_libgit2_for_local_reads() {
    static TUNE: std::sync::Once = std::sync::Once::new();
    TUNE.call_once(|| {
        // Same tradeoff cargo makes: local objects are trusted, skip hashing.
        git2::opts::strict_hash_verification(false);
        // SAFETY: only raises the per-tree limit within the existing process-wide cache cap.
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

    pub(super) fn read_status_from_index(
        repo: &Repository,
        index: &git2::Index,
        root: &Path,
    ) -> Result<Self> {
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
