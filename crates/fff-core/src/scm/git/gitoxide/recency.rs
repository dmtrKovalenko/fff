use crate::git_recency::{GitRecencyConfig, MAX_COMMITS_HARD_CAP, base_path_within_repo};
use ahash::AHashMap;
use gitoxide::{Repository, bstr::ByteSlice};
use std::{ops::ControlFlow, path::Path};

pub(super) fn scores(
    repo: &Repository,
    config: &GitRecencyConfig,
    base: &Path,
) -> Option<AHashMap<String, i16>> {
    if !config.enabled || config.max_commits == 0 {
        return None;
    }
    let head = repo.head_id().ok()?;
    let mut walk = repo.rev_walk([head]);
    if let Some(base) = base_branch(repo)
        && let Ok(common) = repo.merge_base(head, base)
        && common != head
    {
        walk = walk.with_hidden([common]);
    }
    let subdir = base_path_within_repo(repo.workdir()?, base);
    let limit = config.max_commits.min(MAX_COMMITS_HARD_CAP);
    let mut scores = AHashMap::<String, i16>::new();
    let mut qualifying = 0;
    let mut paths = Vec::new();
    for info in walk.all().ok()?.take((limit * 5).max(64)).flatten() {
        if info.parent_ids.len() > 1 {
            continue;
        }
        let Some(tree) = info.object().ok().and_then(|commit| commit.tree().ok()) else {
            continue;
        };
        let parent = match info.parent_ids.first() {
            Some(id) => {
                let Some(tree) = repo
                    .find_commit(*id)
                    .ok()
                    .and_then(|commit| commit.tree().ok())
                else {
                    continue;
                };
                tree
            }
            None => repo.empty_tree(),
        };
        let Ok(mut diff) = parent.changes() else {
            continue;
        };
        diff.options(|options| {
            options.track_path().track_rewrites(None);
        });
        paths.clear();
        let result = diff.for_each_to_obtain_tree(&tree, |change| {
            if change.entry_mode().is_tree() {
                return Ok(ControlFlow::Continue(()));
            }
            let path = change.location().to_str_lossy();
            let path = match subdir.as_deref() {
                Some(prefix) => path.strip_prefix(prefix).and_then(|s| s.strip_prefix('/')),
                None => Some(path.as_ref()),
            };
            if let Some(path) = path {
                paths.push(path.to_owned());
            }
            Ok::<_, std::convert::Infallible>(if paths.len() > config.max_files_per_commit {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            })
        });
        if result.is_err() || paths.len() > config.max_files_per_commit {
            continue;
        }
        for path in paths.drain(..) {
            *scores.entry(path).or_default() += 1;
        }
        qualifying += 1;
        if qualifying == limit {
            break;
        }
    }
    Some(scores)
}

fn base_branch(repo: &Repository) -> Option<gitoxide::ObjectId> {
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
