use crate::shared::{SharedFrecency, WeakFilePicker};
use ahash::AHashSet;
use parking_lot::{Condvar, Mutex};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

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
pub(crate) struct GitStatusWorker {
    state: Mutex<Pending>,
    cv: Condvar,
    consumer_spawned: AtomicBool,
    // owned by `SharedPickerInner` so the pause survives picker re-creation
    paused: Arc<AtomicBool>,
    head_log_len: AtomicU64,
}

impl GitStatusWorker {
    pub(crate) fn new() -> Arc<Self> {
        Self::with_pause_flag(Arc::default())
    }

    pub(crate) fn with_pause_flag(paused: Arc<AtomicBool>) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(Pending::default()),
            cv: Condvar::new(),
            consumer_spawned: AtomicBool::new(false),
            paused,
            head_log_len: AtomicU64::new(0),
        })
    }

    // HEAD moves append to `logs/HEAD`; `git reflog expire` (gc, maintenance)
    // rewrites it with the same or fewer entries and must not trigger a refresh
    pub(crate) fn head_log_appended(&self, head_log: &Path) -> bool {
        let len = std::fs::metadata(head_log).map_or(0, |m| m.len());
        self.head_log_len.swap(len, Ordering::AcqRel) < len
    }

    pub(crate) fn spawn_once(
        self: &Arc<Self>,
        weak_picker: WeakFilePicker,
        frecency: SharedFrecency,
    ) {
        if self
            .consumer_spawned
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            Self::spawn_consumer(Arc::clone(self), weak_picker, frecency);
        }
    }

    pub(crate) fn enqueue_paths<I>(&self, paths: I)
    where
        I: IntoIterator<Item = PathBuf>,
    {
        let mut guard = self.state.lock();
        if self.paused.load(Ordering::Acquire) {
            return;
        }
        guard.paths.extend(paths);
        drop(guard);
        self.cv.notify_one();
    }

    pub(crate) fn request_full_rescan(&self) {
        let mut guard = self.state.lock();
        if self.paused.load(Ordering::Acquire) {
            return;
        }
        guard.full_rescan = true;
        drop(guard);
        self.cv.notify_one();
    }

    // the caller flips `paused` first, so nothing can be queued after this
    pub(crate) fn drop_pending(&self) {
        let mut guard = self.state.lock();
        guard.paths.clear();
        guard.full_rescan = false;
    }

    // without a consumer nothing will ever run, so there is nothing to wait for
    pub(crate) fn is_idle(&self) -> bool {
        if !self.consumer_spawned.load(Ordering::Acquire) {
            return true;
        }
        let guard = self.state.lock();
        guard.shutdown || (!guard.has_work() && !guard.running)
    }

    pub(crate) fn signal_shutdown(&self) {
        let mut guard = self.state.lock();
        guard.shutdown = true;
        drop(guard);
        self.cv.notify_one();
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

    // the problem: git status update can take a lot of time especially on big repositories
    // and there is unpredictable wait time on the lock file if huge commit is going so we have to
    // spawn a separate thread to guartee that notify handler is unlocked even if git update takes a
    // lot of time on every event burst (pretty cheap as this thread is going to sleep 99.9% of time)
    fn spawn_consumer(
        mailbox: Arc<GitStatusWorker>,
        weak_picker: WeakFilePicker,
        frecency: SharedFrecency,
    ) {
        let _ = std::thread::Builder::new()
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
            .inspect_err(|err| tracing::error!(?err, "Failed to spawn git status worker"));
    }
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

    #[test]
    fn head_log_refreshes_only_on_append() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("HEAD");
        let worker = GitStatusWorker::new();
        let one = "0000 1111 t <t@t> 1 +0000\tcommit (initial): one\n";
        let two = "1111 2222 t <t@t> 2 +0000\tcommit: two\n";

        assert!(!worker.head_log_appended(&log), "missing reflog");
        std::fs::write(&log, one).unwrap();
        assert!(worker.head_log_appended(&log), "first commit");
        std::fs::write(&log, format!("{one}{two}")).unwrap();
        assert!(worker.head_log_appended(&log), "commit appends");
        std::fs::write(&log, format!("{one}{two}")).unwrap();
        assert!(!worker.head_log_appended(&log), "identical expire rewrite");
        std::fs::write(&log, two).unwrap();
        assert!(!worker.head_log_appended(&log), "expire dropped entries");
        std::fs::write(&log, format!("{two}{one}")).unwrap();
        assert!(worker.head_log_appended(&log), "append after expire");
        std::fs::remove_file(&log).unwrap();
        assert!(!worker.head_log_appended(&log), "reflog deleted");
        std::fs::write(&log, one).unwrap();
        assert!(worker.head_log_appended(&log), "reflog recreated");
    }
}
