use crate::error::Result;
pub mod git;
use crate::shared::{SharedFrecency, WeakFilePicker};
use std::sync::{Arc, atomic::AtomicBool};
use std::thread::JoinHandle;

use crate::git_recency::GitRecencyConfig;
use ahash::AHashMap;
use git::LibGit2;
use git2::Status;
use std::path::{Path, PathBuf};

pub(crate) trait ScmBackend: std::fmt::Debug + Sync {
    fn discover(&'static self, path: &Path) -> Option<ScmWorkspace>;
    fn open(&self, root: &Path) -> Result<Box<dyn ScmRepository>>;
    fn metadata_dir_name(&self) -> &'static str;
    fn status_metadata_changed(&self, relative: &Path) -> bool;
    fn create_worker(&self, paused: Arc<AtomicBool>) -> Arc<dyn ScmStatusWorker>;
    fn spawn_initial_status(&self, workspace: ScmWorkspace) -> JoinHandle<Option<ScmStatusCache>>;
    fn watch_dirs(&self, metadata: &Path) -> Vec<PathBuf>;
}

pub(crate) trait ScmStatusWorker: Send + Sync {
    fn on_workspace_scan(self: Arc<Self>, picker: WeakFilePicker, frecency: SharedFrecency);
    fn enqueue_paths(&self, paths: Vec<PathBuf>);
    fn request_full_rescan(&self);
    fn drop_pending(&self);
    fn is_idle(&self) -> bool;
    fn signal_shutdown(&self);
}

pub(crate) trait ScmRepository {
    fn status(&self, full: bool) -> Result<ScmStatusCache>;
    fn status_for_paths(&self, paths: &[PathBuf]) -> Result<ScmStatusCache>;
    fn recency(&self, config: &GitRecencyConfig, base: &Path) -> Option<AHashMap<String, i16>>;
    fn is_ignored(&self, path: &Path) -> bool;
    fn wait_for_write(&self);
}

#[derive(Debug, Clone)]
pub(crate) struct ScmWorkspace {
    pub(crate) backend: &'static dyn ScmBackend,
    pub(crate) workdir: PathBuf,
    pub(crate) metadata: PathBuf,
    pub(crate) shared_root: Option<PathBuf>,
}

impl ScmWorkspace {
    #[cfg(test)]
    pub(crate) fn discover(path: &Path) -> Option<Self> {
        configured_backend().discover(path)
    }

    pub(crate) fn open(&self) -> Result<Box<dyn ScmRepository>> {
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

pub(crate) fn is_metadata_path(path: &Path, backend: Option<&dyn ScmBackend>) -> bool {
    let name = backend.unwrap_or(&LibGit2).metadata_dir_name();
    path.components()
        .any(|component| component.as_os_str() == name)
}

pub(crate) fn configured_backend() -> &'static dyn ScmBackend {
    select_backend(std::env::var("FFF_SCM_BACKEND").ok().as_deref())
}

pub(crate) fn select_backend(name: Option<&str>) -> &'static dyn ScmBackend {
    let default = if cfg!(feature = "gitoxide") {
        "gitoxide"
    } else {
        "libgit2"
    };
    match name.unwrap_or(default) {
        #[cfg(feature = "gitoxide")]
        "gitoxide" => &git::gitoxide::GitOxide,
        "libgit2" => &LibGit2,
        name => {
            tracing::warn!(name, "Unavailable SCM backend, using libgit2");
            &LibGit2
        }
    }
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
