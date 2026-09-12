use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use tracing::{error, info};

use crate::FileSync;
use crate::error::Error;
use crate::file_picker::FFFMode;
use crate::index::{build_bigram_index, sniff_binary_for_non_indexable};
use crate::parallelism::BACKGROUND_THREAD_POOL;
use crate::shared::{SharedFilePicker, SharedFrecency};
use crate::types::ContentCacheBudget;
use crate::watch::BackgroundWatcher;

#[derive(Clone, Default)]
pub(crate) struct ScanSignals {
    /// Set to `true` while any scan phase is running
    pub(crate) scanning: Arc<AtomicBool>,
    /// Set to `true` once the filesystem watcher has been installed
    pub(crate) watcher_ready: Arc<AtomicBool>,
    /// Indicates that that owning picker was requested to shut down
    pub(crate) cancelled: Arc<AtomicBool>,
    /// Used to resolve conflicts if multiple rescans were triggered in a queue
    pub(crate) rescan_pending: Arc<AtomicBool>,
    /// Set by `post_scan_snapshot`, cleared by `PostScanSnapshot::drop`.
    /// DO NOT set or clear this manually — it is managed exclusively by the
    /// PostScanSnapshot lifecycle.
    pub(crate) post_scan_indexing_active: Arc<AtomicBool>,
}

/// Which optional phases a scan should run.
#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct ScanConfig {
    pub(crate) warmup: bool,
    pub(crate) content_indexing: bool,
    pub(crate) watch: bool,
    pub(crate) auto_cache_budget: bool,
    pub(crate) install_watcher: bool,
    pub(crate) follow_symlinks: bool,
    pub(crate) enable_fs_root_scanning: bool,
    pub(crate) enable_home_dir_scanning: bool,
}

/// A fully-configured scan job ready to run on a background thread.
///
/// Build with [`ScanJob::from_picker`] (reads all state from the
/// current `FilePicker`) or [`ScanJob::initial`] (for the bootstrap
/// scan, before the picker is published to `SharedPicker`).
pub(crate) struct ScanJob {
    shared_picker: SharedFilePicker,
    shared_frecency: SharedFrecency,
    base_path: PathBuf,
    mode: FFFMode,
    signals: ScanSignals,
    config: ScanConfig,
    /// Walker-maintained counter backing `get_scan_progress` on the UI
    /// side. Reset to 0 at scan start, incremented per-file by the
    /// walker. Shared `Arc` so the UI polls the same atomic.
    scanned_files_counter: Arc<AtomicUsize>,
    trace_span: tracing::Span,
}

impl ScanJob {
    pub fn new_rescan(
        shared_picker: &SharedFilePicker,
        shared_frecency: &SharedFrecency,
    ) -> Result<Option<Self>, Error> {
        let guard = shared_picker.read()?;
        let picker = guard.as_ref().ok_or(Error::FilePickerMissing)?;

        if picker.is_scan_active()
            || picker
                .signals
                .post_scan_indexing_active
                .load(Ordering::Acquire)
        {
            return Ok(None);
        }

        let mode = picker.mode();
        let signals = picker.scan_signals();
        let scanned_files_counter = picker.scanned_files_counter();
        let base_path = picker.base_path().to_path_buf();
        let trace_span = picker.trace_span();

        let new_scan_config = ScanConfig {
            warmup: picker.has_mmap_cache(),
            content_indexing: picker.has_content_indexing(),
            watch: picker.has_watcher(),
            auto_cache_budget: !picker.has_explicit_cache_budget(),
            install_watcher: false, // the watcher is independent of rescan, it is not restarting EVER
            follow_symlinks: picker.follows_symlinks(),
            enable_fs_root_scanning: picker.fs_root_scanning_enabled(),
            enable_home_dir_scanning: picker.home_dir_scanning_enabled(),
        };

        drop(guard); // just a sanity check

        Ok(Some(Self {
            mode,
            signals,
            base_path,
            scanned_files_counter,
            config: new_scan_config,
            shared_picker: shared_picker.clone(),
            shared_frecency: shared_frecency.clone(),
            trace_span,
        }))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_initial(
        shared_picker: SharedFilePicker,
        shared_frecency: SharedFrecency,
        base_path: PathBuf,
        mode: FFFMode,
        signals: ScanSignals,
        scanned_files_counter: Arc<AtomicUsize>,
        trace_span: tracing::Span,
        config: ScanConfig,
    ) -> Self {
        Self {
            shared_picker,
            shared_frecency,
            base_path,
            mode,
            signals,
            scanned_files_counter,
            config,
            trace_span,
        }
    }

    /// Run the job on `BACKGROUND_THREAD_POOL`. Returns immediately.
    ///
    /// Routed through the pool — and not a fresh `std::thread::spawn` — so the
    /// orchestrator inherits rayon's QoS pin (USER_INITIATED). Without that
    /// pin, an interactive nvim's USER_INTERACTIVE main thread spawns a child
    /// at lower QoS, the walker's Zig worker pool inherits the demotion, and
    /// the kernel drifts those workers onto E-cores. On chromium that turns a
    /// ~800 ms walk into ~3 s.
    pub fn spawn(self) {
        self.signals.scanning.store(true, Ordering::Release);
        let span = self.trace_span.clone();
        BACKGROUND_THREAD_POOL.spawn(move || {
            let _g = span.enter();
            self.run();
        });
    }

    fn run(self) {
        let Self {
            shared_picker,
            shared_frecency,
            base_path,
            mode,
            signals,
            scanned_files_counter,
            config,
            trace_span: _,
        } = self;

        let _scanning = ScanningGuard::new(&signals);
        scanned_files_counter.store(0, Ordering::Relaxed);

        // 1. Walk the file system and collect the list of files
        let git_workdir = FileSync::discover_git_workdir(&base_path);
        let sync = match FileSync::walk_filesystem(
            &base_path,
            git_workdir.clone(),
            &scanned_files_counter,
            &shared_frecency,
            mode,
            config.follow_symlinks,
        ) {
            Ok(sync) => sync,
            Err(e) => {
                error!(?e, "scan walk failed");
                return;
            }
        };

        // 2. Populate the file list
        let git_status_worker;
        if let Ok(mut guard) = shared_picker.write()
            && let Some(picker) = guard.as_mut()
        {
            if signals.cancelled.load(Ordering::Acquire) {
                info!("scan cancelled between walk and commit, discarding");
                return;
            }

            let live_count = sync.live_count;
            picker.commit_new_sync(sync);
            git_status_worker = Arc::clone(&picker.git_status_worker);

            if config.auto_cache_budget && !picker.has_explicit_cache_budget() {
                picker.set_cache_budget(ContentCacheBudget::new_for_repo(live_count));
            }
        } else {
            error!("failed to install scan results into picker");
            return;
        }

        // Spawn the git status worker once. BUG PINNNING. If the user initiated git in the folder
        // which is a real use case we need to have a way to start the git worker background thread dynamically
        if git_workdir.is_some() && !signals.cancelled.load(Ordering::Acquire) {
            git_status_worker.spawn_once(shared_picker.weaken(), shared_frecency.clone());
            git_status_worker.request_full_rescan(); // this runs anyway
        }

        // BUG pinning: take the snapshot *before* the storing the scan=true, otherwise there is a tiny
        // race window when there scanned is set to true, but `post_scan_indexing_active` flag is `false`
        let snapshot = if !signals.cancelled.load(Ordering::Acquire) {
            shared_picker.read().ok().and_then(|guard| {
                guard
                    .as_ref()
                    .and_then(|picker| unsafe { picker.post_scan_snapshot() })
            })
        } else {
            None
        };

        signals.scanning.store(false, Ordering::Relaxed); // file are searchable

        // in case we do a rescan, we have to resubscribe a watcher to the new set of directories
        // all the already watched directories are not going to be resubscribed (this is internally deduped)
        if !config.install_watcher && !signals.cancelled.load(Ordering::Acquire) {
            rescubscribe_watcher_post_scan(&shared_picker);
        }

        // 3. Runs post scna in parallel with git status collection
        if !signals.cancelled.load(Ordering::Acquire)
            && let Some(snap) = snapshot.as_ref()
        {
            Self::run_post_scan(&shared_picker, &signals, &config, snap);
        }

        drop(snapshot); // SNAPSHOT SHOULD NOT BE USED AFTER THIS POINT

        // 5. Install filesystem watcher (initial scan only).
        if config.install_watcher && config.watch && !signals.cancelled.load(Ordering::Acquire) {
            let shared_picker: &SharedFilePicker = &shared_picker;
            let shared_frecency: &SharedFrecency = &shared_frecency;
            let base_path: &std::path::Path = &base_path;

            match BackgroundWatcher::new(
                base_path.to_path_buf(),
                git_workdir,
                shared_picker.clone(),
                shared_frecency.clone(),
                mode,
                config.enable_fs_root_scanning,
                config.enable_home_dir_scanning,
                git_status_worker,
                tracing::Span::current(),
            ) {
                Ok(watcher) => {
                    if let Ok(mut guard) = shared_picker.write()
                        && let Some(picker) = guard.as_mut()
                        && picker.base_path() == base_path
                        && !signals.cancelled.load(Ordering::Acquire)
                    {
                        picker.background_watcher = Some(watcher);
                        signals.watcher_ready.store(true, Ordering::Release);
                    }
                }
                Err(e) => error!(?e, "failed to initialize background watcher"),
            };
        }

        // 6. Drain any rescan that arrived while we were busy.
        // if user initiated a new rescan we had no way to cancel current post scan, so do it again
        if !signals.cancelled.load(Ordering::Acquire)
            && signals.rescan_pending.swap(false, Ordering::AcqRel)
        {
            match Self::new_rescan(&shared_picker, &shared_frecency) {
                Ok(Some(follow_up)) => {
                    info!("Rescheduling deferred rescan after current scan finished");
                    follow_up.spawn();
                }
                Ok(None) => {
                    // this should be practically impossible because we do not have any
                    // queue, but if somehow a new rescan was triggered JUST IN THIS MOMENT
                    // just ignore it because the ongoing one is fresh enough
                    tracing::warn!("Post scan was re-triggered, ignoring");
                }
                Err(e) => {
                    error!(?e, "Failed to reschedule deferred rescan");
                }
            }
        }
    }

    /// THIS IS VERY VERY IMPORTANT THAT ANYTHING INSIDE THIS FUNCTION TO NOT READ ANYTHING CLEARABLE OUTSIDE
    /// this is a very silly off lock implementation that actually matters, and that's why it is crafted
    /// to never read anything from the picker, it can only WRITE information using single instructions
    ///
    /// Things that are safe and immutable - file list, indexes of files, paths, and signals.
    #[tracing::instrument(skip_all, fields(warmup = ?config.warmup, indexing = ?config.content_indexing))]
    fn run_post_scan(
        shared_picker: &SharedFilePicker,
        signals: &ScanSignals,
        config: &ScanConfig,
        unsafe_snapshot: &crate::file_picker::PostScanUnsafeSnapshot,
    ) {
        let Some(arena) = unsafe_snapshot
            .arena // we are never touching overlays so this arena is always correct
            .as_ref()
            .map(|s| s.as_arena_ptr())
        else {
            tracing::error!("Failed to run post scan: arena is invalid");
            return;
        };

        let files: &[crate::types::FileItem] = &unsafe_snapshot.files[..unsafe_snapshot.base_count];
        if signals.cancelled.load(Ordering::Acquire) {
            return;
        }

        if config.content_indexing {
            let indexable_count = unsafe_snapshot.indexable_count.min(files.len());
            let (indexable_files, non_indexable_files) = files.split_at(indexable_count);
            let index = build_bigram_index(indexable_files, &unsafe_snapshot.base_path, arena);

            if let Ok(mut guard) = shared_picker.write()
                && let Some(picker) = guard.as_mut()
            {
                picker.set_bigram_index(index);
            }
            crate::index::release_thread_buffers();

            // Bigram only sniffs files <= MAX_INDEXABLE_FILE_SIZE; large
            // unknown-extension binaries slip past it and would otherwise be
            // grep-able as text. Cheap header sniff catches those.
            if !signals.cancelled.load(Ordering::Acquire) {
                sniff_binary_for_non_indexable(
                    non_indexable_files,
                    &unsafe_snapshot.base_path,
                    arena,
                    &signals.cancelled,
                );
            }
        } else {
            // this potentially a long running as we are not parallelizing it but it's okay
            sniff_binary_for_non_indexable(
                files,
                &unsafe_snapshot.base_path,
                arena,
                &signals.cancelled,
            );
        }

        // TODO Skipped as potentially unsafe - figure this out later
        // if config.warmup && !signals.cancelled.load(Ordering::Acquire) {
        //     warmup_mmaps(files, budget, &unsafe_snapshot.base_path, arena);
        // }
    }
}

// Ensures early returns clear the scanning signal.
struct ScanningGuard<'a> {
    signals: &'a ScanSignals,
}

impl<'a> ScanningGuard<'a> {
    fn new(signals: &'a ScanSignals) -> Self {
        signals.scanning.store(true, Ordering::Relaxed);
        Self { signals }
    }
}

impl Drop for ScanningGuard<'_> {
    fn drop(&mut self) {
        self.signals.scanning.store(false, Ordering::Relaxed);
    }
}

/// If the scan encounters new directories created we have to add them to the watch list
/// this is fine because the watcher does deduplicate the entries and doesn't add a lot of
/// garbage notify watchers / fs events streams
#[tracing::instrument(skip_all)]
fn rescubscribe_watcher_post_scan(shared_picker: &SharedFilePicker) {
    let Ok(guard) = shared_picker.read() else {
        return;
    };
    let Some(picker) = guard.as_ref() else {
        return;
    };
    let Some(watcher) = picker.background_watcher.as_ref() else {
        return;
    };

    picker.for_each_dir(|dir: &std::path::Path| {
        watcher.request_watch_dir(dir.to_path_buf());
        std::ops::ControlFlow::Continue(())
    });
}
