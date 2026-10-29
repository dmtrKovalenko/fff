use super::ScmStatusCache;
use crate::scm::{ScmBackend, ScmRepository, ScmWorkspace};
use git2::{Repository, Status};
use gix::diff::index::Change;
use gix::index::entry::Mode;
use gix::status::{Item, index_worktree::iter::Summary};
use std::path::{Path, PathBuf};

pub(super) fn scan(repo: &Repository, full: bool) -> Option<ScmStatusCache> {
    let root = repo.workdir()?;
    match read(root, full) {
        Ok(cache) => Some(cache),
        Err(error) => {
            tracing::debug!(%error, "Falling back to libgit2 status");
            None
        }
    }
}

fn read(root: &Path, full: bool) -> Result<ScmStatusCache, Box<dyn std::error::Error>> {
    let options = gix::open::Options::default().config_overrides(["status.showUntrackedFiles=all"]);
    let repo = gix::open_opts(root, options)?;
    let platform = repo
        .status(gix::progress::Discard)?
        .index_worktree_submodules(None)
        .index_worktree_options_mut(|opts| opts.thread_limit = Some(4))
        .tree_index_track_renames(gix::status::tree_index::TrackRenames::Disabled);
    let mut items = platform.into_iter(std::iter::empty())?;
    let mut cache = ScmStatusCache::default();
    for item in items.by_ref() {
        let item = item?;
        let status = match &item {
            Item::TreeIndex(Change::Addition { entry_mode, .. }) if *entry_mode != Mode::COMMIT => {
                Status::INDEX_NEW
            }
            Item::TreeIndex(Change::Deletion { entry_mode, .. }) if *entry_mode != Mode::COMMIT => {
                Status::INDEX_DELETED
            }
            Item::TreeIndex(Change::Modification {
                entry_mode,
                previous_entry_mode,
                ..
            }) if *entry_mode != Mode::COMMIT && *previous_entry_mode != Mode::COMMIT => {
                if entry_mode.bits() >> 12 != previous_entry_mode.bits() >> 12 {
                    Status::INDEX_TYPECHANGE
                } else {
                    Status::INDEX_MODIFIED
                }
            }
            Item::IndexWorktree(change) => match change.summary() {
                Some(Summary::Added) => Status::WT_NEW,
                Some(Summary::Modified) => Status::WT_MODIFIED,
                Some(Summary::Removed) => Status::WT_DELETED,
                Some(Summary::TypeChange) => Status::WT_TYPECHANGE,
                None => continue,
                _ => return Err("use libgit2 for uncommon worktree status".into()),
            },
            _ => return Err("use libgit2 for gitlinks or rewrites".into()),
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
pub(crate) struct GitOxide;

impl ScmBackend for GitOxide {
    fn discover(&'static self, path: &Path) -> Option<ScmWorkspace> {
        super::backend::discover_git(self, path)
    }

    fn open(&self, root: &Path) -> crate::error::Result<Box<dyn ScmRepository>> {
        super::backend::open_git(root, gitoxide_status)
    }

    fn status_metadata_changed(&self, path: &Path) -> bool {
        super::LibGit2.status_metadata_changed(path)
    }

    fn watch_dirs(&self, metadata: &Path) -> Vec<PathBuf> {
        super::LibGit2.watch_dirs(metadata)
    }
}

fn gitoxide_status(repo: &Repository, full: bool) -> crate::error::Result<ScmStatusCache> {
    match scan(repo, full) {
        Some(cache) => Ok(cache),
        None => super::backend::libgit2_status(repo, full),
    }
}
