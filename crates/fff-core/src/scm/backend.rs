use super::{ScmRepository, ScmStatusCache, ScmStatusWorker, ScmWorkspace, git};
use crate::error::Result;
use std::path::{Path, PathBuf};
use std::sync::{Arc, atomic::AtomicBool};
use std::thread::JoinHandle;

#[derive(Clone, Copy, Debug)]
pub(crate) enum ScmBackend {
    #[cfg(feature = "gitoxide")]
    Gitoxide,
    #[cfg(feature = "libgit2")]
    LibGit2,
    #[cfg(not(any(feature = "gitoxide", feature = "libgit2")))]
    Disabled,
}

impl ScmBackend {
    #[cfg(feature = "gitoxide")]
    const DEFAULT: Self = Self::Gitoxide;
    #[cfg(all(feature = "libgit2", not(feature = "gitoxide")))]
    const DEFAULT: Self = Self::LibGit2;
    #[cfg(not(any(feature = "gitoxide", feature = "libgit2")))]
    const DEFAULT: Self = Self::Disabled;

    pub(crate) fn configured() -> Self {
        #[cfg(all(feature = "gitoxide", feature = "libgit2"))]
        {
            static SELECTED: std::sync::LazyLock<ScmBackend> =
                std::sync::LazyLock::new(|| match std::env::var("FFF_SCM_BACKEND").as_deref() {
                    Ok("libgit2") => ScmBackend::LibGit2,
                    Ok("gitoxide") | Err(_) => ScmBackend::DEFAULT,
                    Ok(name) => {
                        tracing::warn!(name, "Unavailable SCM backend, using the build default");
                        ScmBackend::DEFAULT
                    }
                });
            *SELECTED
        }
        #[cfg(not(all(feature = "gitoxide", feature = "libgit2")))]
        Self::DEFAULT
    }

    pub(crate) fn name(self) -> Option<&'static str> {
        match self {
            #[cfg(feature = "gitoxide")]
            Self::Gitoxide => Some("gitoxide"),
            #[cfg(feature = "libgit2")]
            Self::LibGit2 => Some("libgit2"),
            #[cfg(not(any(feature = "gitoxide", feature = "libgit2")))]
            Self::Disabled => None,
        }
    }

    pub(crate) fn discover(self, path: &Path) -> Option<ScmWorkspace> {
        match self {
            #[cfg(feature = "gitoxide")]
            Self::Gitoxide => git::gitoxide::discover(path),
            #[cfg(feature = "libgit2")]
            Self::LibGit2 => git::backend::discover(path),
            #[cfg(not(any(feature = "gitoxide", feature = "libgit2")))]
            Self::Disabled => {
                let _ = path;
                None
            }
        }
    }

    pub(crate) fn open(self, root: &Path) -> Result<ScmRepository> {
        match self {
            #[cfg(feature = "gitoxide")]
            Self::Gitoxide => {
                let repo = git::gitoxide::open(root)?;
                #[cfg(feature = "libgit2")]
                let repo = Box::new(repo);
                Ok(ScmRepository::Gitoxide(repo))
            }
            #[cfg(feature = "libgit2")]
            Self::LibGit2 => Ok(ScmRepository::LibGit2(git2::Repository::open(root)?)),
            #[cfg(not(any(feature = "gitoxide", feature = "libgit2")))]
            Self::Disabled => Err(crate::Error::InvalidPath(root.to_owned())),
        }
    }

    pub(crate) fn metadata_dir_name(self) -> &'static str {
        ".git"
    }

    pub(crate) fn status_metadata_changed(self, path: &Path) -> bool {
        matches!(path.to_str(), Some("index" | "index.lock" | "HEAD"))
            || path == Path::new("logs/HEAD")
            || matches!(
                path.file_name().and_then(|n| n.to_str()),
                Some("MERGE_HEAD" | "CHERRY_PICK_HEAD" | "REVERT_HEAD")
            )
    }

    pub(crate) fn create_worker(self, paused: Arc<AtomicBool>) -> Arc<dyn ScmStatusWorker> {
        git::worker::GitStatusWorker::with_pause_flag(paused)
    }

    pub(crate) fn spawn_initial_status(
        self,
        workspace: ScmWorkspace,
    ) -> JoinHandle<Option<ScmStatusCache>> {
        git::worker::spawn_initial_status(workspace)
    }

    pub(crate) fn watch_dirs(self, metadata: &Path) -> Vec<PathBuf> {
        vec![metadata.to_owned(), metadata.join("logs")]
    }
}

#[cfg(all(test, not(all(feature = "gitoxide", feature = "libgit2"))))]
#[test]
fn fixed_backend_takes_no_space() {
    assert_eq!(std::mem::size_of::<ScmBackend>(), 0);
}
