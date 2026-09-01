use crate::constants::MAX_OVERFLOW_FILES;
use crate::error::Error;
use crate::file_picker::FFFMode;
use crate::git_status_worker::GitStatusWorker;
use crate::rescan_stats::RescanReason;
use crate::shared::{SharedFilePicker, SharedFrecency};
use crate::sort_buffer::sort_with_buffer;
use crate::watch::{RawWatchEvent, WatchEventKind};
use git2::Repository;
use notify::event::{AccessKind, AccessMode, ModifyKind, RenameMode};
use notify::{Config, EventKind, EventKindMask, RecursiveMode};
use notify_debouncer_full::{DebounceEventResult, DebouncedEvent, NoCache, new_debouncer_opt};
use parking_lot::Mutex;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;
use tracing::{Level, debug, error, info, warn};

type Debouncer = notify_debouncer_full::Debouncer<notify::RecommendedWatcher, NoCache>;

/// Owns the file-system watcher and guarantees that all background threads
/// are fully joined before `stop()` / `Drop` returns.
pub struct BackgroundWatcher {
    debouncer: Arc<Mutex<Option<Debouncer>>>,
    watch_tx: Option<mpsc::Sender<WatchTask>>,
    owner_thread: Option<std::thread::JoinHandle<()>>,
}

enum WatchTask {
    /// Only subscribe to a specific path, this is happening when we did rescun and have to update
    /// the watcher only
    Subscribe(PathBuf),
    /// This is requires a separate walk of the new directory copies or created within a scan
    /// window because it might contain subdirectories we have to walk, prune, and add to index
    IndexNewDir(PathBuf),
}

const DEBOUNCE_TIMEOUT: Duration = Duration::from_millis(50);
/// Minimum seconds between frecency tracks of the same file in AI mode.
/// Prevents score inflation from rapid burst edits by AI agents.
const AI_MODE_COOLDOWN_SECS: u64 = 5 * 60;

impl BackgroundWatcher {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        base_path: PathBuf,
        git_workdir: Option<PathBuf>,
        shared_picker: SharedFilePicker,
        shared_frecency: SharedFrecency,
        mode: FFFMode,
        enable_fs_root_scanning: bool,
        enable_home_dir_scanning: bool,
        git_status_worker: Arc<GitStatusWorker>,
        trace_span: tracing::Span,
    ) -> Result<Self, Error> {
        info!(
            "Initializing background watcher for path: {}, mode: {:?}",
            base_path.display(),
            mode,
        );

        // by default we do not want to allow users to search their FS root, this is very error prone
        // though some consumers would specifically allow that e.g. unikernels, windows disc
        // partition or sub file systems. By default - fail, unless user permits
        let is_fs_root = base_path.parent().is_none();
        // use rust's path api for maximum reliability of the comparison
        let is_home_dir = Some(&base_path) == dirs::home_dir().as_ref();

        if (is_fs_root && !enable_fs_root_scanning) || (is_home_dir && !enable_home_dir_scanning) {
            return Err(Error::FilesystemRoot(base_path));
        }

        // macOS: always use a single recursive FSEvent stream.
        // Per-dir NonRecursive watches create one FSEvent stream per dir.
        // The per-process FSEvent cap is lower than expected in practice
        // (4096 per process, but FFF usually is running within code editors),
        // and each failed `watch()` after the cap blocks ~40 ms on kernel retry.
        // Yes we pay for filtering events on handler phase but it is usable
        //
        // Windows doesn't seem to have a hard cap, but in practice non recursive watching
        // does a way worse job and often looses events which is not an option for us.
        //
        // Linux keeps the per-dir NonRecursive strategy: inotify has no
        // kernel-level watcher recursion, so we have to manually watch every single interested
        // directory for watch events which is in practice stable and fast if system has enough
        // spare watcher (configurable by the user, usually 100k - 1m)
        let use_recursive = cfg!(any(target_os = "macos", target_os = "windows"));

        let (watch_tx, watch_rx) = mpsc::channel::<WatchTask>();
        let watch_tx_for_debouncer = watch_tx.clone();

        let owner_weak_picker = shared_picker.weaken();
        let owner_git_workdir = git_workdir.clone();
        let owner_git_worker = Arc::clone(&git_status_worker);

        let debouncer = Self::create_debouncer(
            base_path,
            git_workdir,
            shared_picker,
            shared_frecency,
            mode,
            use_recursive,
            watch_tx_for_debouncer,
            git_status_worker,
        )?;

        info!("Background file watcher initialized successfully");

        let debouncer = Arc::new(Mutex::new(Some(debouncer)));
        // Only the Linux per-dir-watch branch needs this clone; on other
        // platforms the owner thread never touches the debouncer.
        #[cfg(target_os = "linux")]
        let owner_debouncer = Arc::clone(&debouncer);

        let owner_span = trace_span.clone();
        let owner_thread = std::thread::Builder::new()
            .name("fff-watcher-own".into())
            .spawn(move || {
                let _g = owner_span.enter();
                while let Ok(task) = watch_rx.recv() {
                    // if the picker is dropped we do need to exit the loop
                    let Some(strong_picker) = owner_weak_picker.upgrade() else {
                        break;
                    };

                    let (dir, is_new_dir) = match task {
                        WatchTask::Subscribe(dir) => (dir, false),
                        WatchTask::IndexNewDir(dir) => (dir, true),
                    };

                    // Register the watch BEFORE walking so files created mid-walk still handled
                    #[cfg(target_os = "linux")]
                    if !watch_dirs_nonrecursive(&owner_debouncer, std::iter::once(dir.as_path())) {
                        break;
                    }

                    if is_new_dir {
                        // need to call this on every platform to add subdirectories from the
                        // new folders to the picker, but on linux we have to handle the subdirs
                        let subdirs = index_new_directory(
                            &dir,
                            &strong_picker,
                            &owner_git_workdir,
                            &owner_git_worker,
                        );

                        // on linux we manually resubscribe for new inodes
                        #[cfg(target_os = "linux")]
                        if !watch_dirs_nonrecursive(
                            &owner_debouncer,
                            subdirs.iter().map(|p| p.as_path()),
                        ) {
                            break;
                        }

                        drop(subdirs); // need it cause subdirs is unused on non-linux target'
                    }

                    drop(strong_picker);
                }

                tracing::info!("Background watcher is stopped");
            })
            .expect("failed to spawn fff-watcher-owner thread");

        Ok(Self {
            debouncer,
            watch_tx: Some(watch_tx),
            owner_thread: Some(owner_thread),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn create_debouncer(
        base_path: PathBuf,
        git_workdir: Option<PathBuf>,
        shared_picker: SharedFilePicker,
        shared_frecency: SharedFrecency,
        mode: FFFMode,
        use_recursive: bool,
        watch_tx: mpsc::Sender<WatchTask>,
        git_status_worker: Arc<GitStatusWorker>,
    ) -> Result<Debouncer, Error> {
        let config = Config::default()
            .with_follow_symlinks(false)
            // only the actual modification events, ignore the open syscals that we can generate by
            // our own grep calls and preview window rendering
            .with_event_kinds(EventKindMask::CORE);

        let git_workdir_for_handler = git_workdir.clone();
        let base_path_for_handler = base_path.clone();
        let shared_picker_for_watching = shared_picker.clone();
        let file_picker = shared_picker.weaken();
        let mut debouncer = new_debouncer_opt(
            DEBOUNCE_TIMEOUT,
            Some(DEBOUNCE_TIMEOUT / 2), // tick rate for the event span
            {
                move |result: DebounceEventResult| match result {
                    Ok(events) => {
                        let Some(file_picker) = file_picker.upgrade() else {
                            return;
                        };

                        let new_dirs = handle_debounced_events(
                            mode,
                            events,
                            &base_path_for_handler,
                            &git_workdir_for_handler,
                            &file_picker,
                            &shared_frecency,
                            &git_status_worker,
                        );

                        // every new directory created has to be reflected in the picker state
                        for dir in new_dirs {
                            if let Err(e) = watch_tx.send(WatchTask::IndexNewDir(dir)) {
                                error!(?e, "Failed to send directory update error");
                            }
                        }
                    }
                    Err(errors) => {
                        error!("File watcher errors: {:?}", errors);
                    }
                }
            },
            // There is an issue with recommended cache implementation on macos
            // it keeps track of all the files added to the watcher which is not a problem
            // for us because any rename to the file will anyway require the removing from the
            // ordedred index and adding it back with the new name
            NoCache::new(),
            config,
        )?;

        if use_recursive {
            debouncer.watch(base_path.as_path(), RecursiveMode::Recursive)?;
            info!(
                "File watcher initialized with single recursive watch on {}",
                base_path.display(),
            );
        } else {
            debouncer.watch(base_path.as_path(), RecursiveMode::NonRecursive)?;

            const MAX_CONSECUTIVE_WATCH_FAILURES: usize = 16;

            let mut watched = 0usize;
            let mut consecutive_failures = 0usize;

            // `inotify` is fast-fail: on ENOSPC it returns
            // immediately, no kernel retry loop, so holding this lock is free
            if let Some(guard) = shared_picker_for_watching.read().ok()
                && let Some(picker) = guard.as_ref()
            {
                use std::ops::ControlFlow;
                picker.for_each_dir(|dir| {
                    match debouncer.watch(dir, RecursiveMode::NonRecursive) {
                        Ok(()) => {
                            watched += 1;
                            consecutive_failures = 0;
                            ControlFlow::Continue(())
                        }
                        Err(e) => {
                            consecutive_failures += 1;
                            if consecutive_failures <= 4 {
                                warn!("Failed to watch directory {}: {}", dir.display(), e);
                            }

                            if consecutive_failures >= MAX_CONSECUTIVE_WATCH_FAILURES {
                                warn!(
                                    consecutive_failures,
                                    watched,
                                    "Giving up setting file watcher for all the directories. Check if your system has enough fs watchers limit."
                                );

                                ControlFlow::Break(())
                            } else {
                                ControlFlow::Continue(())
                            }
                        }
                    }
                });
            }

            tracing::info!(
                ?watched,
                path = ?base_path.display(),
                "File watcher initialized"
            );
        }

        // The .git directory is excluded from the file list but we still need
        // to observe changes that affect git status (staging, unstaging,
        // committing, branch switches, merges, etc)
        watch_git_status_paths(&mut debouncer, git_workdir.as_ref());

        Ok(debouncer)
    }

    /// Signal the watcher to shut down without blocking on its worker
    /// threads. Safe to call from any context, including while holding
    /// the [`SharedFilePicker`] write lock.
    pub fn stop(&mut self) {
        self.watch_tx.take();
        if let Some(debouncer) = self.debouncer.lock().take() {
            debouncer.stop_nonblocking();
        }

        self.owner_thread.take();

        info!("Background file watcher stop signaled");
    }

    pub(crate) fn request_watch_dir(&self, dir: PathBuf) -> bool {
        match self.watch_tx.as_ref() {
            Some(tx) => tx.send(WatchTask::Subscribe(dir)).is_ok(),
            None => false,
        }
    }
}

impl Drop for BackgroundWatcher {
    fn drop(&mut self) {
        self.stop();
    }
}

#[tracing::instrument(name = "fs_events", skip(events, shared_picker, shared_frecency, git_status_worker), level = Level::DEBUG)]
pub(crate) fn handle_debounced_events(
    mode: FFFMode,
    events: Vec<DebouncedEvent>,
    base_path: &Path,
    git_workdir: &Option<PathBuf>,
    shared_picker: &SharedFilePicker,
    shared_frecency: &SharedFrecency,
    git_status_worker: &Arc<GitStatusWorker>,
) -> Vec<PathBuf> {
    // this will be called very often, we have to minimiy the lock time for file picker
    let repo = git_workdir.as_ref().and_then(|p| Repository::open(p).ok());
    // Prefer the walker's own ignore rules (zlob); grab a cheap Arc clone once
    // per batch so we don't hold the picker lock during filtering.
    let walker_rules = shared_picker
        .read()
        .ok()
        .and_then(|g| g.as_ref().and_then(|p| p.ignore_rules()));
    let filter = IgnoreFilter::new(base_path, walker_rules, repo.as_ref());
    let mut need_full_git_rescan = false;
    let mut batch_overflow_attempted = false;
    let mut paths_to_remove = Vec::new();
    let mut dirs_to_remove: Vec<PathBuf> = Vec::new();
    let mut paths_to_add_or_modify = Vec::new();
    let mut new_dirs_to_watch = Vec::new();
    let mut affected_paths_count = 0usize;
    let mut explicit_renames: Vec<(PathBuf, PathBuf)> = Vec::new();

    let watch_registry = shared_picker.watch_registry();
    let need_events_propagation = watch_registry.is_active();

    let try_trigger_full_rescan = |reason: RescanReason| -> bool {
        match shared_picker.trigger_full_rescan_with_reason(shared_frecency, reason) {
            Ok(true) => {
                warn!(%reason, "Triggering full rescan");
                watch_registry.dispatch_rescan(base_path);
                true
            }
            Ok(false) => false,
            Err(e) => {
                error!(%reason, "Failed to trigger full rescan: {:?}", e);
                false
            }
        }
    };

    for debounced_event in &events {
        // It is very important to not react to the access errors because we inevitably
        // gonna trigger the sync by our own preview or other unnecessary noise
        if matches!(
            debounced_event.event.kind,
            EventKind::Access(
                AccessKind::Read
                    | AccessKind::Open(_)
                    | AccessKind::Close(AccessMode::Read | AccessMode::Execute)
            )
        ) {
            continue;
        }

        // When macOS FSEvents (or other backends) overflow their event buffer, the kernel
        // drops individual events and emits a rescan flag telling us to re-scan the subtree
        if debounced_event.event.need_rescan() {
            let small_and_known = debounced_event.event.paths.len() < 16 // this should be usually one event
                && debounced_event
                    .paths
                    .iter()
                    // but we are smart enough and not falling into the paths
                    .all(|p| !p.is_dir() && !filter.is_ignored(p));

            if !small_and_known && try_trigger_full_rescan(RescanReason::KernelEventLoss) {
                return Vec::new();
            }

            // Small batches and throttled rescans fall through: the listed
            // paths are still applied incrementally below.
        }

        tracing::debug!(event = ?debounced_event.event, "Processing FS event");

        // Backends that pair the halves for us hand over [from, to] directly.
        // The per-path pass below still classifies both, we only note the link.
        if let EventKind::Modify(ModifyKind::Name(RenameMode::Both)) = debounced_event.event.kind
            && let [from, to] = debounced_event.event.paths.as_slice()
        {
            explicit_renames.push((from.clone(), to.clone()));
        }

        for path in &debounced_event.event.paths {
            if matches!(
                path.file_name().and_then(|f| f.to_str()),
                Some(".ignore") | Some(".gitignore")
            ) {
                if path
                    .parent()
                    .is_some_and(|parent| filter.is_ignored(parent))
                {
                    continue;
                }

                info!(
                    "Detected change in ignore definition file: {}",
                    path.display()
                );

                if try_trigger_full_rescan(RescanReason::IgnoreFileChanged) {
                    return Vec::new();
                }

                // Throttled: fall through so the ignore file itself stays
                // indexed; the stale rules heal on the next admitted rescan.
            }

            if is_dotgit_change_affecting_status(path, &repo) {
                need_full_git_rescan = true;
            }

            if is_git_file(path) {
                continue;
            }

            // Use a combination of event kind and filesystem state to decide
            // whether a path is an addition/modification or a removal.
            //
            // We cannot rely on `path.exists()` alone because:
            //   - A freshly created file might not be visible yet (race).
            //   - macOS FSEvents uses Modify(Name(Any)) for both rename-in
            //     and rename-out, so we must stat the path to disambiguate.
            //
            // We cannot rely on event kind alone because:
            //   - Remove events are not always emitted (macOS often sends
            //     Modify(Name(Any)) instead of Remove).
            let is_removal = matches!(debounced_event.event.kind, EventKind::Remove(_));

            // Directory-level remove: both fsevents and inotify delivers a single
            // `Remove(Folder)` event for a whole directory tree (e.g.
            // after `git reset --hard` wipes a dir full of staged-but-
            // uncommitted files).
            let is_folder_removal = matches!(
                debounced_event.event.kind,
                EventKind::Remove(notify::event::RemoveKind::Folder)
            );

            let is_removed = is_folder_removal || is_removal || !path.exists();

            let (is_dir, is_ignored) = if is_removed {
                (false, true)
            } else {
                (path.is_dir(), filter.is_ignored(path))
            };

            if is_folder_removal {
                dirs_to_remove.push(path.to_path_buf());
            } else if is_removed {
                // best effort but doesn't require a stat and generally correct
                let maybe_directory = !matches!(
                    debounced_event.event.kind,
                    EventKind::Remove(notify::event::RemoveKind::File)
                );

                paths_to_remove.push((path.as_path(), maybe_directory));
            } else if is_dir {
                if !is_ignored {
                    new_dirs_to_watch.push(path.to_path_buf());
                }
            } else if !is_ignored {
                // For additions/modifications, still filter gitignored files.
                paths_to_add_or_modify.push(path.as_path());
            }
        }

        affected_paths_count += debounced_event.event.paths.len();
        if !batch_overflow_attempted && affected_paths_count > MAX_OVERFLOW_FILES * 4 {
            batch_overflow_attempted = true;
            warn!(
                ?affected_paths_count,
                max = MAX_OVERFLOW_FILES * 4,
                "Too many affected paths in a single batch, triggering full rescan",
            );

            if try_trigger_full_rescan(RescanReason::EventBatchOverflow) {
                return Vec::new();
            }
        }
    }

    // It's important to get the allocated sort
    sort_with_buffer(paths_to_add_or_modify.as_mut_slice(), |a, b| {
        a.as_os_str().cmp(b.as_os_str())
    });
    paths_to_add_or_modify.dedup_by(|a, b| a.as_os_str().eq(b.as_os_str()));

    info!(
        "Event processing summary: {} to remove, {} dirs to remove, {} to add/modify, {} new dirs",
        paths_to_remove.len(),
        dirs_to_remove.len(),
        paths_to_add_or_modify.len(),
        new_dirs_to_watch.len()
    );

    if paths_to_remove.is_empty()
        && dirs_to_remove.is_empty()
        && paths_to_add_or_modify.is_empty()
        && !need_full_git_rescan
    {
        debug!("No file index changes to apply");
        return new_dirs_to_watch;
    }

    let mut files_to_update_git_status = Vec::new();
    let mut index_update_rejected = false;
    let mut overflow_count = 0;
    let mut removed_from_dirs = Vec::new();
    // Destination path -> (kind, rename source). One entry per path, so a
    // remove+add pair recognised as a rename collapses into a single event.
    let mut watch_events: ahash::AHashMap<PathBuf, (WatchEventKind, Option<PathBuf>)> =
        ahash::AHashMap::new();
    // Destination -> source for every rename recognised in this batch.
    let mut renames: ahash::AHashMap<PathBuf, PathBuf> = ahash::AHashMap::new();

    if !paths_to_remove.is_empty()
        || !dirs_to_remove.is_empty()
        || !paths_to_add_or_modify.is_empty()
    {
        debug!(
            "Applying file index changes: {} to remove, {} dirs to remove, {} to add/modify",
            paths_to_remove.len(),
            dirs_to_remove.len(),
            paths_to_add_or_modify.len(),
        );

        let Ok(mut guard) = shared_picker.write() else {
            error!("Failed to acquire file picker write lock");
            return new_dirs_to_watch;
        };
        let Some(ref mut picker) = *guard else {
            error!("File picker not initialized");
            return new_dirs_to_watch;
        };

        // Must run before the removals below, while the vanished files still
        // carry their indexed size/mtime.
        renames.extend(detect_renames(
            picker,
            base_path,
            &paths_to_remove,
            &paths_to_add_or_modify,
            &explicit_renames,
        ));

        for (path, may_be_dir) in &paths_to_remove {
            let removed = picker.remove_file_by_path(path);

            if removed {
                if need_events_propagation {
                    watch_events.insert(path.to_path_buf(), (WatchEventKind::Removed, None));
                }
            } else if *may_be_dir {
                // Not an indexed file: likely a dir renamed out of the tree
                // (no Remove(Folder) is emitted), expand it per indexed file.
                dirs_to_remove.push(path.to_path_buf());
            }
        }

        // Single index scan for all dirs; misses (never-indexed paths) are free.
        dirs_to_remove.sort_unstable();
        dirs_to_remove.dedup();
        if !dirs_to_remove.is_empty() {
            let dirs = dirs_to_remove.iter().map(PathBuf::as_path);
            if need_events_propagation {
                picker.remove_all_files_in_dirs_with_callback(dirs, |path| {
                    removed_from_dirs.push(path.to_path_buf());
                })
            } else {
                picker.remove_all_files_in_dirs(dirs)
            };
        }

        if need_events_propagation {
            for path in removed_from_dirs.drain(..) {
                watch_events.insert(path, (WatchEventKind::Removed, None));
            }
        }

        files_to_update_git_status.reserve(paths_to_add_or_modify.len());
        for path in &paths_to_add_or_modify {
            if picker.get_overflow_files().len() >= MAX_OVERFLOW_FILES
                && picker.get_file_by_path(path).is_none()
            {
                index_update_rejected = true;
                break;
            }

            let existed = need_events_propagation && picker.get_file_by_path(path).is_some();

            if picker.handle_create_or_modify(path).is_some() {
                files_to_update_git_status.push(path.to_path_buf());
                if need_events_propagation {
                    let event = match renames.get(*path) {
                        Some(from) => (WatchEventKind::Renamed, Some(from.clone())),
                        None if existed => (WatchEventKind::Modified, None),
                        None => (WatchEventKind::Created, None),
                    };

                    watch_events.insert(path.to_path_buf(), event);
                }
            } else {
                index_update_rejected = true;
            }
        }

        // A recognised move is one change, not a removal plus a creation. Drop
        // the source's own event only where the destination really reported the
        // pair — a destination that never made it into the index still needs
        // its source announced as removed.
        if need_events_propagation {
            for (to, from) in &renames {
                if matches!(watch_events.get(to), Some((WatchEventKind::Renamed, _))) {
                    watch_events.remove(from);
                }
            }
        }

        overflow_count = picker.get_overflow_files().len();
    }

    info!(
        files_updated = files_to_update_git_status.len(),
        overflow_count, "File index changes applied",
    );

    let rescan_started = if index_update_rejected || overflow_count > MAX_OVERFLOW_FILES {
        let reason = if index_update_rejected {
            RescanReason::IndexUpdateRejected
        } else {
            RescanReason::OverflowCapacity
        };

        info!(%reason, "Watcher faced limit of index overflow. Triggering rescan");
        try_trigger_full_rescan(reason)
    } else {
        false
    };

    // When the rescan is throttled the incrementally applied changes are
    // still the freshest state we have — propagate them to subscribers.
    if !rescan_started && need_events_propagation {
        watch_registry.dispatch(
            base_path,
            watch_events
                .into_iter()
                .map(|(path, (kind, from))| RawWatchEvent {
                    path,
                    kind,
                    is_ignored: false,
                    from,
                })
                .collect(),
        );
    }

    // Carry the access history onto the new path. Copied, not moved: checking
    // out a revision where the old path exists again must still rank it.
    if !renames.is_empty() {
        let mut carried = Vec::new();
        if let Ok(frecency_guard) = shared_frecency.read()
            && let Some(ref frecency) = *frecency_guard
        {
            let pairs: Vec<_> = renames
                .iter()
                .map(|(to, from)| (from.clone(), to.clone()))
                .collect();
            match frecency.copy_history_many(&pairs) {
                Ok(paths) => carried = paths,
                Err(e) => error!("Failed to carry frecency over renames: {:?}", e),
            }
        }

        if !carried.is_empty() {
            info!(count = carried.len(), "Carried frecency across renames");
            if let Ok(mut picker_guard) = shared_picker.write()
                && let Some(ref mut picker) = *picker_guard
                && let Ok(frecency_guard) = shared_frecency.read()
                && let Some(ref frecency) = *frecency_guard
            {
                for path in &carried {
                    let _ = picker.update_single_file_frecency(path, frecency);
                }
            }
        }
    }

    // AI mode: auto-track frecency for all modified/created files.
    // Uses a 5-minute cooldown per file to prevent score inflation from rapid
    // burst edits (AI agents often edit the same file many times in minutes).
    // This runs after apply_changes so the picker write lock is released.
    if mode.is_ai() && !paths_to_add_or_modify.is_empty() {
        let mut tracked_count = 0usize;
        if let Ok(frecency_guard) = shared_frecency.read()
            && let Some(ref frecency) = *frecency_guard
        {
            for path in &paths_to_add_or_modify {
                // Skip if this file was tracked less than 5 minutes ago
                let should_track = match frecency.seconds_since_last_access(path) {
                    Ok(Some(secs)) => secs >= AI_MODE_COOLDOWN_SECS,
                    Ok(None) => true, // Never tracked before
                    Err(_) => true,   // DB error, track anyway
                };
                if !should_track {
                    continue;
                }

                if let Err(e) = frecency.track_access(path) {
                    error!("Failed to track frecency for {:?}: {:?}", path, e);
                } else {
                    tracked_count += 1;
                }
            }
            if tracked_count > 0 {
                info!("AI mode: tracked frecency for {} files", tracked_count);
            }
        }

        // Update in-memory frecency scores for tracked files
        if tracked_count > 0
            && let Ok(mut picker_guard) = shared_picker.write()
            && let Some(ref mut picker) = *picker_guard
            && let Ok(frecency_guard) = shared_frecency.read()
            && let Some(ref frecency) = *frecency_guard
        {
            for path in &paths_to_add_or_modify {
                let _ = picker.update_single_file_frecency(path, frecency);
            }
        }
    }

    // do not try to update the paths if we anyway going to rescan everything from scratch
    // no repo => no consumer thread, so don't accumulate paths nobody will drain
    if !index_update_rejected && repo.is_some() {
        if need_full_git_rescan {
            // A full git rescan re-reads every tracked path (including ones that just
            // went clean after a commit), so it already subsumes the per-path update.
            git_status_worker.request_full_rescan();
        } else if !files_to_update_git_status.is_empty() {
            git_status_worker.enqueue_paths(files_to_update_git_status);
        }
    }

    new_dirs_to_watch
}

fn index_new_directory(
    dir: &Path,
    shared_picker: &SharedFilePicker,
    git_workdir: &Option<PathBuf>,
    git_status_worker: &Arc<GitStatusWorker>,
) -> Vec<PathBuf> {
    let repo = git_workdir.as_ref().and_then(|p| Repository::open(p).ok());
    // Prefer the walker's ignore rules; read base_path + rules from the picker.
    let (base_path, walker_rules, follow_symlinks) = match shared_picker.read().ok().and_then(|g| {
        g.as_ref().map(|p| {
            (
                p.base_path().to_path_buf(),
                p.ignore_rules(),
                p.follows_symlinks(),
            )
        })
    }) {
        Some(triple) => triple,
        None => return Vec::new(),
    };

    let walk = match crate::walk::walk_collect_files(
        dir,
        repo.is_some(),
        follow_symlinks,
        1,
        &Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    ) {
        Ok(walk) => walk,
        Err(e) => {
            warn!(?e, dir = %dir.display(), "Failed to walk new directory");
            return Vec::new();
        }
    };

    // TODO: figure out a better optimized way for zlob to rerun the directory walk using existing
    // ignore rules, but currently we have to filter out ignored files on our own
    let filter = IgnoreFilter::new(&base_path, walker_rules, repo.as_ref());
    let join_unless_ignored = |relative_path: &str| -> Option<PathBuf> {
        let path = dir.join(relative_path);
        (!filter.is_ignored(&path)).then_some(path)
    };

    let files_to_add: Vec<PathBuf> = walk
        .pairs
        .iter()
        .filter_map(|(_, path)| join_unless_ignored(path))
        .collect();

    let subdirs: Vec<PathBuf> = walk
        .dirs
        .iter()
        .filter_map(|path| join_unless_ignored(path.trim_end_matches('/')))
        .collect();

    if files_to_add.is_empty() {
        return subdirs;
    }

    let mut indexed_files = Vec::with_capacity(files_to_add.len());
    {
        let Ok(mut guard) = shared_picker.write() else {
            return subdirs;
        };

        let Some(ref mut picker) = *guard else {
            return subdirs;
        };

        for path in files_to_add {
            if picker.handle_create_or_modify(&path).is_some() {
                indexed_files.push(path);
            }
        }
    }
    let added = indexed_files.len();

    let watch_registry = shared_picker.watch_registry();
    if watch_registry.is_active() {
        let events = indexed_files
            .iter()
            .map(|path| RawWatchEvent {
                path: path.clone(),
                kind: WatchEventKind::Created,
                is_ignored: false,
                from: None,
            })
            .collect();

        watch_registry.dispatch(&base_path, events);
    }

    if repo.is_some() {
        git_status_worker.enqueue_paths(indexed_files);
    }

    debug!(
        "Indexed new {} files from new directory {}",
        added,
        dir.display(),
    );

    subdirs
}

#[cfg(target_os = "linux")]
fn watch_dirs_nonrecursive<'a>(
    debouncer: &Mutex<Option<Debouncer>>,
    dirs: impl Iterator<Item = &'a Path>,
) -> bool {
    let mut guard = debouncer.lock();
    let Some(debouncer) = guard.as_mut() else {
        return false;
    };

    for dir in dirs {
        if let Err(e) = debouncer.watch(dir, RecursiveMode::NonRecursive) {
            warn!(
                ?e,
                dir = %dir.display(),
                "Failed to init watcher for new directory"
            );
        }
    }

    true
}

struct IgnoreFilter<'a> {
    base_path: &'a Path,
    /// Reusable ignore rules from the last walk (zlob backend only).
    rules: Option<Arc<crate::walk::WalkIgnoreRules>>,
    /// libgit2 repo, consulted only when `rules` is `None`. Borrowed from the
    /// caller's repo (also used for git-status queries) to avoid re-opening.
    repo: Option<&'a Repository>,
}

impl<'a> IgnoreFilter<'a> {
    fn new(
        base_path: &'a Path,
        rules: Option<Arc<crate::walk::WalkIgnoreRules>>,
        repo: Option<&'a Repository>,
    ) -> Self {
        Self {
            base_path,
            rules,
            repo,
        }
    }

    /// Whether `path` (absolute) is ignored.
    fn is_ignored(&self, path: &Path) -> bool {
        if let Some(rules) = self.rules.as_ref() {
            let Ok(relative) = path.strip_prefix(self.base_path) else {
                return false;
            };
            // `IgnoreRules::is_ignored` enumerates every ancestor .gitignore
            // layer internally, so a leaf under an ignored directory (rule
            // `build/`, path `build/out.rs`) is caught in one call.
            return rules.is_ignored(relative);
        }
        match self.repo {
            Some(repo) => repo.is_path_ignored(path) == Ok(true),
            // No repo and no rules: the non-code-dir heuristic, applied to the
            // base-relative path so ancestors of the base (e.g. a temp dir
            // under AppData/Local on Windows) never match.
            None => crate::ignore::is_non_code_directory(
                path.strip_prefix(self.base_path).unwrap_or(path),
            ),
        }
    }
}

#[inline]
pub(crate) fn is_git_file(path: &Path) -> bool {
    // it could be in submodule
    path.components()
        .any(|component| component.as_os_str() == ".git")
}

fn is_dotgit_change_affecting_status(changed: &Path, repo: &Option<Repository>) -> bool {
    let Some(repo) = repo.as_ref() else {
        return false;
    };

    let git_dir = repo.path();

    if let Ok(path_in_git_dir) = changed.strip_prefix(git_dir) {
        // Only react to changes that rewrite the worktree state: commits,
        // staging, checkouts, merges, conflict resolution. Ref-only updates
        // under refs/ (fetch, push, tag writes, pack-refs) do not change
        // which files are modified/untracked, so we deliberately skip them
        // watching refs/ recursively would cost one inotify watch per ref
        // namespace on repos with many branches/remotes.
        if path_in_git_dir == Path::new("index") || path_in_git_dir == Path::new("index.lock") {
            return true;
        }

        if path_in_git_dir == Path::new("HEAD") {
            return true;
        }

        // some of the git ops are not involving nethier index nor HEAD change, or sometimes
        // index updates can arrive too late after the change - that's why we track the log
        // the actual user action, once user
        if path_in_git_dir == Path::new("logs/HEAD") {
            return true;
        }

        if let Some(fname) = path_in_git_dir.file_name().and_then(|f| f.to_str())
            && matches!(fname, "MERGE_HEAD" | "CHERRY_PICK_HEAD" | "REVERT_HEAD")
        {
            return true;
        }
    }

    false
}

fn watch_git_status_paths(debouncer: &mut Debouncer, git_workdir: Option<&PathBuf>) {
    let Some(workdir) = git_workdir else {
        return;
    };

    let git_dir = workdir.join(".git");
    if !git_dir.is_dir() {
        return;
    }

    // We have tried to be smart about the internal git state but
    // it appeared more harmful that it's worth it, so we just watch
    // for the most obvious paths like HEAD, MERGE_HEAD, index.lock
    if let Err(e) = debouncer.watch(&git_dir, RecursiveMode::NonRecursive) {
        warn!("Failed to watch .git directory: {}", e);
    }

    // `.git` above is non-recursive, so on Linux (per-dir inotify watches)
    // events for `logs/HEAD` — the commit-finished signal used by
    // `is_dotgit_change_affecting_status` — would never be delivered without
    // watching `.git/logs` itself. On macOS/Windows the recursive base watch
    // already covers it; an extra watch is harmless there.
    let logs_dir = git_dir.join("logs");
    if logs_dir.is_dir()
        && let Err(e) = debouncer.watch(&logs_dir, RecursiveMode::NonRecursive)
    {
        warn!("Failed to watch .git/logs directory: {}", e);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct FileIdentity {
    size: u64,
    mtime: u64,
}

// Returns destination -> source. Explicit pairs from the OS win outright; the
// rest is inferred from files that kept their exact size and mtime. Call while
// the removed paths are still indexed — that is the only source of their
// metadata once they are gone from disk.
fn detect_renames(
    picker: &crate::file_picker::FilePicker,
    base_path: &Path,
    paths_to_remove: &[(&Path, bool)],
    paths_to_add_or_modify: &[&Path],
    explicit: &[(PathBuf, PathBuf)],
) -> ahash::AHashMap<PathBuf, PathBuf> {
    // Subscribers only ever see paths relative to the base, so a move that
    // crosses the boundary stays a plain removal or creation.
    let mut renames: ahash::AHashMap<PathBuf, PathBuf> = explicit
        .iter()
        .filter(|(from, to)| from != to && from.starts_with(base_path) && to.starts_with(base_path))
        .map(|(from, to)| (to.clone(), from.clone()))
        .collect();

    if paths_to_remove.is_empty() || paths_to_add_or_modify.is_empty() {
        return renames;
    }

    let removed: Vec<(PathBuf, FileIdentity)> = paths_to_remove
        .iter()
        .filter(|(path, _)| !renames.values().any(|from| from == path))
        .filter_map(|(path, _)| {
            let file = picker.get_file_by_path(path)?;
            Some((
                path.to_path_buf(),
                FileIdentity {
                    size: file.size,
                    mtime: file.modified,
                },
            ))
        })
        .collect();

    let added: Vec<(PathBuf, FileIdentity)> = paths_to_add_or_modify
        .iter()
        .filter(|path| !renames.contains_key(**path))
        // A path already live in the index was edited, not moved into place.
        .filter(|path| {
            picker
                .get_file_by_path(path)
                .is_none_or(|file| file.is_deleted())
        })
        .filter_map(|path| {
            let meta = std::fs::metadata(path).ok()?;
            let mtime = meta
                .modified()
                .ok()?
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_secs();
            Some((
                path.to_path_buf(),
                FileIdentity {
                    size: meta.len(),
                    mtime,
                },
            ))
        })
        .collect();

    for (from, to) in pair_renames_by_identity(&removed, &added) {
        renames.insert(to, from);
    }

    renames
}

// Pair a removed with an added path when they are the only two files in the
// batch sharing a (size, mtime) — a rename preserves both exactly. Uniqueness
// on both sides is what stops bulk operations from cross-wiring histories.
fn pair_renames_by_identity(
    removed: &[(PathBuf, FileIdentity)],
    added: &[(PathBuf, FileIdentity)],
) -> Vec<(PathBuf, PathBuf)> {
    if removed.is_empty() || added.is_empty() {
        return Vec::new();
    }

    // `None` marks an identity claimed by more than one distinct path. The OS
    // repeats a path within a batch (FSEvents does routinely), and a repeat is
    // still one candidate, not a collision.
    fn index_by_identity(
        entries: &[(PathBuf, FileIdentity)],
    ) -> ahash::AHashMap<FileIdentity, Option<&Path>> {
        let mut by_identity: ahash::AHashMap<FileIdentity, Option<&Path>> =
            ahash::AHashMap::default();
        for (path, identity) in entries {
            by_identity
                .entry(*identity)
                .and_modify(|slot| {
                    if *slot != Some(path.as_path()) {
                        *slot = None;
                    }
                })
                .or_insert(Some(path.as_path()));
        }
        by_identity
    }

    let sources = index_by_identity(removed);
    let targets = index_by_identity(added);

    targets
        .into_iter()
        .filter_map(|(identity, target)| {
            let target = target?;
            let source = (*sources.get(&identity)?)?;
            Some((source.to_path_buf(), target.to_path_buf()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_picker::{FilePicker, FilePickerOptions};
    use crate::watch::{WatchEvent, WatchOptions};
    use notify::Event;
    use notify::event::{CreateKind, DataChange, ModifyKind, RemoveKind};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    #[test]
    fn replacement_batch_emits_one_modified_event() {
        let tmp = tempfile::tempdir().unwrap();
        let base = crate::path_utils::canonicalize(tmp.path()).unwrap();
        let path = base.join("file.txt");
        std::fs::write(&path, "before").unwrap();

        let shared_picker = SharedFilePicker::default();
        let shared_frecency = SharedFrecency::noop();
        let mut picker = FilePicker::new(FilePickerOptions {
            base_path: base.to_string_lossy().into_owned(),
            watch: false,
            ..Default::default()
        })
        .unwrap();
        picker.collect_files().unwrap();
        shared_picker.rebase_watches(&base);
        *shared_picker.write().unwrap() = Some(picker);

        let (sender, receiver) = mpsc::channel::<Vec<WatchEvent>>();
        shared_picker
            .watch_registry()
            .subscribe(
                &base,
                "**",
                WatchOptions::default(),
                Box::new(move |_, events| sender.send(events.to_vec()).unwrap()),
            )
            .unwrap();

        std::fs::write(&path, "after").unwrap();
        let now = Instant::now();
        let events = vec![
            DebouncedEvent::new(
                Event::new(EventKind::Remove(RemoveKind::File)).add_path(path.clone()),
                now,
            ),
            DebouncedEvent::new(
                Event::new(EventKind::Create(CreateKind::File)).add_path(path.clone()),
                now,
            ),
            DebouncedEvent::new(
                Event::new(EventKind::Modify(ModifyKind::Data(DataChange::Content)))
                    .add_path(path.clone()),
                now,
            ),
        ];

        handle_debounced_events(
            FFFMode::Neovim,
            events,
            &base,
            &None,
            &shared_picker,
            &shared_frecency,
            &GitStatusWorker::new(),
        );

        let received = receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].path, path);
        assert_eq!(received[0].kind, WatchEventKind::Modified);
    }

    fn identity(size: u64, mtime: u64) -> FileIdentity {
        FileIdentity { size, mtime }
    }

    fn entry(path: &str, size: u64, mtime: u64) -> (PathBuf, FileIdentity) {
        (PathBuf::from(path), identity(size, mtime))
    }

    #[test]
    fn unique_size_and_mtime_pairs_as_a_rename() {
        let pairs = pair_renames_by_identity(
            &[entry("/w/old.rs", 120, 1_700_000_000)],
            &[entry("/w/new.rs", 120, 1_700_000_000)],
        );

        assert_eq!(
            pairs,
            vec![(PathBuf::from("/w/old.rs"), PathBuf::from("/w/new.rs"))]
        );
    }

    #[test]
    fn ambiguous_identities_are_never_paired() {
        let two_removed = pair_renames_by_identity(
            &[entry("/w/a.rs", 10, 5), entry("/w/b.rs", 10, 5)],
            &[entry("/w/c.rs", 10, 5)],
        );
        assert!(
            two_removed.is_empty(),
            "two removed files sharing an identity are ambiguous: {two_removed:?}"
        );

        let two_added = pair_renames_by_identity(
            &[entry("/w/a.rs", 10, 5)],
            &[entry("/w/b.rs", 10, 5), entry("/w/c.rs", 10, 5)],
        );
        assert!(
            two_added.is_empty(),
            "two added files sharing an identity are ambiguous: {two_added:?}"
        );
    }

    #[test]
    fn a_repeated_path_is_one_candidate_not_a_collision() {
        // FSEvents routinely reports the same path several times per batch.
        let pairs = pair_renames_by_identity(
            &[entry("/w/old.rs", 15, 99), entry("/w/old.rs", 15, 99)],
            &[entry("/w/new.rs", 15, 99), entry("/w/new.rs", 15, 99)],
        );

        assert_eq!(
            pairs,
            vec![(PathBuf::from("/w/old.rs"), PathBuf::from("/w/new.rs"))]
        );
    }

    #[test]
    fn empty_files_are_paired_only_when_unambiguous() {
        let unique = pair_renames_by_identity(&[entry("/w/a", 0, 7)], &[entry("/w/b", 0, 7)]);
        assert_eq!(
            unique,
            vec![(PathBuf::from("/w/a"), PathBuf::from("/w/b"))],
            "a lone empty file still has a usable identity"
        );

        let colliding = pair_renames_by_identity(
            &[entry("/w/a", 0, 7), entry("/w/b", 0, 7)],
            &[entry("/w/c", 0, 7), entry("/w/d", 0, 7)],
        );
        assert!(colliding.is_empty(), "{colliding:?}");
    }

    #[test]
    fn differing_metadata_is_not_a_rename() {
        // git checkout rewrites mtime, so branch switches must never pair.
        let fresh_mtime = pair_renames_by_identity(
            &[entry("/w/old.rs", 120, 1_700_000_000)],
            &[entry("/w/new.rs", 120, 1_700_000_900)],
        );
        assert!(fresh_mtime.is_empty(), "{fresh_mtime:?}");

        let other_size = pair_renames_by_identity(
            &[entry("/w/old.rs", 120, 1_700_000_000)],
            &[entry("/w/new.rs", 121, 1_700_000_000)],
        );
        assert!(other_size.is_empty(), "{other_size:?}");
    }

    #[test]
    fn independent_identities_pair_side_by_side() {
        let pairs = pair_renames_by_identity(
            &[entry("/w/a.rs", 10, 1), entry("/w/b.rs", 20, 2)],
            &[entry("/w/y.rs", 20, 2), entry("/w/x.rs", 10, 1)],
        );

        let mut pairs = pairs;
        pairs.sort();
        assert_eq!(
            pairs,
            vec![
                (PathBuf::from("/w/a.rs"), PathBuf::from("/w/x.rs")),
                (PathBuf::from("/w/b.rs"), PathBuf::from("/w/y.rs")),
            ]
        );
    }

    #[test]
    fn dotgit_status_filter_matches_worktree_state_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(tmp.path()).unwrap();
        let git_dir = repo.path().to_path_buf();
        let repo = Some(repo);

        let affecting = ["index", "index.lock", "HEAD", "logs/HEAD", "MERGE_HEAD"];
        for p in affecting {
            assert!(
                is_dotgit_change_affecting_status(&git_dir.join(p), &repo),
                "{p} must trigger a git status rescan"
            );
        }

        // Ref-only updates (fetch/push/tags) and commit scratch files must not.
        let non_affecting = [
            "refs/heads/main",
            "refs/heads/main.lock",
            "logs/refs/remotes/origin/main",
            "COMMIT_EDITMSG",
            "packed-refs",
        ];
        for p in non_affecting {
            assert!(
                !is_dotgit_change_affecting_status(&git_dir.join(p), &repo),
                "{p} must NOT trigger a git status rescan"
            );
        }

        // Worktree paths outside .git never match.
        assert!(!is_dotgit_change_affecting_status(
            &tmp.path().join("src/main.rs"),
            &repo
        ));
        assert!(!is_dotgit_change_affecting_status(
            &git_dir.join("index"),
            &None
        ));
    }
}
