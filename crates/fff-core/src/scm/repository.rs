use super::ScmStatusCache;
use crate::error::Result;
use crate::git_recency::GitRecencyConfig;
use ahash::AHashMap;
use std::path::{Path, PathBuf};

pub(crate) enum ScmRepository {
    #[cfg(all(feature = "gitoxide", not(feature = "libgit2")))]
    Gitoxide(gitoxide::Repository),
    #[cfg(all(feature = "gitoxide", feature = "libgit2"))]
    Gitoxide(Box<gitoxide::Repository>),
    #[cfg(feature = "libgit2")]
    LibGit2(git2::Repository),
}

#[cfg(any(feature = "gitoxide", feature = "libgit2"))]
pub(crate) trait RepositoryOps {
    fn status(&self, full: bool) -> Result<ScmStatusCache>;
    fn status_for_paths(&self, paths: &[PathBuf]) -> Result<ScmStatusCache>;
    fn recency(&self, config: &GitRecencyConfig, base: &Path) -> Option<AHashMap<String, i16>>;
    fn is_ignored(&self, path: &Path) -> bool;
    fn wait_for_write(&self);
}

macro_rules! call_repository {
    ($repo:expr, $method:ident $(, $arg:expr)*) => {
        match *$repo {
            #[cfg(feature = "gitoxide")]
            ScmRepository::Gitoxide(ref repo) => <gitoxide::Repository as RepositoryOps>::$method(repo $(, $arg)*),
            #[cfg(feature = "libgit2")]
            ScmRepository::LibGit2(ref repo) => RepositoryOps::$method(repo $(, $arg)*),
        }
    };
}

#[cfg_attr(
    not(any(feature = "gitoxide", feature = "libgit2")),
    allow(unused_variables)
)]
impl ScmRepository {
    pub(crate) fn status(&self, full: bool) -> Result<ScmStatusCache> {
        call_repository!(self, status, full)
    }

    pub(crate) fn status_for_paths(&self, paths: &[PathBuf]) -> Result<ScmStatusCache> {
        call_repository!(self, status_for_paths, paths)
    }

    pub(crate) fn recency(
        &self,
        config: &GitRecencyConfig,
        base: &Path,
    ) -> Option<AHashMap<String, i16>> {
        call_repository!(self, recency, config, base)
    }

    pub(crate) fn is_ignored(&self, path: &Path) -> bool {
        call_repository!(self, is_ignored, path)
    }

    pub(crate) fn wait_for_write(&self) {
        call_repository!(self, wait_for_write)
    }
}
