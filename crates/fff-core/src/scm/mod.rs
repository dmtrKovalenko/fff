use crate::error::Result;
mod backend;
pub mod git;
mod repository;
use crate::shared::{SharedFrecency, WeakFilePicker};
pub(crate) use backend::ScmBackend;
pub(crate) use repository::ScmRepository;
use std::sync::Arc;

use git::Status;
use std::path::{Path, PathBuf};

pub(crate) trait ScmStatusWorker: Send + Sync {
    fn on_workspace_scan(self: Arc<Self>, picker: WeakFilePicker, frecency: SharedFrecency);
    fn enqueue_paths(&self, paths: Vec<PathBuf>);
    fn request_full_rescan(&self);
    fn drop_pending(&self);
    fn is_idle(&self) -> bool;
    fn signal_shutdown(&self);
}

#[derive(Debug, Clone)]
pub(crate) struct ScmWorkspace {
    pub(crate) backend: ScmBackend,
    pub(crate) workdir: PathBuf,
    pub(crate) metadata: PathBuf,
    pub(crate) shared_root: Option<PathBuf>,
}

impl ScmWorkspace {
    #[cfg(test)]
    pub(crate) fn discover(path: &Path) -> Option<Self> {
        ScmBackend::configured().discover(path)
    }

    pub(crate) fn open(&self) -> Result<ScmRepository> {
        self.backend.open(&self.workdir)
    }

    // `base_path` re-rooted into the shared checkout, `None` outside linked worktrees
    pub(crate) fn shared_base(&self, base_path: &Path) -> Option<PathBuf> {
        let shared_root = self.shared_root.as_ref()?;
        let relative = match base_path.strip_prefix(&self.workdir) {
            Ok(relative) => relative.to_path_buf(),
            // libgit2 reports a symlink-free workdir
            Err(_) => crate::path_utils::canonicalize(base_path)
                .ok()?
                .strip_prefix(&self.workdir)
                .ok()?
                .to_path_buf(),
        };

        // `join("")` appends a separator, changing the history key of the main checkout
        Some(if relative.as_os_str().is_empty() {
            shared_root.clone()
        } else {
            shared_root.join(relative)
        })
    }
}

pub(crate) fn is_metadata_path(path: &Path, backend: Option<ScmBackend>) -> bool {
    let name = backend.map_or(".git", ScmBackend::metadata_dir_name);
    path.components()
        .any(|component| component.as_os_str() == name)
}

pub(crate) trait StatusMap {
    fn iter(&self) -> impl Iterator<Item = (&str, Status)>;
}

pub(crate) enum ScmStatusCache {
    #[cfg(feature = "gitoxide")]
    Gitoxide(git::gitoxide::Statuses),
    #[cfg(feature = "libgit2")]
    LibGit2(git::libgit2_status::Statuses),
}

impl StatusMap for ScmStatusCache {
    fn iter(&self) -> impl Iterator<Item = (&str, Status)> {
        #[cfg(all(feature = "gitoxide", feature = "libgit2"))]
        {
            use rayon::iter::Either;
            match self {
                Self::Gitoxide(rows) => Either::Left(rows.iter()),
                Self::LibGit2(rows) => Either::Right(rows.iter()),
            }
        }
        #[cfg(all(feature = "gitoxide", not(feature = "libgit2")))]
        {
            let Self::Gitoxide(rows) = self;
            rows.iter()
        }
        #[cfg(all(feature = "libgit2", not(feature = "gitoxide")))]
        {
            let Self::LibGit2(rows) = self;
            rows.iter()
        }
        #[cfg(not(any(feature = "gitoxide", feature = "libgit2")))]
        std::iter::empty()
    }
}

// Main checkout twin of `path` for a picker at `base_path` keyed by `shared_base`
pub(crate) fn shared_twin(path: &Path, base_path: &Path, shared_base: &Path) -> Option<PathBuf> {
    Some(shared_base.join(path.strip_prefix(base_path).ok()?))
}
