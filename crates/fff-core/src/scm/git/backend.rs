use super::{
    ScmStatusCache, libgit2_status::initial_scan_status_options, main_checkout_root,
    wait_for_git_index_lock_release,
};
use crate::error::Result;
use crate::git_recency::{self, GitRecencyConfig};
use crate::scm::{ScmBackend, ScmWorkspace, repository::RepositoryOps};
use ahash::AHashMap;
use git2::Repository;
use std::path::{Path, PathBuf};

pub(in crate::scm) fn discover(path: &Path) -> Option<ScmWorkspace> {
    super::libgit2_status::tune_libgit2_for_local_reads();
    let repo = Repository::discover(path).ok()?;
    let workdir = crate::path_utils::normalize(repo.workdir()?.to_owned());
    let shared_root = repo
        .is_worktree()
        .then(|| main_checkout_root(repo.commondir()))
        .map(crate::path_utils::normalize);
    Some(ScmWorkspace {
        workdir,
        metadata: repo.path().to_owned(),
        shared_root,
        backend: ScmBackend::LibGit2,
    })
}

impl RepositoryOps for Repository {
    fn status(&self, full: bool) -> Result<ScmStatusCache> {
        if full {
            ScmStatusCache::read_full_status(self)
        } else {
            ScmStatusCache::read_status(self, &mut initial_scan_status_options())
        }
    }

    fn status_for_paths(&self, paths: &[PathBuf]) -> Result<ScmStatusCache> {
        ScmStatusCache::git_status_for_paths(self, paths)
    }

    fn recency(&self, config: &GitRecencyConfig, base: &Path) -> Option<AHashMap<String, i16>> {
        git_recency::compute_git_recency(self, config, base)
    }

    fn is_ignored(&self, path: &Path) -> bool {
        self.is_path_ignored(path) == Ok(true)
    }

    fn wait_for_write(&self) {
        wait_for_git_index_lock_release(self.path());
    }
}
