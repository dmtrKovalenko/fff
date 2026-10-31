use super::ScmStatusCache;
use crate::scm::ScmStatusWorker;
use crate::scm::{ScmBackend, ScmRepository, ScmWorkspace};
use git2::{Repository, Status};
use gitoxide::diff::index::Change;
use gitoxide::index::entry::Mode;
use gitoxide::status::{Item, index_worktree::iter::Summary};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

pub(super) fn scan(repo: &Repository, full: bool) -> Option<ScmStatusCache> {
    let root = repo.workdir()?;
    match read(root, full) {
        Ok(cache) => Some(cache),
        Err(error) => {
            tracing::warn!(%error, "Gitoxide status failed, using libgit2 from now on");
            GITOXIDE.use_libgit2.store(true, Ordering::Relaxed);
            None
        }
    }
}

fn read(root: &Path, full: bool) -> Result<ScmStatusCache, Box<dyn std::error::Error>> {
    let options =
        gitoxide::open::Options::default().config_overrides(["status.showUntrackedFiles=all"]);
    let repo = gitoxide::open_opts(root, options)?;
    let platform = repo
        .status(gitoxide::progress::Discard)?
        .index_worktree_submodules(None)
        .index_worktree_options_mut(|opts| opts.thread_limit = Some(status_threads()))
        .tree_index_track_renames(gitoxide::status::tree_index::TrackRenames::Disabled);
    let mut items = platform.into_iter(std::iter::empty())?;
    let mut cache = ScmStatusCache::default();
    for item in items.by_ref() {
        let item = item?;
        let status = match &item {
            // libgit2 runs with `exclude_submodules`, so gitlinks are skipped
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
            Item::TreeIndex(Change::Rewrite { .. }) => {
                return Err("use libgit2 for rewrites".into());
            }
            Item::IndexWorktree(change) => match change.summary() {
                Some(Summary::Added) => Status::WT_NEW,
                Some(Summary::Modified) => Status::WT_MODIFIED,
                Some(Summary::Removed) => Status::WT_DELETED,
                Some(Summary::TypeChange) => Status::WT_TYPECHANGE,
                Some(Summary::Conflict) => Status::CONFLICTED,
                Some(Summary::IntentToAdd) => Status::INDEX_NEW | Status::WT_MODIFIED,
                Some(Summary::Renamed | Summary::Copied) => {
                    return Err("use libgit2 for worktree rewrites".into());
                }
                None => continue,
            },
        };
        if let Ok(path) = std::str::from_utf8(item.location()) {
            let path = crate::path_utils::normalize(root.join(path));
            *cache.0.entry(path).or_insert(Status::CURRENT) |= status;
        }
    }
    if full {
        let index = &items
            .outcome_mut()
            .ok_or("missing status outcome")?
            .worktree_index;
        let additional = index.entries().len().saturating_sub(cache.0.len());
        cache.0.reserve(additional);
        for entry in index.entries() {
            if entry.mode != Mode::COMMIT
                && let Ok(path) = std::str::from_utf8(entry.path(index))
            {
                cache
                    .0
                    .entry(crate::path_utils::normalize(root.join(path)))
                    .or_insert(Status::CURRENT);
            }
        }
    }
    Ok(cache)
}

#[derive(Debug)]
pub(crate) struct GitOxide {
    use_libgit2: AtomicBool,
}

pub(crate) static GITOXIDE: GitOxide = GitOxide {
    use_libgit2: AtomicBool::new(false),
};

impl ScmBackend for GitOxide {
    fn discover(&'static self, path: &Path) -> Option<ScmWorkspace> {
        super::backend::discover_git(self, path)
    }

    fn open(&self, root: &Path) -> crate::error::Result<Box<dyn ScmRepository>> {
        super::backend::open_git(root, gitoxide_status)
    }

    fn metadata_dir_name(&self) -> &'static str {
        ".git"
    }

    fn status_metadata_changed(&self, path: &Path) -> bool {
        super::LibGit2.status_metadata_changed(path)
    }

    fn create_worker(&self, paused: Arc<AtomicBool>) -> Arc<dyn ScmStatusWorker> {
        super::worker::GitStatusWorker::with_pause_flag(paused)
    }

    fn spawn_initial_status(&self, workspace: ScmWorkspace) -> JoinHandle<Option<ScmStatusCache>> {
        super::worker::spawn_initial_status(workspace)
    }

    fn watch_dirs(&self, metadata: &Path) -> Vec<PathBuf> {
        super::LibGit2.watch_dirs(metadata)
    }
}

fn gitoxide_status(repo: &Repository, full: bool) -> crate::error::Result<ScmStatusCache> {
    if !GITOXIDE.use_libgit2.load(Ordering::Relaxed)
        && let Some(cache) = scan(repo, full)
    {
        return Ok(cache);
    }
    super::backend::libgit2_status(repo, full)
}

// gix adds two producer threads on top of the worker limit
fn status_threads() -> usize {
    crate::parallelism::background_thread_count()
        .saturating_sub(2)
        .max(1)
}
