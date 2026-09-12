use ahash::AHashMap;
use git2::{DiffOptions, Oid, Repository};
use std::path::Path;

#[derive(Debug, Clone, Copy)]
pub struct GitRecencyConfig {
    pub enabled: bool,
    pub max_commits: usize,
    pub max_files_per_commit: usize,
}

impl Default for GitRecencyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_commits: 10,
            // Ignore commits that touched every single file in the repo
            max_files_per_commit: 50,
        }
    }
}

const MAX_COMMITS_HARD_CAP: usize = 128;

// Computes per file recency bonuses
#[tracing::instrument(skip(repo), level = tracing::Level::DEBUG)]
pub(crate) fn compute_git_recency(
    repo: &Repository,
    config: &GitRecencyConfig,
    base_path: &Path,
) -> Option<AHashMap<String, i16>> {
    if !config.enabled || config.max_commits == 0 {
        return None;
    }

    // Unborn/orphan HEAD: there is no window to compute from.
    let head_ref = repo.head().ok()?;
    let head = head_ref.target()?;
    let head_branch = head_ref.shorthand().ok().map(str::to_owned);

    let subdir = base_path_within_repo(repo, base_path);
    let max_commits = config.max_commits.min(MAX_COMMITS_HARD_CAP);

    let mut revwalk = repo.revwalk().ok()?;
    revwalk.push(head).ok()?;

    // only if we can resolve default branch (master, main) attempt to use the recency
    if let Some((base_branch, base)) = resolve_base_branch(repo)
        && head_branch.as_deref() != Some(base_branch.as_str())
        && let Ok(merge_base) = repo.merge_base(head, base)
        && merge_base != head
    {
        let _ = revwalk.hide(merge_base);
    }

    let mut scores: AHashMap<String, i16> = AHashMap::new();
    let mut qualifying = 0usize;
    // Bounds total walked commits so histories full of skipped (merge/bulk)
    // commits can't turn the walk into a full history scan.
    let walk_budget = (max_commits * 5).max(64);

    for oid in revwalk.take(walk_budget) {
        if qualifying >= max_commits {
            break;
        }

        let Ok(commit) = oid.and_then(|oid| repo.find_commit(oid)) else {
            continue;
        };

        // Merge commits carry no authored changes; the merged commits are
        // walked on their own anyway.
        if commit.parent_count() > 1 {
            continue;
        }

        let Ok(tree) = commit.tree() else { continue };
        let parent_tree = commit.parent(0).ok().and_then(|p| p.tree().ok());

        // Pathspec prunes the tree recursion to the indexed subtree — libgit2
        // sets the tree iterators' start/end range from the pathspec prefix.
        let mut diff_opts = DiffOptions::new();
        if let Some(subdir) = subdir.as_deref() {
            diff_opts.pathspec(subdir);
        }
        let Ok(diff) =
            repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut diff_opts))
        else {
            continue;
        };

        let deltas = diff.deltas();
        if deltas.len() > config.max_files_per_commit {
            continue;
        }

        for delta in deltas {
            let Some(path_bytes) = delta
                .new_file()
                .path_bytes()
                .or_else(|| delta.old_file().path_bytes())
            else {
                continue;
            };

            let repo_relative = String::from_utf8_lossy(path_bytes);
            // Fold repo-relative down to base_path-relative when indexing a subdir
            let relative_path = match subdir.as_deref() {
                Some(subdir) => match repo_relative
                    .strip_prefix(subdir)
                    .and_then(|rest| rest.strip_prefix('/'))
                {
                    Some(rest) => rest,
                    None => continue,
                },
                None => repo_relative.as_ref(),
            };

            // get-before-insert keeps repeat participations allocation-free
            if let Some(count) = scores.get_mut(relative_path) {
                *count = count.saturating_add(1);
            } else {
                scores.insert(relative_path.to_owned(), 1);
            }
        }

        qualifying += 1;
    }

    tracing::debug!(
        files_scored = scores.len(),
        commits_analyzed = qualifying,
        "git recency computed"
    );

    Some(scores)
}

fn base_path_within_repo(repo: &Repository, base_path: &Path) -> Option<String> {
    let workdir = crate::path_utils::normalize(repo.workdir()?.to_path_buf());
    let subdir = base_path.strip_prefix(workdir).ok()?;
    let subdir = crate::path_utils::to_canonical_slashes(&subdir.to_string_lossy()).into_owned();
    (!subdir.is_empty()).then_some(subdir)
}

// The branch feature work is measured against: `origin/HEAD`, else
// `init.defaultBranch` when configured, else `main`, else `master`.
fn resolve_base_branch(repo: &Repository) -> Option<(String, Oid)> {
    let remote_head = repo
        .find_reference("refs/remotes/origin/HEAD")
        .ok()
        .and_then(|r| {
            r.symbolic_target()
                .ok()??
                .strip_prefix("refs/remotes/origin/")
                .map(str::to_owned)
        });

    let configured = repo
        .config()
        .and_then(|config| config.get_string("init.defaultBranch"))
        .ok()
        .filter(|name| !name.is_empty());

    remote_head
        .as_deref()
        .into_iter()
        .chain(configured.as_deref())
        .chain(["main", "master"])
        .find_map(|branch| {
            Some((branch.to_owned(), {
                // prefer remote branches
                repo.resolve_reference_from_short_name(&format!("origin/{branch}"))
                    .or_else(|_| repo.resolve_reference_from_short_name(branch))
                    .ok()?
                    .target()
            }?))
        })
}
