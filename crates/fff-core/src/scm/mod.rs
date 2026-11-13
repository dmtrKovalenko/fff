use crate::error::Result;
mod backend;
pub mod git;
mod repository;
use crate::shared::{SharedFrecency, WeakFilePicker};
pub(crate) use backend::ScmBackend;
pub(crate) use repository::ScmRepository;
use std::sync::Arc;

use ahash::AHashMap;
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

#[derive(Debug, Clone, Default)]
pub(crate) struct ScmStatusCache(pub(crate) AHashMap<PathBuf, Status>);

impl IntoIterator for ScmStatusCache {
    type Item = (PathBuf, Status);
    type IntoIter = <AHashMap<PathBuf, Status> as IntoIterator>::IntoIter;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl ScmStatusCache {
    pub fn statuses_len(&self) -> usize {
        self.0.len()
    }

    #[inline]
    pub fn lookup_status(&self, full_path: &Path) -> Option<Status> {
        self.0.get(full_path).copied()
    }
}

// Main checkout twin of `path` for a picker at `base_path` keyed by `shared_base`
pub(crate) fn shared_twin(path: &Path, base_path: &Path, shared_base: &Path) -> Option<PathBuf> {
    Some(shared_base.join(path.strip_prefix(base_path).ok()?))
}
