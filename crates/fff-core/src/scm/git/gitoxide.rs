use super::{ScmStatusCache, Status, main_checkout_root, wait_for_git_index_lock_release};
use crate::error::{Error, Result};
use crate::git_recency::GitRecencyConfig;
use crate::scm::{ScmBackend, ScmWorkspace, repository::RepositoryOps};
use ahash::AHashMap;
use gitoxide::bstr::{BString, ByteSlice};
use gitoxide::diff::index::Change;
use gitoxide::index::entry::Mode;
use gitoxide::status::{Item, UntrackedFiles, index_worktree::iter::Summary};
use std::path::{Path, PathBuf};

mod recency;

pub(in crate::scm) fn discover(path: &Path) -> Option<ScmWorkspace> {
    let repo = gitoxide::discover(path).ok()?;
    let workdir = crate::path_utils::canonicalize(repo.workdir()?).ok()?;
    let metadata = crate::path_utils::canonicalize(repo.git_dir()).ok()?;
    let shared_root = if repo.worktree()?.is_main() {
        None
    } else {
        Some(main_checkout_root(
            &crate::path_utils::canonicalize(repo.common_dir()).ok()?,
        ))
    };
    Some(ScmWorkspace {
        backend: ScmBackend::Gitoxide,
        workdir,
        metadata,
        shared_root,
    })
}

pub(in crate::scm) fn open(root: &Path) -> Result<gitoxide::Repository> {
    let options =
        gitoxide::open::Options::default().config_overrides(["status.showUntrackedFiles=all"]);
    gitoxide::open_opts(root, options).map_err(git_error)
}

impl RepositoryOps for gitoxide::Repository {
    fn status(&self, full: bool) -> Result<ScmStatusCache> {
        #[cfg(feature = "libgit2")]
        {
            use std::sync::atomic::{AtomicBool, Ordering};
            static USE_LIBGIT2: AtomicBool = AtomicBool::new(false);
            if !USE_LIBGIT2.load(Ordering::Relaxed) {
                match read_status(self, full, &[]) {
                    Ok(cache) => return Ok(cache),
                    Err(error) => {
                        tracing::warn!(%error, "Gitoxide status failed, using libgit2 for subsequent scans");
                        USE_LIBGIT2.store(true, Ordering::Relaxed);
                    }
                }
            }
            RepositoryOps::status(&git2::Repository::open(self.git_dir())?, full)
        }
        #[cfg(not(feature = "libgit2"))]
        read_status(self, full, &[])
    }

    fn status_for_paths(&self, paths: &[PathBuf]) -> Result<ScmStatusCache> {
        if paths.is_empty() {
            return Ok(ScmStatusCache::default());
        }
        read_status(self, true, paths)
    }

    fn recency(&self, config: &GitRecencyConfig, base: &Path) -> Option<AHashMap<String, i16>> {
        recency::scores(self, config, base)
    }

    fn is_ignored(&self, path: &Path) -> bool {
        let Some(root) = self.workdir() else {
            return false;
        };
        let Ok(relative) = path.strip_prefix(root) else {
            return false;
        };
        let Ok(index) = self.index_or_empty() else {
            return false;
        };
        let Ok(mut excludes) = self.excludes(
            &index,
            None,
            gitoxide::worktree::stack::state::ignore::Source::WorktreeThenIdMappingIfNotSkipped,
        ) else {
            return false;
        };
        excludes
            .at_path(relative, path.is_dir().then_some(Mode::DIR))
            .is_ok_and(|entry| entry.is_excluded())
    }

    fn wait_for_write(&self) {
        wait_for_git_index_lock_release(self.git_dir());
    }
}

pub(super) fn read_status(
    repo: &gitoxide::Repository,
    full: bool,
    paths: &[PathBuf],
) -> Result<ScmStatusCache> {
    let root = repo
        .workdir()
        .ok_or_else(|| Error::InvalidPath(repo.git_dir().to_owned()))?;
    let mut relative = Vec::with_capacity(paths.len());
    let mut patterns = Vec::with_capacity(paths.len());
    for path in paths {
        let path = path.strip_prefix(root)?;
        let path = gitoxide::path::to_unix_separators_on_windows(
            gitoxide::path::try_into_bstr(path).map_err(git_error)?,
        );
        let mut pattern = b":(top,literal)".to_vec();
        pattern.extend_from_slice(&path);
        patterns.push(BString::from(pattern));
        relative.push(path);
    }
    let mut items = repo
        .status(gitoxide::progress::Discard)
        .map_err(git_error)?
        .untracked_files(UntrackedFiles::Files)
        .index_worktree_submodules(None)
        .index_worktree_options_mut(|opts| opts.thread_limit = Some(status_threads()))
        .tree_index_track_renames(gitoxide::status::tree_index::TrackRenames::Disabled)
        .index_worktree_rewrites(None)
        .into_iter(patterns)
        .map_err(git_error)?;
    let mut cache = ScmStatusCache::default();
    for item in items.by_ref() {
        let item = item.map_err(git_error)?;
        let status = match &item {
            Item::TreeIndex(change) if change.entry_mode() == Mode::COMMIT => match change {
                Change::Modification {
                    previous_entry_mode,
                    ..
                } if *previous_entry_mode != Mode::COMMIT => Status::INDEX_TYPECHANGE,
                _ => continue,
            },
            Item::TreeIndex(Change::Addition { .. }) => Status::INDEX_NEW,
            Item::TreeIndex(Change::Deletion { .. }) => Status::INDEX_DELETED,
            Item::TreeIndex(Change::Modification {
                entry_mode,
                previous_entry_mode,
                ..
            }) => {
                if entry_mode.bits() >> 12 != previous_entry_mode.bits() >> 12 {
                    Status::INDEX_TYPECHANGE
                } else {
                    Status::INDEX_MODIFIED
                }
            }
            Item::TreeIndex(Change::Rewrite { .. }) => Status::INDEX_RENAMED,
            Item::IndexWorktree(change) => match change.summary() {
                Some(Summary::Added | Summary::Copied) => Status::WT_NEW,
                Some(Summary::Modified) => Status::WT_MODIFIED,
                Some(Summary::Removed) => Status::WT_DELETED,
                Some(Summary::TypeChange) => Status::WT_TYPECHANGE,
                Some(Summary::Conflict) => Status::CONFLICTED,
                Some(Summary::IntentToAdd) => Status::INDEX_NEW | Status::WT_MODIFIED,
                Some(Summary::Renamed) => Status::WT_RENAMED,
                None => continue,
            },
        };
        if let Ok(path) = item.location().to_str() {
            let path = crate::path_utils::normalize(root.join(path));
            *cache.0.entry(path).or_insert(Status::CURRENT) |= status;
        }
    }
    if full {
        let index = &items
            .outcome_mut()
            .ok_or_else(|| Error::Gitoxide("missing status outcome".into()))?
            .worktree_index;
        if paths.is_empty() {
            let additional = index.entries().len().saturating_sub(cache.0.len());
            cache.0.reserve(additional);
            for entry in index.entries() {
                if entry.mode != Mode::COMMIT
                    && let Ok(path) = entry.path(index).to_str()
                {
                    cache
                        .0
                        .entry(crate::path_utils::normalize(root.join(path)))
                        .or_insert(Status::CURRENT);
                }
            }
        } else {
            for (path, key) in paths.iter().zip(relative) {
                if index
                    .entry_by_path(&key)
                    .is_some_and(|entry| entry.mode != Mode::COMMIT)
                {
                    cache.0.entry(path.clone()).or_insert(Status::CURRENT);
                }
            }
        }
    }
    Ok(cache)
}

fn git_error(err: impl std::fmt::Display) -> Error {
    Error::Gitoxide(err.to_string())
}

// Gitoxide starts two producer threads in addition to its workers.
fn status_threads() -> usize {
    crate::parallelism::background_thread_count()
        .saturating_sub(2)
        .max(1)
}
