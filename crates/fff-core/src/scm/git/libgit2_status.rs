use super::{ScmStatusCache, Status};
use crate::error::Result;
use crate::scm::StatusMap;
use ahash::AHashSet;
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

#[derive(Default)]
pub(crate) struct Statuses {
    paths: String,
    entries: Vec<(usize, Status)>,
}

impl Statuses {
    fn push(&mut self, path: &str, status: Status) {
        self.paths.push_str(path);
        self.entries.push((self.paths.len(), status));
    }
}

impl StatusMap for Statuses {
    fn iter(&self) -> impl Iterator<Item = (&str, Status)> {
        self.entries.iter().scan(0, |start, &(end, status)| {
            let path = &self.paths[*start..end];
            *start = end;
            Some((path, status))
        })
    }
}

impl ScmStatusCache {
    pub(super) fn read_status(repo: &Repository, options: &mut StatusOptions) -> Result<Self> {
        let native = repo.statuses(Some(options))?;
        let mut rows = Statuses::default();
        rows.entries.reserve(native.len());
        for entry in &native {
            if let Ok(path) = entry.path() {
                rows.push(path, entry.status());
            }
        }
        Ok(Self::LibGit2(rows))
    }

    pub(super) fn read_full_status(repo: &Repository) -> Result<Self> {
        if cfg!(target_os = "linux")
            && let Ok(index) = repo.index()
            && index.len() >= 16_384
            && !index.has_conflicts()
            && !repo.config()?.get_bool("core.ignorecase").unwrap_or(false)
        {
            return Self::read_status_from_index(repo, &index);
        }
        Self::read_status(repo, &mut default_status_options())
    }

    pub(super) fn read_status_from_index(repo: &Repository, index: &git2::Index) -> Result<Self> {
        let changes = repo.statuses(Some(&mut initial_scan_status_options()))?;
        let mut changed = AHashSet::with_capacity(changes.len());
        let mut rows = Statuses::default();
        rows.entries.reserve(index.len().max(changes.len()));
        for entry in &changes {
            let delta = entry.head_to_index().or_else(|| entry.index_to_workdir());
            if let Some(path) = delta.and_then(|d| d.old_file().path_bytes())
                && let Ok(path) = std::str::from_utf8(path)
            {
                changed.insert(path);
                rows.push(path, entry.status());
            }
        }
        for entry in index.iter() {
            if entry.mode != 0o160000
                && let Ok(path) = std::str::from_utf8(&entry.path)
                && !changed.contains(path)
            {
                rows.push(path, Status::CURRENT);
            }
        }
        Ok(Self::LibGit2(rows))
    }

    #[tracing::instrument(skip(repo), fields(paths_count = paths.len()), level = tracing::Level::DEBUG)]
    pub(super) fn git_status_for_paths<TPath: AsRef<Path> + Debug>(
        repo: &Repository,
        paths: &[TPath],
    ) -> Result<Self> {
        let Some(workdir) = repo.workdir() else {
            return Ok(Self::LibGit2(Statuses::default()));
        };
        let workdir = crate::path_utils::normalize(workdir.to_owned());
        if paths.is_empty() {
            return Ok(Self::LibGit2(Statuses::default()));
        }
        if let [path] = paths {
            let relative = path.as_ref().strip_prefix(workdir)?;
            let status = repo.status_file(relative)?;
            let mut rows = Statuses::default();
            if let Some(path) = relative.to_str() {
                rows.push(&crate::path_utils::to_canonical_slashes(path), status);
            }
            return Ok(Self::LibGit2(rows));
        }
        let mut options = default_status_options();
        options.disable_pathspec_match(true);
        for path in paths {
            options.pathspec(path.as_ref().strip_prefix(&workdir)?);
        }
        Self::read_status(repo, &mut options)
    }
}
