use super::recency::{GitRecencyConfig, base_path_within_repo};
use super::{ScmStatusCache, Status, main_checkout_root, wait_for_git_index_lock_release};
use crate::error::{Error, Result};
use crate::path_utils::path_in_base;
use crate::scm::{ScmBackend, ScmWorkspace, StatusMap, repository::RepositoryOps};
use ahash::AHashMap;
use gitoxide::bstr::{BStr, BString, ByteSlice};
use gitoxide::diff::index::Change;
use gitoxide::index::entry::Mode;
use gitoxide::status::{Item, UntrackedFiles, index_worktree::iter::Summary};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};

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
            return Ok(ScmStatusCache::Gitoxide(Statuses::default()));
        }
        read_status(self, true, paths)
    }

    fn recency(&self, config: &GitRecencyConfig, base: &Path) -> Option<AHashMap<String, i16>> {
        if !config.enabled || config.max_commits == 0 {
            return None;
        }
        let head = self.head_id().ok()?;
        let mut walk = self.rev_walk([head]);
        if let Some(base) = base_branch(self)
            && let Ok(common) = self.merge_base(head, base)
            && common != head
        {
            walk = walk.with_hidden([common]);
        }
        let subdir = base_path_within_repo(self.workdir()?, base);
        let mut paths = Vec::new();
        Some(config.score_commits(walk.all().ok()?, |info, scores| {
            let info = info.ok()?;
            if info.parent_ids.len() > 1 {
                return None;
            }
            let tree = info.object().ok()?.tree().ok()?;
            let parent = match info.parent_ids.first() {
                Some(id) => self.find_commit(*id).ok()?.tree().ok()?,
                None => self.empty_tree(),
            };
            let mut diff = parent.changes().ok()?;
            diff.options(|opts| {
                opts.track_path().track_rewrites(None);
            });
            paths.clear();
            diff.for_each_to_obtain_tree(&tree, |change| {
                if !change.entry_mode().is_tree()
                    && let Some(path) =
                        path_in_base(&change.location().to_str_lossy(), subdir.as_deref())
                {
                    paths.push(path.to_owned());
                }
                Ok::<_, std::convert::Infallible>(if paths.len() > config.max_files_per_commit {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                })
            })
            .ok()?;
            if paths.len() > config.max_files_per_commit {
                return None;
            }
            for path in paths.drain(..) {
                let score = scores.entry(path).or_default();
                *score = score.saturating_add(1);
            }
            Some(())
        }))
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
    let index = repo.index_or_empty().map_err(git_error)?;
    let mut statuses = Statuses::default();
    let mut patterns = Vec::with_capacity(paths.len());
    for path in paths {
        let relative = git_path(root, path)?;
        let mut pattern = Vec::with_capacity(b":(top,literal)".len() + relative.len());
        pattern.extend_from_slice(b":(top,literal)");
        pattern.extend_from_slice(&relative);
        patterns.push(BString::from(pattern));
        if full
            && index
                .entry_by_path(&relative)
                .is_some_and(|entry| entry.mode != Mode::COMMIT)
        {
            statuses.changes.entry(relative.into_owned()).or_default();
        }
    }
    let items = repo
        .status(gitoxide::progress::Discard)
        .map_err(git_error)?
        .index(gitoxide::worktree::IndexPersistedOrInMemory::Persisted(
            index.clone(),
        ))
        .untracked_files(UntrackedFiles::Files)
        .index_worktree_submodules(None)
        .index_worktree_options_mut(|opts| opts.thread_limit = Some(status_threads()))
        .tree_index_track_renames(gitoxide::status::tree_index::TrackRenames::Disabled)
        .index_worktree_rewrites(None)
        .into_iter(patterns)
        .map_err(git_error)?;
    for item in items {
        match item.map_err(git_error)? {
            Item::TreeIndex(change) => {
                if let Some((path, summary)) = staged_change(change) {
                    statuses.changes.entry(path).or_default().0 = Some(summary);
                }
            }
            Item::IndexWorktree(change) => {
                if let Some(summary) = change.summary() {
                    use gitoxide::status::index_worktree::Item::*;
                    let path = match change {
                        Modification { rela_path, .. } => rela_path,
                        DirectoryContents { entry, .. } => entry.rela_path,
                        Rewrite { dirwalk_entry, .. } => dirwalk_entry.rela_path,
                    };
                    statuses.changes.entry(path).or_default().1 = Some(summary);
                }
            }
        }
    }
    if full && paths.is_empty() {
        statuses.index = Some(index);
    }
    Ok(ScmStatusCache::Gitoxide(statuses))
}

#[derive(Default)]
pub(crate) struct Statuses {
    changes: AHashMap<BString, (Option<Summary>, Option<Summary>)>,
    index: Option<gitoxide::worktree::Index>,
}

impl StatusMap for Statuses {
    fn iter(&self) -> impl Iterator<Item = (&str, Status)> {
        let changed = self
            .changes
            .iter()
            .filter_map(|(path, &(staged, worktree))| {
                let status = match staged {
                    Some(Summary::Added) => Status::INDEX_NEW,
                    Some(Summary::Removed) => Status::INDEX_DELETED,
                    Some(Summary::Modified) => Status::INDEX_MODIFIED,
                    Some(Summary::TypeChange) => Status::INDEX_TYPECHANGE,
                    Some(Summary::Renamed) => Status::INDEX_RENAMED,
                    _ => Status::CURRENT,
                } | match worktree {
                    Some(Summary::Added | Summary::Copied) => Status::WT_NEW,
                    Some(Summary::Modified) => Status::WT_MODIFIED,
                    Some(Summary::Removed) => Status::WT_DELETED,
                    Some(Summary::TypeChange) => Status::WT_TYPECHANGE,
                    Some(Summary::Conflict) => Status::CONFLICTED,
                    Some(Summary::IntentToAdd) => Status::INDEX_NEW | Status::WT_MODIFIED,
                    Some(Summary::Renamed) => Status::WT_RENAMED,
                    None => Status::CURRENT,
                };
                Some((path.to_str().ok()?, status))
            });
        let clean = self.index.iter().flat_map(|index| {
            index.entries().iter().filter_map(|entry| {
                let path = entry.path(index);
                if entry.mode == Mode::COMMIT || self.changes.contains_key(path) {
                    return None;
                }
                Some((path.to_str().ok()?, Status::CURRENT))
            })
        });
        changed.chain(clean)
    }
}

fn staged_change(change: Change) -> Option<(BString, Summary)> {
    if change.entry_mode() == Mode::COMMIT
        && !matches!(&change, Change::Modification { previous_entry_mode, .. } if *previous_entry_mode != Mode::COMMIT)
    {
        return None;
    }
    let (path, summary) = match change {
        Change::Addition { location, .. } => (location, Summary::Added),
        Change::Deletion { location, .. } => (location, Summary::Removed),
        Change::Rewrite { location, .. } => (location, Summary::Renamed),
        Change::Modification {
            location,
            previous_entry_mode,
            entry_mode,
            ..
        } => {
            let summary = if entry_mode.bits() >> 12 == previous_entry_mode.bits() >> 12 {
                Summary::Modified
            } else {
                Summary::TypeChange
            };
            (location, summary)
        }
    };
    Some((path.into_owned(), summary))
}

fn git_path<'a>(root: &Path, path: &'a Path) -> Result<std::borrow::Cow<'a, BStr>> {
    Ok(gitoxide::path::to_unix_separators_on_windows(
        gitoxide::path::try_into_bstr(path.strip_prefix(root)?).map_err(git_error)?,
    ))
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

fn base_branch(repo: &gitoxide::Repository) -> Option<gitoxide::ObjectId> {
    let remote = repo.find_reference("refs/remotes/origin/HEAD").ok();
    let target = remote.as_ref().map(|r| r.target());
    let remote = target
        .as_ref()
        .and_then(|t| t.try_name())
        .and_then(|name| name.as_bstr().to_str().ok())
        .and_then(|s| s.strip_prefix("refs/remotes/origin/"));
    let config = repo.config_snapshot();
    let configured = config.string("init.defaultBranch");
    let head = repo.head_name().ok()?;
    for branch in remote
        .into_iter()
        .chain(configured.as_deref().and_then(|b| b.to_str().ok()))
        .chain(["main", "master"])
    {
        for name in [
            format!("refs/remotes/origin/{branch}"),
            format!("refs/heads/{branch}"),
        ] {
            if let Ok(mut reference) = repo.find_reference(name.as_str()) {
                if head
                    .as_ref()
                    .is_some_and(|head| head.shorten() == branch.as_bytes())
                {
                    return None;
                }
                return reference.peel_to_id().ok().map(|id| id.detach());
            }
        }
    }
    None
}
