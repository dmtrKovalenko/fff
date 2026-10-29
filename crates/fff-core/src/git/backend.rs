use super::{ScmStatusCache, initial_scan_status_options, main_checkout_root};
use crate::error::Result;
use crate::git_recency::{self, GitRecencyConfig};
use crate::scm::{ScmBackend, ScmRepository, ScmWorkspace};
use ahash::AHashMap;
use git2::Repository;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Debug, Copy, Clone)]
pub(crate) enum GitBackend {
    Libgit2,
    #[cfg(feature = "gitoxide-status")]
    Gitoxide,
}

impl ScmBackend for GitBackend {
    fn discover(&'static self, path: &Path) -> Option<ScmWorkspace> {
        super::tune_libgit2_for_local_reads();
        let repo = Repository::discover(path).ok()?;
        let workdir = crate::path_utils::normalize(repo.workdir()?.to_owned());
        let shared_root = repo
            .is_worktree()
            .then(|| main_checkout_root(&repo))
            .map(crate::path_utils::normalize);
        Some(ScmWorkspace {
            workdir,
            metadata: repo.path().to_owned(),
            shared_root,
            backend: self,
        })
    }

    fn open(&self, root: &Path) -> Result<Box<dyn ScmRepository>> {
        Ok(Box::new(GitRepository {
            repo: Repository::open(root)?,
            backend: *self,
        }))
    }

    fn status_metadata_changed(&self, path: &Path) -> bool {
        matches!(path.to_str(), Some("index" | "index.lock" | "HEAD"))
            || path == Path::new("logs/HEAD")
            || matches!(
                path.file_name().and_then(|n| n.to_str()),
                Some("MERGE_HEAD" | "CHERRY_PICK_HEAD" | "REVERT_HEAD")
            )
    }

    fn watch_dirs(&self, metadata: &Path) -> Vec<PathBuf> {
        vec![metadata.to_owned(), metadata.join("logs")]
    }
}

struct GitRepository {
    repo: Repository,
    backend: GitBackend,
}

impl ScmRepository for GitRepository {
    fn status(&self, full: bool) -> Result<ScmStatusCache> {
        match self.backend {
            GitBackend::Libgit2 => (),
            #[cfg(feature = "gitoxide-status")]
            GitBackend::Gitoxide => {
                if let Some(cache) = super::gitoxide::scan(&self.repo, full) {
                    return Ok(cache);
                }
            }
        }
        if full {
            ScmStatusCache::read_full_status(&self.repo)
        } else {
            ScmStatusCache::read_status(&self.repo, &mut initial_scan_status_options())
        }
    }

    fn status_for_paths(&self, paths: &[PathBuf]) -> Result<ScmStatusCache> {
        ScmStatusCache::git_status_for_paths(&self.repo, paths)
    }

    fn recency(&self, config: &GitRecencyConfig, base: &Path) -> Option<AHashMap<String, i16>> {
        git_recency::compute_git_recency(&self.repo, config, base)
    }

    fn is_ignored(&self, path: &Path) -> bool {
        self.repo.is_path_ignored(path) == Ok(true)
    }

    fn wait_for_write(&self) {
        wait_for_git_index_lock_release(self.repo.path());
    }
}

fn wait_for_git_index_lock_release(git_dir: &Path) {
    const GIT_LOCK_POLL: Duration = Duration::from_millis(10);
    const GIT_LOCK_MAX_WAIT: Duration = Duration::from_millis(500);

    let lock = git_dir.join("index.lock");
    // Fast path: no lock present.
    if !lock.exists() {
        return;
    }
    let deadline = Instant::now() + GIT_LOCK_MAX_WAIT;
    while lock.exists() && Instant::now() < deadline {
        std::thread::sleep(GIT_LOCK_POLL);
    }
    if lock.exists() {
        tracing::warn!(
            "Proceeding with git status refresh despite lingering \
             .git/index.lock at {} — will retry once it clears",
            lock.display()
        );
    }
}
