use crate::scm::{ScmStatusCache, ScmStatusWorker, ScmWorkspace};
use crate::shared::{SharedFrecency, WeakFilePicker};
use ahash::AHashSet;
use parking_lot::{Condvar, Mutex};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

// we don't really need a queue here
#[derive(Default)]
struct Pending {
    paths: AHashSet<PathBuf>,
    full_rescan: bool,
    shutdown: bool,
    // the consumer took a batch and has not applied it yet
    running: bool,
}

impl Pending {
    fn has_work(&self) -> bool {
        self.full_rescan || !self.paths.is_empty()
    }
}

/// Condvar based queue that is used for batch processing events
pub(super) struct GitStatusWorker {
    state: Mutex<Pending>,
    cv: Condvar,
    consumer_spawned: AtomicBool,
    // owned by `SharedPickerInner` so the pause survives picker re-creation
    paused: Arc<AtomicBool>,
}

impl GitStatusWorker {
    #[cfg(test)]
    fn new() -> Arc<Self> {
        Self::with_pause_flag(Arc::default())
    }

    pub(super) fn with_pause_flag(paused: Arc<AtomicBool>) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(Pending::default()),
            cv: Condvar::new(),
            consumer_spawned: AtomicBool::new(false),
            paused,
        })
    }

    fn wait_and_take(&self) -> Option<Pending> {
        let mut guard = self.state.lock();
        while !guard.shutdown && !guard.has_work() {
            self.cv.wait(&mut guard);
        }
        if guard.shutdown {
            return None;
        }
        let work = std::mem::take(&mut *guard);
        guard.running = true;
        Some(work)
    }

    fn finish_batch(&self) {
        self.state.lock().running = false;
    }

    // Keep slow status reads off the filesystem watcher thread.
    fn spawn_consumer(
        mailbox: Arc<GitStatusWorker>,
        weak_picker: WeakFilePicker,
        frecency: SharedFrecency,
    ) -> std::io::Result<std::thread::JoinHandle<()>> {
        std::thread::Builder::new()
            .name("fff-git-status".into())
            .spawn(move || {
                while let Some(work) = mailbox.wait_and_take() {
                    let Some(picker) = weak_picker.upgrade() else {
                        break;
                    };

                    if work.full_rescan {
                        if let Err(e) = picker.refresh_git_status(&frecency) {
                            tracing::error!("git-status worker: full rescan failed: {e:?}");
                        }
                    } else if !work.paths.is_empty() {
                        let paths: Vec<PathBuf> = work.paths.into_iter().collect();
                        if let Err(e) = picker.update_git_status_for_paths(&paths, &frecency) {
                            tracing::error!("git-status worker: path update failed: {e:?}");
                        }
                    }

                    mailbox.finish_batch();
                }

                tracing::info!("git-status worker stopped");
            })
    }
}

impl ScmStatusWorker for GitStatusWorker {
    fn on_workspace_scan(self: Arc<Self>, weak_picker: WeakFilePicker, frecency: SharedFrecency) {
        if self
            .consumer_spawned
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
            && let Err(error) = Self::spawn_consumer(Arc::clone(&self), weak_picker, frecency)
        {
            self.consumer_spawned.store(false, Ordering::Release);
            tracing::error!(?error, "Failed to spawn Git status worker");
            return;
        }
        self.request_full_rescan();
    }

    fn enqueue_paths(&self, paths: Vec<PathBuf>) {
        let mut guard = self.state.lock();
        if self.paused.load(Ordering::Acquire) {
            return;
        }
        guard.paths.extend(paths);
        drop(guard);
        self.cv.notify_one();
    }

    fn request_full_rescan(&self) {
        let mut guard = self.state.lock();
        if self.paused.load(Ordering::Acquire) {
            return;
        }
        guard.full_rescan = true;
        drop(guard);
        self.cv.notify_one();
    }

    // the caller flips `paused` first, so nothing can be queued after this
    fn drop_pending(&self) {
        let mut guard = self.state.lock();
        guard.paths.clear();
        guard.full_rescan = false;
    }

    // without a consumer nothing will ever run, so there is nothing to wait for
    fn is_idle(&self) -> bool {
        if !self.consumer_spawned.load(Ordering::Acquire) {
            return true;
        }
        let guard = self.state.lock();
        guard.shutdown || (!guard.has_work() && !guard.running)
    }

    fn signal_shutdown(&self) {
        let mut guard = self.state.lock();
        guard.shutdown = true;
        drop(guard);
        self.cv.notify_one();
    }
}

pub(super) fn spawn_initial_status(
    workspace: ScmWorkspace,
) -> std::thread::JoinHandle<Option<ScmStatusCache>> {
    std::thread::spawn(move || {
        workspace
            .open()
            .and_then(|repo| repo.status(false))
            .inspect_err(|e| tracing::error!(?e, "Failed to read SCM status"))
            .ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stays_busy_until_the_taken_batch_is_applied() {
        let worker = GitStatusWorker::new();
        worker.consumer_spawned.store(true, Ordering::Release);
        assert!(worker.is_idle());

        worker.request_full_rescan();
        assert!(!worker.is_idle());

        assert!(worker.wait_and_take().unwrap().full_rescan);
        assert!(
            !worker.is_idle(),
            "queue is empty but the batch is not applied"
        );

        worker.finish_batch();
        assert!(worker.is_idle());
    }
}
