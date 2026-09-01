use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard, Weak};
use std::time::{Duration, Instant};

use crate::dbs::lmdb::{LmdbStore, spawn_lmdb_gc};
use crate::error::Error;
use crate::file_picker::FilePicker;
use crate::frecency::FrecencyTracker;
use crate::git::GitStatusCache;
use crate::query_tracker::QueryTracker;
use crate::rescan_stats::{RescanCounters, RescanReason, RescanStats};
use crate::rescan_throttle::RescanThrottle;
use crate::scan::ScanJob;
use crate::watch::{WatchEvent, WatchId, WatchOptions, WatchRegistry};
use git2::Repository;

/// Poll `.git/index.lock` until it disappears (git write completed), giving up
/// after [`GIT_LOCK_MAX_WAIT`]. Used by [`SharedPicker::refresh_git_status`]
/// to avoid reading a half-updated index when the watcher fires mid-`git add`.
///
/// The wait is bounded and cheap: the lock file is typically cleared within
/// a few milliseconds of the git command exiting.
fn wait_for_git_index_lock_release(git_root: &Path) {
    const GIT_LOCK_POLL: Duration = Duration::from_millis(10);
    const GIT_LOCK_MAX_WAIT: Duration = Duration::from_millis(500);

    let lock = git_root.join(".git").join("index.lock");
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

/// Poll `done` every 10ms until it returns `true`, or until `timeout` elapses.
/// Returns `true` if the condition was met, `false` on timeout.
fn poll_until(timeout: Duration, mut done: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while !done() {
        if start.elapsed() >= timeout {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    true
}

/// Thread-safe shared handle to the [`FilePicker`] instance.
/// This accumulates only asynchronous non-blocking operations against the
/// file picker: creating, triggering various rescans and so on.
///
/// For blocking access use internal picker via `.read()` or `.write()`
///
/// ```ignore
/// let shared_picker = SharedFilePicker::default();
///
/// if let Some(picker) = shared_picker.read()?.as_ref() {
///     let files = picker.fuzzy_search(&query, options);
///     println!("Found {} files", files.len());
/// } else {
///     println!("Picker not initialized");
/// }
/// ```
#[derive(Clone, Default)]
pub struct SharedFilePicker(pub(crate) Arc<SharedPickerInner>);

pub struct SharedPickerInner {
    picker: parking_lot::RwLock<Option<FilePicker>>,
    /// Watch subscriptions live outside the picker lock so delivery and
    /// (un)subscribing never contend with searches.
    watchers: Arc<WatchRegistry>,
    rescans: RescanCounters,
    rescan_throttle: RescanThrottle,
}

impl Default for SharedPickerInner {
    fn default() -> Self {
        Self {
            picker: parking_lot::RwLock::new(None),
            watchers: Arc::new(WatchRegistry::default()),
            rescans: RescanCounters::default(),
            rescan_throttle: RescanThrottle::default(),
        }
    }
}

/// Non-owning handle to a [`SharedPicker`].
#[derive(Clone)]
pub(crate) struct WeakFilePicker(Weak<SharedPickerInner>);

impl WeakFilePicker {
    /// Try to promote the weak handle back to a strong [`SharedPicker`].
    ///
    /// Returns `None` once every strong `SharedPicker` clone has been
    /// dropped. Callers should treat that as "the picker is being
    /// torn down" and exit their current iteration cleanly.
    pub(crate) fn upgrade(&self) -> Option<SharedFilePicker> {
        self.0.upgrade().map(SharedFilePicker)
    }
}

impl std::fmt::Debug for SharedFilePicker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("SharedPicker").field(&"..").finish()
    }
}

impl SharedFilePicker {
    pub fn read(&self) -> Result<parking_lot::RwLockReadGuard<'_, Option<FilePicker>>, Error> {
        Ok(self.0.picker.read())
    }

    pub fn write(&self) -> Result<parking_lot::RwLockWriteGuard<'_, Option<FilePicker>>, Error> {
        Ok(self.0.picker.write())
    }

    /// Signal the background scan to cancel. Non-blocking: post-scan
    /// threads check this flag and bail out at their next cancellation point.
    pub fn cancel(&self) {
        if let Ok(guard) = self.read()
            && let Some(picker) = guard.as_ref()
        {
            picker.cancel();
        }
    }

    /// Produce a non-owning handle to the same inner picker.
    /// Use it if you don't need to block internal threads from dropping while owning this ref
    pub(crate) fn weaken(&self) -> WeakFilePicker {
        WeakFilePicker(Arc::downgrade(&self.0))
    }

    /// Return `true` if this is an instance of the picker that requires a complicated post-scan
    /// indexing/cache warmup job. The indexing is not crazy but it takes time.
    pub fn need_complex_rebuild(&self) -> bool {
        let guard = self.0.picker.read();
        guard
            .as_ref()
            .is_some_and(|p| p.has_mmap_cache() || p.has_content_indexing())
    }

    /// Block until the background filesystem scan finishes.
    /// Returns `true` if scan completed, `false` on timeout.
    pub fn wait_for_scan(&self, timeout: Duration) -> bool {
        let signal = {
            let guard = self.0.picker.read();
            match &*guard {
                Some(picker) => Arc::clone(&picker.signals.scanning),
                None => return true,
            }
        };

        poll_until(timeout, || {
            !signal.load(std::sync::atomic::Ordering::Acquire)
        })
    }

    /// Block until the background file watcher is ready.
    /// Returns `true` if watcher ready, `false` on timeout.
    pub fn wait_for_watcher(&self, timeout: Duration) -> bool {
        let watch_ready_signal = {
            let guard = self.0.picker.read();
            match &*guard {
                Some(picker) => Arc::clone(&picker.signals.watcher_ready),
                None => return true,
            }
        };

        poll_until(timeout, || {
            watch_ready_signal.load(std::sync::atomic::Ordering::Acquire)
        })
    }

    /// Blocks until both the filesystem walk and post-scan indexing are done.
    /// Returns true once scanning=false AND post_scan_indexing_active=false.
    pub fn wait_for_indexing_complete(&self, timeout: Duration) -> bool {
        let (scanning, post_scan_active) = {
            let guard = self.0.picker.read();
            match &*guard {
                Some(picker) => (
                    Arc::clone(&picker.signals.scanning),
                    Arc::clone(&picker.signals.post_scan_indexing_active),
                ),
                None => return true,
            }
        };

        poll_until(timeout, || {
            !scanning.load(std::sync::atomic::Ordering::Acquire)
                && !post_scan_active.load(std::sync::atomic::Ordering::Acquire)
        })
    }

    /// Trigger a full filesystem rescan without blocking the caller.
    /// Performs a safe async rescan. Guarantees only single active rescan per picker.
    /// If many rescans requested the last one guaranteed to be finished.
    pub fn trigger_full_rescan_async(&self, shared_frecency: &SharedFrecency) -> Result<(), Error> {
        self.trigger_full_rescan_with_reason(shared_frecency, RescanReason::Explicit)
            .map(|_| ())
    }

    /// Returns admitted and throttled rescan requests by reason.
    /// Counters start at picker creation or the last reset.
    pub fn rescan_stats(&self) -> RescanStats {
        self.0.rescans.snapshot()
    }

    pub fn reset_rescan_stats(&self) {
        self.0.rescans.reset();
    }

    /// Returns `Ok(true)` when a rescan was started (or queued behind an
    /// active scan) and `Ok(false)` when the request was throttled — the
    /// caller must then fall back to incremental event processing.
    pub(crate) fn trigger_full_rescan_with_reason(
        &self,
        shared_frecency: &SharedFrecency,
        reason: RescanReason,
    ) -> Result<bool, Error> {
        // for giant folders we have no other choice other than throttling rescans
        // if user is running application in millions of files with a ton of rescan events
        // we drop / throttle some of requests to avoid constant burst of IO
        if reason == RescanReason::Explicit {
            self.0.rescan_throttle.note_explicit_scan();
        } else if !self.check_rescan_throttle(reason) {
            return Ok(false);
        }

        self.0.rescans.record(reason);

        match ScanJob::new_rescan(self, shared_frecency)? {
            Some(job) => {
                job.spawn();
            }
            None => {
                // we can not abort the ongoing sync, but if the events
                if let Ok(guard) = self.read()
                    && let Some(picker) = guard.as_ref()
                {
                    picker
                        .scan_signals()
                        .rescan_pending
                        .store(true, std::sync::atomic::Ordering::Release);
                    tracing::info!(
                        "Full rescan requested while another scan is active — \
                         deferred via rescan_pending flag"
                    );
                }
            }
        }
        Ok(true)
    }

    fn check_rescan_throttle(&self, reason: RescanReason) -> bool {
        let (live_files, has_git) = self
            .read()
            .ok()
            .and_then(|guard| {
                guard
                    .as_ref()
                    .map(|picker| (picker.live_file_count(), picker.has_git_repo()))
            })
            .unwrap_or((0, false));

        if self.0.rescan_throttle.admit(live_files, has_git) {
            return true;
        }

        self.0.rescans.record_throttled(reason);
        tracing::debug!(%reason, live_files, "Rescan throttled, skipping");
        false
    }

    /// Subscribe to filesystem changes matching `pattern`.
    ///
    /// Patterns may be base-relative globs (./ works), exact paths inside the indexed
    /// tree, or existing directories. An empty pattern watches the whole tree.
    ///
    /// Events are debounced over a 50-ms window and submitted in batches of at most 128 events.
    /// Gitignored and other ignored files are never triggering watcher.
    pub fn watch(
        &self,
        pattern: &str,
        options: WatchOptions,
        callback: impl Fn(WatchId, &[WatchEvent]) + Send + Sync + 'static,
    ) -> Result<WatchId, Error> {
        let (base_path, has_watcher, watcher_ready) = {
            let guard = self.read()?;
            let picker = guard.as_ref().ok_or(Error::FilePickerMissing)?;

            (
                picker.base_path().to_path_buf(),
                picker.has_watcher(),
                picker.is_watcher_ready(),
            )
        };

        if !has_watcher {
            return Err(Error::WatcherDisabled);
        }
        if !watcher_ready {
            return Err(Error::WatcherNotReady);
        }

        self.0
            .watchers
            .subscribe(&base_path, pattern, options, Box::new(callback))
    }

    /// Remove a watch subscription. Returns `true` if the id was active.
    pub fn unwatch(&self, id: WatchId) -> bool {
        self.0.watchers.unsubscribe(id)
    }

    /// Return whether a watch subscription is active.
    pub fn is_watch_active(&self, id: WatchId) -> bool {
        self.0.watchers.contains(id)
    }

    /// Remove every subscription without waiting for an executing callback.
    pub fn shutdown_watches(&self) {
        self.0.watchers.shutdown();
    }

    /// Remove every subscription and wait for an executing callback.
    /// When called by that callback, it does not wait on itself.
    pub fn shutdown_watches_and_wait(&self) {
        self.0.watchers.shutdown_and_wait();
    }

    pub(crate) fn rebase_watches(&self, base_path: &Path) {
        self.0.watchers.rebase(base_path);
    }

    pub(crate) fn watch_registry(&self) -> &Arc<WatchRegistry> {
        &self.0.watchers
    }

    /// Refresh git statuses for all indexed files
    #[tracing::instrument(level = "info", skip_all)]
    pub fn refresh_git_status(&self, shared_frecency: &SharedFrecency) -> Result<usize, Error> {
        use tracing::debug;

        let git_status = {
            let git_root = {
                let guard = self.read()?;
                let Some(ref picker) = *guard else {
                    return Err(Error::FilePickerMissing);
                };
                picker.git_root().map(|p| p.to_path_buf())
            };

            debug!(?git_root, "Refreshing git status for picker");

            if let Some(ref root) = git_root {
                wait_for_git_index_lock_release(root);
            }

            GitStatusCache::read_git_status(
                git_root.as_deref(),
                &mut crate::git::default_status_options(),
            )
        };

        let mut guard = self.write()?;
        let picker = guard.as_mut().ok_or(Error::FilePickerMissing)?;

        let statuses_count = if let Some(git_status) = git_status {
            let count = git_status.statuses_len();
            picker.update_git_statuses(git_status, shared_frecency)?;
            count
        } else {
            0
        };

        Ok(statuses_count)
    }

    /// Recompute and apply git status for a specific set of paths.
    pub fn update_git_status_for_paths(
        &self,
        paths: &[PathBuf],
        shared_frecency: &SharedFrecency,
    ) -> Result<(), Error> {
        if paths.is_empty() {
            return Ok(());
        }

        let git_root = {
            let guard = self.read()?;
            let Some(ref picker) = *guard else {
                return Err(Error::FilePickerMissing);
            };
            picker.git_root().map(|p| p.to_path_buf())
        };
        let Some(git_root) = git_root else {
            return Ok(());
        };

        wait_for_git_index_lock_release(&git_root);

        let repo = Repository::open(&git_root)?;
        let status = GitStatusCache::git_status_for_paths(&repo, paths)?;

        let mut guard = self.write()?;
        let picker = guard.as_mut().ok_or(Error::FilePickerMissing)?;
        picker.update_git_statuses(status, shared_frecency)
    }
}

/// Thread-safe shared handle to an LMDB-backed store. A disabled (`noop`)
/// instance silently ignores writes. See the [`SharedFrecency`] and
/// [`SharedQueryTracker`] aliases.
///
/// `LmdbStore` is intentionally crate-private, so the store type is sealed:
/// only `FrecencyTracker` / `QueryTracker` can ever instantiate this.
#[allow(private_bounds)]
pub struct SharedDb<T: LmdbStore> {
    inner: Arc<RwLock<Option<T>>>,
    enabled: bool,
}

// Hand-written to avoid a spurious `T: Clone` bound — `Arc` is always `Clone`.
impl<T: LmdbStore> Clone for SharedDb<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            enabled: self.enabled,
        }
    }
}

impl<T: LmdbStore> Default for SharedDb<T> {
    fn default() -> Self {
        Self {
            inner: Arc::new(RwLock::new(None)),
            enabled: true,
        }
    }
}

impl<T: LmdbStore> std::fmt::Debug for SharedDb<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("SharedDb").field(&T::LABEL).finish()
    }
}

#[allow(private_bounds)]
impl<T: LmdbStore> SharedDb<T> {
    /// Creates a disabled instance that silently ignores all writes.
    pub fn noop() -> Self {
        Self {
            inner: Arc::new(RwLock::new(None)),
            enabled: false,
        }
    }

    pub fn read(&self) -> Result<RwLockReadGuard<'_, Option<T>>, Error> {
        self.inner.read().map_err(|_| Error::AcquireFrecencyLock)
    }

    pub fn write(&self) -> Result<RwLockWriteGuard<'_, Option<T>>, Error> {
        self.inner.write().map_err(|_| Error::AcquireFrecencyLock)
    }

    /// Initialize the store + spawn GC in the background. No-op when disabled.
    pub fn init(&self, tracker: T) -> Result<(), Error> {
        if !self.enabled {
            return Ok(());
        }

        {
            let mut guard = self.write()?;
            *guard = Some(tracker);
        }

        // GC holds a read guard on this lock, so destroy / re-init wait won't race
        spawn_lmdb_gc(self.inner.clone());
        Ok(())
    }

    /// Drop the in-memory tracker and delete the on-disk database directory.
    ///
    /// Returns `Ok(Some(path))` with the deleted path, or `Ok(None)` if no tracker was initialized.
    pub fn destroy(&self) -> Result<Option<PathBuf>, Error> {
        let mut guard = self.write()?;
        let Some(tracker) = guard.take() else {
            return Ok(None);
        };

        let closing_event = match tracker.shared_env().destroy() {
            Ok(closing) => closing,
            Err(e) => {
                *guard = Some(tracker);
                return Err(e);
            }
        };

        let db_path = tracker.env().path().to_path_buf();
        // Drop closes the LMDB env and unmaps the files
        drop(tracker);
        drop(guard);

        // Deleting before mdb_env_close finishes would race the unmap.
        if let Some(event) = closing_event {
            event.wait_timeout(Duration::from_secs(5));
        }

        std::fs::remove_dir_all(&db_path).map_err(|source| Error::RemoveDbDir {
            path: db_path.clone(),
            source,
        })?;
        Ok(Some(db_path))
    }
}

/// Thread-safe shared handle to the [`FrecencyTracker`] instance.
pub type SharedFrecency = SharedDb<FrecencyTracker>;

/// Thread-safe shared handle to the [`QueryTracker`] instance.
pub type SharedQueryTracker = SharedDb<QueryTracker>;
