use crate::error::Error;
use crate::index::constraints::{GlobPattern, compile_one, glob_matches_into};
use parking_lot::Mutex;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use tracing::{debug, error};

/// Watcher subscription/watch id
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WatchId(pub u64);

pub(crate) type WatchCallback = Box<dyn Fn(WatchId, &[WatchEvent]) + Send + Sync>;

/// The kind of filesystem change.
///
/// Event kinds are normalized on a best-effort basis. Editors and operating
/// systems may represent the same operation with different native events.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchEventKind {
    Created = 0,
    Modified = 1,
    Removed = 2,
    /// Individual events were lost; rescan the reported path.
    Rescan = 3,
    /// The file moved. `path` is the destination, `from` the source.
    Renamed = 4,
}

impl WatchEventKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            WatchEventKind::Created => "created",
            WatchEventKind::Modified => "modified",
            WatchEventKind::Removed => "removed",
            WatchEventKind::Rescan => "rescan",
            WatchEventKind::Renamed => "renamed",
        }
    }
}

/// A single change notification delivered to subscribers.
#[derive(Debug, Clone)]
pub struct WatchEvent {
    /// Absolute affected path (the indexed base path for `Rescan`).
    pub path: PathBuf,
    pub kind: WatchEventKind,
    /// Source path, set only for [`WatchEventKind::Renamed`].
    pub from: Option<PathBuf>,
}

/// Per-subscription options.
#[derive(Debug, Clone, Default)]
pub struct WatchOptions {
    /// Additional glob or path-prefix exclusions.
    pub ignore: Vec<String>,
}

type WatchMask = u128;
const MAX_BATCH_EVENTS: usize = WatchMask::BITS as usize;

pub(crate) struct RawWatchEvent {
    pub(crate) path: PathBuf,
    pub(crate) kind: WatchEventKind,
    pub(crate) is_ignored: bool,
    pub(crate) from: Option<PathBuf>,
}

enum WatchMatcher {
    Glob(GlobPattern),
    Exact(PathBuf),
    Dir(PathBuf),
}

impl WatchMatcher {
    fn new(pattern: &str, base: &Path) -> Result<Self, Error> {
        let pattern = pattern.trim();
        if pattern.is_empty() {
            return Ok(WatchMatcher::Dir(PathBuf::new()));
        }

        let Some(relative) = relative_pattern(pattern, base) else {
            return Err(Error::InvalidGlobPattern {
                pattern: pattern.to_string(),
                reason: "watch patterns must be inside the indexed base path".into(),
            });
        };

        if fff_query_parser::glob_detect::has_wildcards(pattern) {
            let glob = relative.to_string_lossy().replace('\\', "/");
            return compile_one(&glob).map(WatchMatcher::Glob).ok_or_else(|| {
                Error::InvalidGlobPattern {
                    pattern: pattern.to_string(),
                    reason: "failed to compile glob".into(),
                }
            });
        }

        if base.join(&relative).is_dir() {
            return Ok(WatchMatcher::Dir(relative));
        }

        Ok(WatchMatcher::Exact(relative))
    }
}

#[derive(Default)]
struct SubIgnore {
    globs: Vec<GlobPattern>,
    prefixes: Vec<PathBuf>,
}

impl SubIgnore {
    fn prefix_matches(&self, path: &Path) -> bool {
        self.prefixes.iter().any(|prefix| path.starts_with(prefix))
    }
}

fn relative_pattern(pattern: &str, base: &Path) -> Option<PathBuf> {
    let expanded = crate::path_utils::expand_tilde(pattern);
    let relative = if expanded.is_absolute() || expanded.has_root() {
        match expanded.strip_prefix(base) {
            Ok(rel) => rel,
            // Windows: the caller may pass an 8.3 short-name or differently
            // cased path; canonicalize and retry before rejecting.
            Err(_) => {
                let canonical = crate::path_utils::canonicalize(&expanded).ok()?;
                return relative_from_canonical(&canonical, base);
            }
        }
    } else {
        &expanded
    };

    reject_parent_components(relative)
}

fn relative_from_canonical(canonical: &Path, base: &Path) -> Option<PathBuf> {
    let relative = canonical.strip_prefix(base).ok()?;
    reject_parent_components(relative)
}

fn reject_parent_components(path: &Path) -> Option<PathBuf> {
    if path
        .components()
        .any(|component| component == std::path::Component::ParentDir)
    {
        return None;
    }

    Some(path.components().collect())
}

fn resolve_sub_ignore(patterns: &[String], base: &Path) -> Result<SubIgnore, Error> {
    let mut ignore = SubIgnore::default();

    for pattern in patterns {
        let pattern = pattern.trim();
        if pattern.is_empty() {
            continue;
        }
        let Some(relative) = relative_pattern(pattern, base) else {
            return Err(Error::InvalidGlobPattern {
                pattern: pattern.to_string(),
                reason: "ignore patterns must be inside the indexed base path".into(),
            });
        };

        if fff_query_parser::glob_detect::has_wildcards(pattern) {
            match compile_one(&relative.to_string_lossy().replace('\\', "/")) {
                Some(compiled) => ignore.globs.push(compiled),
                None => {
                    return Err(Error::InvalidGlobPattern {
                        pattern: pattern.to_string(),
                        reason: "failed to compile ignore glob".into(),
                    });
                }
            }
        } else {
            ignore.prefixes.push(relative);
        }
    }

    Ok(ignore)
}

struct WatchSub {
    id: WatchId,
    matcher: WatchMatcher,
    ignore: SubIgnore,
    callback: WatchCallback,
    active: AtomicBool,
    epoch: AtomicU64,
}

impl WatchSub {
    fn filter_mask(&self, paths: &[&str], scratch: &mut Vec<usize>) -> WatchMask {
        let mut mask = 0;

        match &self.matcher {
            WatchMatcher::Glob(g) => {
                scratch.clear();
                glob_matches_into(g, paths, scratch);
                for &index in scratch.iter() {
                    mask |= 1 << index;
                }
            }
            WatchMatcher::Dir(d) => {
                for (index, path) in paths.iter().enumerate() {
                    if Path::new(path).starts_with(d) {
                        mask |= 1 << index;
                    }
                }
            }
            WatchMatcher::Exact(p) => {
                for (index, path) in paths.iter().enumerate() {
                    if Path::new(path) == p {
                        mask |= 1 << index;
                    }
                }
            }
        }

        // Subtract per-subscription ignores from the match mask.
        for g in &self.ignore.globs {
            scratch.clear();
            glob_matches_into(g, paths, scratch);
            for &index in scratch.iter() {
                mask &= !(1 << index);
            }
        }
        if !self.ignore.prefixes.is_empty() {
            for (index, path) in paths.iter().enumerate() {
                if self.ignore.prefix_matches(Path::new(path)) {
                    mask &= !(1 << index);
                }
            }
        }

        mask
    }
}

struct CallbackDelivery {
    sub: Arc<WatchSub>,
    events: Vec<WatchEvent>,
    epoch: u64,
}

enum CallbackMessage {
    Deliver(Vec<CallbackDelivery>),
    // used to drain all the callbacks and close the sender right after
    Drain(mpsc::Sender<()>),
    Stop,
}

#[derive(Default)]
struct CallbackDispatcherState {
    sender: Option<mpsc::Sender<CallbackMessage>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

#[derive(Default)]
struct CallbackDispatcher {
    state: Mutex<CallbackDispatcherState>,
}

impl CallbackDispatcher {
    /// We have to use a separate thread becuause the callback is the actual C function pointer which
    /// can block on the user side, we can not allow our watcher logic to get into deadlocked state
    fn start(&self) -> Result<(), Error> {
        let mut state = self.state.lock();
        if state.sender.is_some() {
            return Ok(());
        }

        let (sender, receiver) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("fff-watch-callback".into())
            .spawn(move || {
                while let Ok(message) = receiver.recv() {
                    match message {
                        CallbackMessage::Deliver(deliveries) => {
                            for delivery in deliveries {
                                if !delivery.sub.active.load(Ordering::Acquire)
                                    || delivery.sub.epoch.load(Ordering::Acquire) != delivery.epoch
                                {
                                    continue;
                                }

                                let id = delivery.sub.id;
                                let result =
                                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                        (delivery.sub.callback)(id, &delivery.events)
                                    }));
                                if result.is_err() {
                                    error!(sub = id.0, "watch callback panicked");
                                }
                            }
                        }
                        CallbackMessage::Drain(done) => {
                            let _ = done.send(());
                        }
                        CallbackMessage::Stop => break,
                    }
                }
            })
            .map_err(Error::WatchDispatcherStart)?;

        state.sender = Some(sender);
        state.thread = Some(thread);
        Ok(())
    }

    fn deliver(&self, deliveries: Vec<CallbackDelivery>) {
        if deliveries.is_empty() {
            return;
        }
        let Some(sender) = self.state.lock().sender.clone() else {
            error!("watch callback dispatcher is not running");
            return;
        };
        if sender.send(CallbackMessage::Deliver(deliveries)).is_err() {
            error!("watch callback dispatcher stopped unexpectedly");
        }
    }

    fn drain(&self) {
        let (sender, is_dispatch_thread) = {
            let state = self.state.lock();
            let Some(sender) = state.sender.as_ref() else {
                return;
            };
            let is_dispatch_thread = state
                .thread
                .as_ref()
                .is_some_and(|thread| thread.thread().id() == std::thread::current().id());
            (sender.clone(), is_dispatch_thread)
        };

        if is_dispatch_thread {
            return;
        }

        let (done_tx, done_rx) = mpsc::channel();
        if sender.send(CallbackMessage::Drain(done_tx)).is_ok() {
            let _ = done_rx.recv();
        }
    }
}

impl Drop for CallbackDispatcher {
    fn drop(&mut self) {
        let state = self.state.get_mut();
        if let Some(sender) = state.sender.take() {
            let _ = sender.send(CallbackMessage::Stop);
        }

        if let Some(thread) = state.thread.take()
            && thread.thread().id() != std::thread::current().id()
        {
            let _ = thread.join();
        }
    }
}

#[derive(Default)]
struct WatchRegistryState {
    subs: Vec<Arc<WatchSub>>,
    base_path: Option<PathBuf>,
    epoch: u64,
}

// External subscribers for one SharedFilePicker.
#[derive(Default)]
pub(crate) struct WatchRegistry {
    state: Mutex<WatchRegistryState>,
    dispatcher: CallbackDispatcher,
}

// Process-wide ids let FFI clients route all instances through one map.
static NEXT_WATCH_ID: AtomicU64 = AtomicU64::new(1);

impl WatchRegistry {
    #[inline]
    pub(crate) fn is_active(&self) -> bool {
        !self.state.lock().subs.is_empty()
    }

    pub(crate) fn subscribe(
        &self,
        base_path: &Path,
        pattern: &str,
        options: WatchOptions,
        callback: WatchCallback,
    ) -> Result<WatchId, Error> {
        let matcher = WatchMatcher::new(pattern, base_path)?;
        let ignore = resolve_sub_ignore(&options.ignore, base_path)?;

        let mut state = self.state.lock();
        if state.base_path.as_deref() != Some(base_path) {
            return Err(Error::WatchBaseChanged);
        }
        self.dispatcher.start()?;

        let id = WatchId(NEXT_WATCH_ID.fetch_add(1, Ordering::Relaxed));
        let sub = Arc::new(WatchSub {
            id,
            matcher,
            ignore,
            callback,
            active: AtomicBool::new(true),
            epoch: AtomicU64::new(state.epoch),
        });

        state.subs.push(sub);
        Ok(id)
    }

    pub(crate) fn unsubscribe(&self, id: WatchId) -> bool {
        let mut state = self.state.lock();
        let Some(idx) = state.subs.iter().position(|s| s.id == id) else {
            return false;
        };
        let sub = state.subs.swap_remove(idx);
        sub.active.store(false, Ordering::Release);
        drop(state);
        drop(sub);
        true
    }

    pub(crate) fn contains(&self, id: WatchId) -> bool {
        self.state.lock().subs.iter().any(|sub| sub.id == id)
    }

    pub(crate) fn shutdown(&self) {
        let mut state = self.state.lock();
        let subs = std::mem::take(&mut state.subs);
        for sub in &subs {
            sub.active.store(false, Ordering::Release);
        }
        drop(state);
    }

    pub(crate) fn shutdown_and_wait(&self) {
        self.shutdown();
        self.dispatcher.drain();
    }

    pub(crate) fn rebase(&self, base_path: &Path) {
        let mut state = self.state.lock();
        if state.base_path.as_deref() == Some(base_path) {
            return;
        }

        state.base_path = Some(base_path.to_path_buf());
        state.epoch = state.epoch.wrapping_add(1);
        for sub in &state.subs {
            sub.epoch.store(state.epoch, Ordering::Release);
        }
        drop(state);
        self.dispatcher.drain();
    }

    pub(crate) fn dispatch(&self, base_path: &Path, events: Vec<RawWatchEvent>) {
        if events.is_empty() {
            return;
        }

        let state = self.state.lock();
        if state.subs.is_empty() || state.base_path.as_deref() != Some(base_path) {
            return;
        }

        for batch in events.chunks(MAX_BATCH_EVENTS) {
            let mut paths = Vec::with_capacity(batch.len());
            let mut visible_mask = 0;
            let mut rescan_mask = 0;
            let has_renames = batch.iter().any(|event| event.from.is_some());
            // Parallel array of rename sources, so a subscription matching only
            // the pre-rename path still hears about the move.
            let mut from_paths = Vec::with_capacity(if has_renames { batch.len() } else { 0 });

            for (index, event) in batch.iter().enumerate() {
                let relative = event
                    .path
                    .strip_prefix(base_path)
                    .expect("watch event path must be inside the indexed base path");
                paths.push(relative.to_string_lossy().replace('\\', "/"));

                if has_renames {
                    let source = event
                        .from
                        .as_deref()
                        .and_then(|from| from.strip_prefix(base_path).ok())
                        .map(|rel| rel.to_string_lossy().replace('\\', "/"));
                    from_paths.push(source.unwrap_or_else(|| paths[index].clone()));
                }

                let bit = 1 << index;
                if event.kind == WatchEventKind::Rescan {
                    rescan_mask |= bit;
                } else if !event.is_ignored {
                    visible_mask |= bit;
                }
            }

            let path_refs: Vec<&str> = paths.iter().map(String::as_str).collect();
            let from_refs: Vec<&str> = from_paths.iter().map(String::as_str).collect();
            let mut scratch = Vec::new();
            let mut deliveries = Vec::with_capacity(state.subs.len());
            for sub in &state.subs {
                let mut matched = sub.filter_mask(&path_refs, &mut scratch);
                if has_renames {
                    matched |= sub.filter_mask(&from_refs, &mut scratch);
                }
                let mut delivery_mask = (matched & visible_mask) | rescan_mask;
                if delivery_mask == 0 {
                    continue;
                }

                let mut filtered = Vec::with_capacity(delivery_mask.count_ones() as usize);
                while delivery_mask != 0 {
                    let index = delivery_mask.trailing_zeros() as usize;
                    let event = &batch[index];
                    filtered.push(WatchEvent {
                        path: event.path.clone(),
                        kind: event.kind,
                        from: event.from.clone(),
                    });
                    delivery_mask &= delivery_mask - 1;
                }

                debug!(
                    sub = sub.id.0,
                    count = filtered.len(),
                    "queueing watch events"
                );
                deliveries.push(CallbackDelivery {
                    sub: Arc::clone(sub),
                    events: filtered,
                    epoch: state.epoch,
                });
            }
            self.dispatcher.deliver(deliveries);
        }
    }

    // Signal that individual events were lost.
    pub(crate) fn dispatch_rescan(&self, base_path: &Path) {
        self.dispatch(
            base_path,
            vec![RawWatchEvent {
                path: base_path.to_path_buf(),
                kind: WatchEventKind::Rescan,
                is_ignored: false,
                from: None,
            }],
        );
    }
}

impl Drop for WatchRegistry {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::{Condvar, Mutex};
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    use std::time::Duration;

    fn raw(path: &str, kind: WatchEventKind, is_ignored: bool) -> RawWatchEvent {
        RawWatchEvent {
            path: PathBuf::from(path),
            kind,
            is_ignored,
            from: None,
        }
    }

    fn registry(base: &Path) -> Arc<WatchRegistry> {
        let registry = Arc::new(WatchRegistry::default());
        registry.rebase(base);
        registry
    }

    type Collected = Arc<Mutex<Vec<WatchEvent>>>;

    // Appends every delivered event.
    fn collector() -> (WatchCallback, Collected) {
        let collected: Collected = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&collected);
        let cb: WatchCallback = Box::new(move |_id, events| sink.lock().extend_from_slice(events));
        (cb, collected)
    }

    // Dispatch is asynchronous.
    fn wait_events(collected: &Collected, n: usize) -> Vec<WatchEvent> {
        for _ in 0..1000 {
            if collected.lock().len() >= n {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        collected.lock().clone()
    }

    #[test]
    fn dispatcher_starts_on_first_subscription() {
        let base = Path::new("/repo");
        let registry = WatchRegistry::default();
        registry.rebase(base);

        assert!(registry.dispatcher.state.lock().thread.is_none());
        let (cb, _) = collector();
        registry
            .subscribe(base, "**", WatchOptions::default(), cb)
            .unwrap();
        assert!(registry.dispatcher.state.lock().thread.is_some());
    }

    #[test]
    fn resolve_relative_glob() {
        let base = Path::new("/repo");
        assert!(matches!(
            WatchMatcher::new("./**/*.rs", base).unwrap(),
            WatchMatcher::Glob(_)
        ));
        assert!(matches!(
            WatchMatcher::new("src/*.ts", base).unwrap(),
            WatchMatcher::Glob(_)
        ));
    }

    #[test]
    fn reject_absolute_glob_outside_base() {
        assert!(WatchMatcher::new("/other/**/*.rs", Path::new("/repo")).is_err());
    }

    #[test]
    fn resolve_exact_paths() {
        let base = std::env::temp_dir();
        let inside = base.join("some_file.txt");
        match WatchMatcher::new(inside.to_str().unwrap(), &base).unwrap() {
            WatchMatcher::Exact(path) => assert_eq!(path, Path::new("some_file.txt")),
            _ => panic!("expected exact"),
        }
        match WatchMatcher::new("relative_file.txt", &base).unwrap() {
            WatchMatcher::Exact(path) => assert_eq!(path, Path::new("relative_file.txt")),
            _ => panic!("expected exact"),
        }
        assert!(WatchMatcher::new("../outside", &base).is_err());
    }

    #[test]
    fn resolve_existing_dir_as_subtree() {
        let tmp = tempfile::TempDir::new().unwrap();
        let base = tmp.path().to_path_buf();
        std::fs::create_dir(base.join("src")).unwrap();

        match WatchMatcher::new("src", &base).unwrap() {
            WatchMatcher::Dir(path) => assert_eq!(path, Path::new("src")),
            _ => panic!("expected dir"),
        }
        match WatchMatcher::new(base.to_str().unwrap(), &base).unwrap() {
            WatchMatcher::Dir(path) => assert!(path.as_os_str().is_empty()),
            _ => panic!("expected dir"),
        }
    }

    #[test]
    fn resolve_empty_pattern_as_whole_tree() {
        let tmp = tempfile::TempDir::new().unwrap();
        let base = tmp.path().to_path_buf();

        for pattern in ["", "   "] {
            match WatchMatcher::new(pattern, &base).unwrap() {
                WatchMatcher::Dir(path) => assert!(path.as_os_str().is_empty()),
                _ => panic!("expected dir"),
            }
        }
    }

    #[test]
    fn mixed_batch_dispatch_preserves_order_and_filters() {
        let base = Path::new("/repo");
        let registry = registry(base);

        let (glob_cb, glob_events) = collector();
        registry
            .subscribe(
                base,
                "**/*.rs",
                WatchOptions {
                    ignore: vec!["src/vendor".into(), "*.gen.rs".into()],
                },
                glob_cb,
            )
            .unwrap();
        let (dir_cb, dir_events) = collector();
        registry
            .subscribe(base, "src/**", WatchOptions::default(), dir_cb)
            .unwrap();
        let (exact_cb, exact_events) = collector();
        registry
            .subscribe(base, "dist/out.js", WatchOptions::default(), exact_cb)
            .unwrap();

        registry.dispatch(
            base,
            vec![
                raw("/repo/src/a.rs", WatchEventKind::Created, false),
                raw("/repo/src/b.gen.rs", WatchEventKind::Modified, false),
                raw("/repo/src/vendor/c.rs", WatchEventKind::Modified, false),
                raw("/repo/lib/d.rs", WatchEventKind::Removed, false),
                raw("/repo/dist/out.js", WatchEventKind::Modified, true), // index-ignored
                raw("/repo/src/e.txt", WatchEventKind::Created, true),    // index-ignored
            ],
        );

        let glob = wait_events(&glob_events, 2);
        let paths: Vec<_> = glob.iter().map(|e| e.path.clone()).collect();
        assert_eq!(
            paths,
            vec![
                PathBuf::from("/repo/src/a.rs"),
                PathBuf::from("/repo/lib/d.rs"),
            ]
        );

        let dir = wait_events(&dir_events, 3);
        let paths: Vec<_> = dir.iter().map(|e| e.path.clone()).collect();
        assert_eq!(
            paths,
            vec![
                PathBuf::from("/repo/src/a.rs"),
                PathBuf::from("/repo/src/b.gen.rs"),
                PathBuf::from("/repo/src/vendor/c.rs"),
            ]
        );

        assert!(exact_events.lock().is_empty());
    }

    #[test]
    fn rescan_is_broadcast_to_every_subscription() {
        let base = Path::new("/repo");
        let registry = registry(base);
        let (glob_cb, glob_events) = collector();
        let (exact_cb, exact_events) = collector();
        registry
            .subscribe(base, "src/**", WatchOptions::default(), glob_cb)
            .unwrap();
        registry
            .subscribe(base, "dist/out.js", WatchOptions::default(), exact_cb)
            .unwrap();

        registry.dispatch_rescan(base);

        for events in [&glob_events, &exact_events] {
            let events = wait_events(events, 1);
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].path, base);
            assert_eq!(events[0].kind, WatchEventKind::Rescan);
        }
    }

    #[test]
    fn registry_dispatch_matches_glob_and_batches() {
        let base = Path::new("/repo");
        let registry = registry(base);
        let hits = Arc::new(Mutex::new(Vec::<WatchEvent>::new()));
        let calls = Arc::new(AtomicUsize::new(0));

        let hits_cb = Arc::clone(&hits);
        let calls_cb = Arc::clone(&calls);
        let id = registry
            .subscribe(
                base,
                "**/*.rs",
                WatchOptions::default(),
                Box::new(move |_id, events| {
                    calls_cb.fetch_add(1, Ordering::SeqCst);
                    hits_cb.lock().extend_from_slice(events);
                }),
            )
            .unwrap();

        registry.dispatch(
            base,
            vec![
                raw("/repo/src/a.rs", WatchEventKind::Modified, false),
                raw("/repo/src/b.ts", WatchEventKind::Modified, false),
                raw("/repo/target/c.rs", WatchEventKind::Created, true),
            ],
        );

        for _ in 0..400 {
            if calls.load(Ordering::SeqCst) == 1 {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let events = hits.lock();
        // ignored + non-matching + out-of-tree are all filtered, in ONE call
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].path, PathBuf::from("/repo/src/a.rs"));

        assert!(registry.unsubscribe(id));
        assert!(!registry.is_active());
        assert!(!registry.unsubscribe(id));
    }

    #[test]
    fn large_batch_is_delivered_without_coalescing() {
        let base = Path::new("/repo");
        let registry = registry(base);

        let collected: Collected = Arc::new(Mutex::new(Vec::new()));
        let batch_sizes = Arc::new(Mutex::new(Vec::new()));
        let collected_cb = Arc::clone(&collected);
        let batch_sizes_cb = Arc::clone(&batch_sizes);
        registry
            .subscribe(
                base,
                "**/*.rs",
                WatchOptions::default(),
                Box::new(move |_, events| {
                    batch_sizes_cb.lock().push(events.len());
                    collected_cb.lock().extend_from_slice(events);
                }),
            )
            .unwrap();

        let events: Vec<RawWatchEvent> = (0..257)
            .map(|i| {
                raw(
                    &format!("/repo/src/f{i}.rs"),
                    WatchEventKind::Modified,
                    false,
                )
            })
            .collect();
        registry.dispatch(base, events);

        let delivered = wait_events(&collected, 257);
        assert_eq!(delivered.len(), 257);
        assert!(
            delivered
                .iter()
                .all(|event| event.kind == WatchEventKind::Modified)
        );
        assert_eq!(*batch_sizes.lock(), vec![128, 128, 1]);
    }

    #[test]
    fn duplicate_paths_are_delivered_in_order() {
        let base = Path::new("/repo");
        let registry = registry(base);
        let (cb, collected) = collector();
        registry
            .subscribe(base, "**", WatchOptions::default(), cb)
            .unwrap();

        registry.dispatch(
            base,
            vec![
                raw("/repo/a.rs", WatchEventKind::Created, false),
                raw("/repo/a.rs", WatchEventKind::Modified, false),
                raw("/repo/a.rs", WatchEventKind::Removed, false),
            ],
        );

        let delivered = wait_events(&collected, 3);
        let kinds: Vec<_> = delivered.iter().map(|event| event.kind).collect();
        assert_eq!(
            kinds,
            vec![
                WatchEventKind::Created,
                WatchEventKind::Modified,
                WatchEventKind::Removed,
            ]
        );
    }

    #[test]
    fn rebase_keeps_relative_subscriptions() {
        let old_base = Path::new("/old");
        let new_base = Path::new("/new");
        let registry = registry(old_base);
        let (cb, collected) = collector();
        let id = registry
            .subscribe(old_base, "src/**", WatchOptions::default(), cb)
            .unwrap();

        registry.dispatch(
            old_base,
            vec![raw("/old/src/a.rs", WatchEventKind::Created, false)],
        );
        assert_eq!(wait_events(&collected, 1).len(), 1);

        registry.rebase(new_base);
        assert!(registry.contains(id));
        registry.dispatch(
            old_base,
            vec![raw("/old/src/stale.rs", WatchEventKind::Created, false)],
        );
        registry.dispatch(
            new_base,
            vec![raw("/new/src/b.rs", WatchEventKind::Created, false)],
        );

        let delivered = wait_events(&collected, 2);
        let paths: Vec<_> = delivered.iter().map(|event| event.path.clone()).collect();
        assert_eq!(
            paths,
            vec![
                PathBuf::from("/old/src/a.rs"),
                PathBuf::from("/new/src/b.rs")
            ]
        );
    }

    #[test]
    fn rebase_from_callback_skips_queued_old_events() {
        let old_base = Path::new("/old");
        let new_base = PathBuf::from("/new");
        let registry = registry(old_base);
        let collected: Collected = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&collected);
        let weak = Arc::downgrade(&registry);
        let callback_base = new_base.clone();

        registry
            .subscribe(
                old_base,
                "**",
                WatchOptions::default(),
                Box::new(move |_, events| {
                    sink.lock().extend_from_slice(events);
                    if let Some(registry) = weak.upgrade() {
                        registry.rebase(&callback_base);
                    }
                }),
            )
            .unwrap();

        registry.dispatch(
            old_base,
            (0..129)
                .map(|index| {
                    raw(
                        &format!("/old/file-{index}"),
                        WatchEventKind::Modified,
                        false,
                    )
                })
                .collect(),
        );

        let old_events = wait_events(&collected, 128);
        assert_eq!(old_events.len(), 128);
        assert!(
            !old_events
                .iter()
                .any(|event| event.path == Path::new("/old/file-128"))
        );

        registry.dispatch(
            &new_base,
            vec![raw("/new/current", WatchEventKind::Created, false)],
        );
        let events = wait_events(&collected, 129);
        assert_eq!(events.last().unwrap().path, Path::new("/new/current"));
    }

    #[test]
    fn callback_panic_does_not_stop_dispatcher() {
        let base = Path::new("/repo");
        let registry = registry(base);
        registry
            .subscribe(
                base,
                "**",
                WatchOptions::default(),
                Box::new(|_, _| panic!("test callback panic")),
            )
            .unwrap();
        let (cb, collected) = collector();
        registry
            .subscribe(base, "**", WatchOptions::default(), cb)
            .unwrap();

        registry.dispatch(
            base,
            vec![raw("/repo/a.rs", WatchEventKind::Created, false)],
        );
        assert_eq!(wait_events(&collected, 1).len(), 1);
    }

    #[test]
    fn shutdown_and_wait_joins_in_flight_callback() {
        let base = Path::new("/repo");
        let registry = registry(base);
        let (started_tx, started_rx) = mpsc::channel();
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let release_cb = Arc::clone(&release);
        registry
            .subscribe(
                base,
                "**",
                WatchOptions::default(),
                Box::new(move |_, _| {
                    let _ = started_tx.send(());
                    let (released, ready) = &*release_cb;
                    ready.wait_while(&mut released.lock(), |released| !*released);
                }),
            )
            .unwrap();
        registry.dispatch(
            base,
            vec![raw("/repo/a.rs", WatchEventKind::Created, false)],
        );
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let registry_wait = Arc::clone(&registry);
        let waiting = std::thread::spawn(move || registry_wait.shutdown_and_wait());
        std::thread::sleep(Duration::from_millis(20));
        assert!(!waiting.is_finished());

        let (released, ready) = &*release;
        *released.lock() = true;
        ready.notify_all();
        waiting.join().unwrap();
        assert!(!registry.is_active());
    }

    #[test]
    fn index_ignored_events_are_never_delivered() {
        let base = Path::new("/repo");
        let registry = registry(base);

        let (cb, events) = collector();
        registry
            .subscribe(base, "dist/**", WatchOptions::default(), cb)
            .unwrap();

        registry.dispatch(
            base,
            vec![
                raw("/repo/dist/bundle.js", WatchEventKind::Created, true),
                raw("/repo/dist/keep.js", WatchEventKind::Created, false),
            ],
        );

        let got = wait_events(&events, 1);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].path, PathBuf::from("/repo/dist/keep.js"));
    }

    #[test]
    fn callback_receives_its_subscription_id_and_ids_are_unique() {
        let base = Path::new("/repo");
        let a = registry(base);
        let b = registry(base);

        let seen_id = Arc::new(Mutex::new(None::<WatchId>));
        let seen_cb = Arc::clone(&seen_id);
        let id_a = a
            .subscribe(
                base,
                "**",
                WatchOptions::default(),
                Box::new(move |id, _| {
                    *seen_cb.lock() = Some(id);
                }),
            )
            .unwrap();
        let (b_cb, _b_events) = collector();
        let id_b = b
            .subscribe(base, "**", WatchOptions::default(), b_cb)
            .unwrap();

        // ids are process-wide unique, even across registries (instances)
        assert_ne!(id_a, id_b);

        a.dispatch(
            base,
            vec![raw("/repo/a.rs", WatchEventKind::Modified, false)],
        );
        for _ in 0..200 {
            if seen_id.lock().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(*seen_id.lock(), Some(id_a));
    }

    #[test]
    fn shutdown_quiesces_and_allows_restart() {
        let base = Path::new("/repo");
        let registry = registry(base);
        let calls = Arc::new(AtomicUsize::new(0));

        let calls_cb = Arc::clone(&calls);
        registry
            .subscribe(
                base,
                "**",
                WatchOptions::default(),
                Box::new(move |_, _| {
                    calls_cb.fetch_add(1, Ordering::SeqCst);
                }),
            )
            .unwrap();

        registry.shutdown();
        assert!(!registry.is_active());

        // after shutdown: no deliveries
        registry.dispatch(
            base,
            vec![raw("/repo/a.rs", WatchEventKind::Modified, false)],
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        // shutdown is idempotent and the registry restarts on next subscribe
        registry.shutdown();
        let (cb, events) = collector();
        registry
            .subscribe(base, "**", WatchOptions::default(), cb)
            .unwrap();
        registry.dispatch(
            base,
            vec![raw("/repo/b.rs", WatchEventKind::Created, false)],
        );
        assert_eq!(wait_events(&events, 1).len(), 1);
    }

    #[test]
    fn unsubscribe_from_inside_callback_does_not_deadlock() {
        let base = Path::new("/repo");
        let registry = registry(base);
        let unsubscribed = Arc::new(AtomicBool::new(false));

        let registry_cb = Arc::downgrade(&registry);
        let unsub_cb = Arc::clone(&unsubscribed);
        // one-shot pattern: the callback removes its own subscription
        registry
            .subscribe(
                base,
                "**",
                WatchOptions::default(),
                Box::new(move |id, _| {
                    if let Some(registry) = registry_cb.upgrade() {
                        registry.unsubscribe(id);
                        unsub_cb.store(true, Ordering::SeqCst);
                    }
                }),
            )
            .unwrap();

        registry.dispatch(
            base,
            vec![raw("/repo/a.rs", WatchEventKind::Modified, false)],
        );

        for _ in 0..400 {
            if unsubscribed.load(Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            unsubscribed.load(Ordering::SeqCst),
            "self-unsubscribe from the callback deadlocked"
        );
        assert!(!registry.is_active());
    }
}
