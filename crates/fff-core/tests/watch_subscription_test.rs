use fff_search::file_picker::{FFFMode, FilePicker};
use fff_search::{
    FilePickerOptions, SharedFilePicker, SharedFrecency, WatchEvent, WatchEventKind, WatchOptions,
};
use parking_lot::Mutex;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tempfile::TempDir;

fn make_watched_picker(base: &Path) -> (SharedFilePicker, SharedFrecency) {
    let shared_picker = SharedFilePicker::default();
    let shared_frecency = SharedFrecency::noop();

    FilePicker::new_with_shared_state(
        shared_picker.clone(),
        shared_frecency.clone(),
        FilePickerOptions {
            base_path: base.to_string_lossy().into_owned(),
            enable_mmap_cache: false,
            enable_content_indexing: false,
            mode: FFFMode::Neovim,
            watch: true,
            ..Default::default()
        },
    )
    .expect("FilePicker::new_with_shared_state");

    assert!(
        shared_picker.wait_for_scan(Duration::from_secs(30)),
        "initial scan did not complete"
    );
    assert!(
        shared_picker.wait_for_watcher(Duration::from_secs(30)),
        "watcher did not install"
    );
    // macOS FSEvents streams need a beat before they deliver reliably
    std::thread::sleep(Duration::from_millis(300));

    (shared_picker, shared_frecency)
}

fn wait_for<F: Fn() -> bool>(cond: F, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    cond()
}

fn seed(base: &Path) {
    fs::create_dir_all(base.join("src")).unwrap();
    fs::write(base.join("src/main.rs"), "fn main() {}\n").unwrap();
    fs::write(base.join("README.md"), "# seed\n").unwrap();
}

type Collected = Arc<Mutex<Vec<WatchEvent>>>;

/// Subscribe with a collector callback; returns the shared event sink.
fn watch_collect(picker: &SharedFilePicker, pattern: &str, options: WatchOptions) -> Collected {
    let collected: Collected = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&collected);
    picker
        .watch(pattern, options, move |_id, events| {
            sink.lock().extend_from_slice(events)
        })
        .expect("watch subscription failed");
    collected
}

#[test]
fn glob_subscription_receives_created_and_removed_events() {
    let tmp = TempDir::new().unwrap();
    let base = fff_search::path_utils::canonicalize(tmp.path()).unwrap();
    seed(&base);
    let (picker, _frecency) = make_watched_picker(&base);

    let events: Arc<Mutex<Vec<WatchEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let calls = Arc::new(AtomicUsize::new(0));

    let events_cb = Arc::clone(&events);
    let calls_cb = Arc::clone(&calls);
    let id = picker
        .watch("**/*.rs", WatchOptions::default(), move |_id, batch| {
            calls_cb.fetch_add(1, Ordering::SeqCst);
            events_cb.lock().extend_from_slice(batch);
        })
        .expect("subscribe glob");

    let rs_file = base.join("src/new_module.rs");
    let ts_file = base.join("src/ignored_by_glob.ts");
    fs::write(&rs_file, "pub fn hi() {}\n").unwrap();
    fs::write(&ts_file, "export {};\n").unwrap();

    assert!(
        wait_for(
            || events.lock().iter().any(|e| e.path == rs_file),
            Duration::from_secs(10)
        ),
        "did not receive event for created .rs file, got: {:?}",
        events.lock()
    );
    assert!(
        !events.lock().iter().any(|e| e.path == ts_file),
        ".ts file must not match the *.rs glob"
    );

    fs::remove_file(&rs_file).unwrap();
    assert!(
        wait_for(
            || events
                .lock()
                .iter()
                .any(|e| e.path == rs_file && e.kind == WatchEventKind::Removed),
            Duration::from_secs(10)
        ),
        "did not receive Removed event, got: {:?}",
        events.lock()
    );

    // batching: each debounce window is one callback invocation, so the call
    // count must be well below the delivered event count + noise ceiling
    assert!(calls.load(Ordering::SeqCst) <= events.lock().len() + 2);

    assert!(picker.unwatch(id));
    let count_after = events.lock().len();
    fs::write(base.join("src/after_unsub.rs"), "\n").unwrap();
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        events.lock().len(),
        count_after,
        "no events after unsubscribe"
    );
}

#[test]
fn watch_events_reflect_applied_file_transitions() {
    let tmp = TempDir::new().unwrap();
    let base = fff_search::path_utils::canonicalize(tmp.path()).unwrap();
    let removed_path = base.join("removed.txt");
    let created_path = base.join("created.txt");
    let replaced_path = base.join("replaced.txt");
    fs::write(&removed_path, "remove me").unwrap();
    fs::write(&replaced_path, "before").unwrap();

    let (picker, _frecency) = make_watched_picker(&base);
    let removed = watch_collect(
        &picker,
        removed_path.to_str().unwrap(),
        WatchOptions::default(),
    );
    let created = watch_collect(
        &picker,
        created_path.to_str().unwrap(),
        WatchOptions::default(),
    );
    let replaced = watch_collect(
        &picker,
        replaced_path.to_str().unwrap(),
        WatchOptions::default(),
    );

    fs::remove_file(&removed_path).unwrap();
    assert!(
        wait_for(|| !removed.lock().is_empty(), Duration::from_secs(10)),
        "remove event was not delivered"
    );

    fs::write(&created_path, "created").unwrap();
    assert!(
        wait_for(|| !created.lock().is_empty(), Duration::from_secs(10)),
        "create event was not delivered"
    );

    fs::remove_file(&replaced_path).unwrap();
    fs::write(&replaced_path, "after").unwrap();
    assert!(
        wait_for(|| !replaced.lock().is_empty(), Duration::from_secs(10)),
        "replacement event was not delivered"
    );

    std::thread::sleep(Duration::from_millis(300));
    let removed = removed.lock();
    assert_eq!(removed.len(), 1, "unexpected remove events: {removed:?}");
    assert_eq!(removed[0].path, removed_path);
    assert_eq!(removed[0].kind, WatchEventKind::Removed);

    let created = created.lock();
    assert_eq!(created.len(), 1, "unexpected create events: {created:?}");
    assert_eq!(created[0].path, created_path);
    assert_eq!(created[0].kind, WatchEventKind::Created);

    let replaced = replaced.lock();
    assert_eq!(
        replaced.len(),
        1,
        "replacement must be one event: {replaced:?}"
    );
    assert_eq!(replaced[0].path, replaced_path);
    assert_eq!(replaced[0].kind, WatchEventKind::Modified);
}

#[test]
fn removed_directory_delivers_removed_event_per_file() {
    let tmp = TempDir::new().unwrap();
    let base = fff_search::path_utils::canonicalize(tmp.path()).unwrap();
    seed(&base);
    let dir = base.join("doomed");
    fs::create_dir_all(dir.join("nested")).unwrap();
    let files = [dir.join("a.rs"), dir.join("b.txt"), dir.join("nested/c.rs")];
    for f in &files {
        fs::write(f, "content\n").unwrap();
    }

    let (picker, _frecency) = make_watched_picker(&base);
    let events = watch_collect(&picker, "", WatchOptions::default());

    fs::remove_dir_all(&dir).unwrap();

    assert!(
        wait_for(
            || {
                let got = events.lock();
                files.iter().all(|f| {
                    got.iter()
                        .any(|e| e.path == *f && e.kind == WatchEventKind::Removed)
                })
            },
            Duration::from_secs(10)
        ),
        "expected Removed for every file in the removed dir, got: {:?}",
        events.lock()
    );
}

#[test]
fn moved_out_directory_delivers_removed_event_per_file() {
    let tmp = TempDir::new().unwrap();
    let trash = TempDir::new().unwrap();
    let base = fff_search::path_utils::canonicalize(tmp.path()).unwrap();
    seed(&base);
    let dir = base.join("doomed");
    fs::create_dir_all(dir.join("nested")).unwrap();
    let files = [dir.join("a.rs"), dir.join("b.txt"), dir.join("nested/c.rs")];
    for f in &files {
        fs::write(f, "content\n").unwrap();
    }

    let (picker, _frecency) = make_watched_picker(&base);
    let events = watch_collect(&picker, "", WatchOptions::default());

    // mimics `mv dir elsewhere` / Finder trash: one rename event on the dir,
    // no per-file remove events from the OS
    fs::rename(&dir, trash.path().join("doomed")).unwrap();

    assert!(
        wait_for(
            || {
                let got = events.lock();
                files.iter().all(|f| {
                    got.iter()
                        .any(|e| e.path == *f && e.kind == WatchEventKind::Removed)
                })
            },
            Duration::from_secs(10)
        ),
        "expected Removed for every file in the moved-out dir, got: {:?}",
        events.lock()
    );
}

#[test]
fn renaming_a_file_delivers_one_renamed_event_with_both_paths() {
    let tmp = TempDir::new().unwrap();
    let base = fff_search::path_utils::canonicalize(tmp.path()).unwrap();
    seed(&base);
    let old = base.join("src/renamed_from.rs");
    fs::write(&old, "pub fn hi() {}\n").unwrap();

    let (picker, _frecency) = make_watched_picker(&base);
    let events = watch_collect(&picker, "", WatchOptions::default());

    let new = base.join("src/renamed_to.rs");
    fs::rename(&old, &new).unwrap();

    assert!(
        wait_for(
            || events
                .lock()
                .iter()
                .any(|e| e.kind == WatchEventKind::Renamed && e.path == new),
            Duration::from_secs(10)
        ),
        "expected a Renamed event for {}, got: {:?}",
        new.display(),
        events.lock()
    );

    let got = events.lock();
    let renamed = got
        .iter()
        .find(|e| e.kind == WatchEventKind::Renamed)
        .unwrap();
    assert_eq!(renamed.from.as_deref(), Some(old.as_path()));
    assert!(
        !got.iter()
            .any(|e| e.path == old && e.kind == WatchEventKind::Removed),
        "a recognised rename must not also report the source as Removed: {got:?}"
    );
}

#[test]
fn renaming_out_of_a_watched_subtree_still_notifies() {
    let tmp = TempDir::new().unwrap();
    let base = fff_search::path_utils::canonicalize(tmp.path()).unwrap();
    seed(&base);
    fs::create_dir_all(base.join("vendor")).unwrap();
    let inside = base.join("src/leaving.rs");
    fs::write(&inside, "pub fn leaving() {}\n").unwrap();

    let (picker, _frecency) = make_watched_picker(&base);
    // Only `src/` is watched; the destination is outside it.
    let events = watch_collect(&picker, "src", WatchOptions::default());

    let outside = base.join("vendor/leaving.rs");
    fs::rename(&inside, &outside).unwrap();

    assert!(
        wait_for(
            || events
                .lock()
                .iter()
                .any(|e| e.kind == WatchEventKind::Renamed
                    && e.from.as_deref() == Some(inside.as_path())),
            Duration::from_secs(10)
        ),
        "a subscriber on src/ must hear that a file moved out of it, got: {:?}",
        events.lock()
    );
}

#[test]
fn renaming_into_a_watched_subtree_still_notifies() {
    let tmp = TempDir::new().unwrap();
    let base = fff_search::path_utils::canonicalize(tmp.path()).unwrap();
    seed(&base);
    fs::create_dir_all(base.join("vendor")).unwrap();
    let outside = base.join("vendor/arriving.rs");
    fs::write(&outside, "pub fn arriving() {}\n").unwrap();

    let (picker, _frecency) = make_watched_picker(&base);
    let events = watch_collect(&picker, "src", WatchOptions::default());

    let inside = base.join("src/arriving.rs");
    fs::rename(&outside, &inside).unwrap();

    assert!(
        wait_for(
            || events
                .lock()
                .iter()
                .any(|e| e.kind == WatchEventKind::Renamed && e.path == inside),
            Duration::from_secs(10)
        ),
        "a subscriber on src/ must hear that a file moved into it, got: {:?}",
        events.lock()
    );
}

#[test]
fn renaming_across_the_base_boundary_degrades_to_removed() {
    let tmp = TempDir::new().unwrap();
    let outside_root = TempDir::new().unwrap();
    let base = fff_search::path_utils::canonicalize(tmp.path()).unwrap();
    seed(&base);
    let inside = base.join("src/exiting.rs");
    fs::write(&inside, "pub fn exiting() {}\n").unwrap();

    let (picker, _frecency) = make_watched_picker(&base);
    let events = watch_collect(&picker, "", WatchOptions::default());

    // The destination is outside the indexed tree, so there is no pair to
    // report — this stays a plain removal.
    fs::rename(&inside, outside_root.path().join("exiting.rs")).unwrap();

    assert!(
        wait_for(
            || events
                .lock()
                .iter()
                .any(|e| e.path == inside && e.kind == WatchEventKind::Removed),
            Duration::from_secs(10)
        ),
        "expected Removed for a file moved out of the base, got: {:?}",
        events.lock()
    );
    assert!(
        events
            .lock()
            .iter()
            .all(|e| e.kind != WatchEventKind::Renamed),
        "no Renamed may escape with a path outside the base: {:?}",
        events.lock()
    );
}

#[test]
fn empty_pattern_watches_the_whole_tree() {
    let tmp = TempDir::new().unwrap();
    let base = fff_search::path_utils::canonicalize(tmp.path()).unwrap();
    seed(&base);
    let (picker, _frecency) = make_watched_picker(&base);

    let events = watch_collect(
        &picker,
        "",
        WatchOptions {
            ignore: vec!["*.log".to_string()],
            ..Default::default()
        },
    );

    let rs_file = base.join("src/anywhere.rs");
    let txt_file = base.join("notes.txt");
    let log_file = base.join("noise.log");
    fs::write(&rs_file, "\n").unwrap();
    fs::write(&txt_file, "\n").unwrap();
    fs::write(&log_file, "\n").unwrap();

    assert!(
        wait_for(
            || {
                let got = events.lock();
                got.iter().any(|e| e.path == rs_file) && got.iter().any(|e| e.path == txt_file)
            },
            Duration::from_secs(10)
        ),
        "watch-all did not receive events for both files, got: {:?}",
        events.lock()
    );
    // the ignore option still filters within a watch-all subscription
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !events.lock().iter().any(|e| e.path == log_file),
        "*.log must be filtered by the ignore option"
    );
}

#[test]
fn exact_out_of_tree_paths_are_rejected() {
    let tmp = TempDir::new().unwrap();
    let base = fff_search::path_utils::canonicalize(tmp.path()).unwrap();
    seed(&base);

    let outside = TempDir::new().unwrap();
    let outside_file = fff_search::path_utils::canonicalize(outside.path())
        .unwrap()
        .join("config.txt");
    fs::write(&outside_file, "v1").unwrap();

    let (picker, _frecency) = make_watched_picker(&base);

    assert!(
        picker
            .watch(
                outside_file.to_str().unwrap(),
                WatchOptions::default(),
                |_, _| {}
            )
            .is_err(),
        "exact paths outside the indexed tree must be rejected"
    );
}

#[test]
fn gitignored_files_are_never_delivered() {
    let tmp = TempDir::new().unwrap();
    let base = fff_search::path_utils::canonicalize(tmp.path()).unwrap();
    seed(&base);
    fs::create_dir_all(base.join("dist")).unwrap();
    fs::write(base.join(".gitignore"), "dist/\n*.log\n").unwrap();
    git2::Repository::init(&base).unwrap();
    let (picker, _frecency) = make_watched_picker(&base);

    let events = watch_collect(&picker, "", WatchOptions::default());

    fs::write(base.join("dist/bundle.js"), "js").unwrap();
    fs::write(base.join("noise.log"), "log").unwrap();
    fs::write(base.join("visible.txt"), "txt").unwrap();

    assert!(
        wait_for(
            || events
                .lock()
                .iter()
                .any(|e| e.path == base.join("visible.txt")),
            Duration::from_secs(10)
        ),
        "non-ignored file must be delivered, got {:?}",
        events.lock()
    );

    std::thread::sleep(Duration::from_millis(500));
    let collected = events.lock();
    assert!(
        !collected
            .iter()
            .any(|e| e.path == base.join("dist/bundle.js")),
        "gitignored directory content must not be delivered: {:?}",
        collected
    );
    assert!(
        !collected.iter().any(|e| e.path == base.join("noise.log")),
        "gitignored file must not be delivered: {:?}",
        collected
    );
}

#[test]
fn dir_subscription_with_ignore_option() {
    let tmp = TempDir::new().unwrap();
    let base = fff_search::path_utils::canonicalize(tmp.path()).unwrap();
    seed(&base);
    fs::create_dir_all(base.join("src/vendor")).unwrap();
    let (picker, _frecency) = make_watched_picker(&base);

    // parcel-style: subscribe to a directory subtree with excludes
    let got = watch_collect(
        &picker,
        "src",
        WatchOptions {
            ignore: vec!["*.map".to_string(), "src/vendor".to_string()],
            ..Default::default()
        },
    );

    fs::write(base.join("src/feature.rs"), "pub fn f() {}\n").unwrap();
    fs::write(base.join("src/feature.js.map"), "{}\n").unwrap();
    fs::write(base.join("src/vendor/lib.js"), "x\n").unwrap();
    fs::write(base.join("outside_dir.txt"), "not in src\n").unwrap();

    assert!(
        wait_for(
            || got
                .lock()
                .iter()
                .any(|e| e.path == base.join("src/feature.rs")),
            Duration::from_secs(10)
        ),
        "dir subscriber must see files in its subtree, got {:?}",
        got.lock()
    );
    let got = got.lock();
    assert!(
        !got.iter()
            .any(|e| e.path == base.join("src/feature.js.map")),
        "ignore glob leaked: {got:?}"
    );
    assert!(
        !got.iter().any(|e| e.path == base.join("src/vendor/lib.js")),
        "ignore prefix leaked: {got:?}"
    );
    assert!(
        !got.iter().any(|e| e.path == base.join("outside_dir.txt")),
        "event outside the subscribed dir leaked: {got:?}"
    );
}

#[test]
fn shutdown_watches_stops_future_deliveries() {
    let tmp = TempDir::new().unwrap();
    let base = fff_search::path_utils::canonicalize(tmp.path()).unwrap();
    seed(&base);
    let (picker, _frecency) = make_watched_picker(&base);

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_cb = Arc::clone(&calls);
    picker
        .watch("**/*.txt", WatchOptions::default(), move |_, _| {
            calls_cb.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap();

    fs::write(base.join("one.txt"), "1\n").unwrap();
    assert!(
        wait_for(|| calls.load(Ordering::SeqCst) > 0, Duration::from_secs(10)),
        "callback never fired before shutdown"
    );

    picker.shutdown_watches();
    let after = calls.load(Ordering::SeqCst);

    fs::write(base.join("two.txt"), "2\n").unwrap();
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        after,
        "callback fired after shutdown_watches returned"
    );
}

#[test]
fn non_canonical_dir_pattern_resolves_into_the_tree() {
    let tmp = TempDir::new().unwrap();
    let base = fff_search::path_utils::canonicalize(tmp.path()).unwrap();
    seed(&base);
    let (picker, _) = make_watched_picker(&base);

    // tmp.path() is the non-canonical spelling (e.g. /var/... symlinked to
    // /private/var/... on macOS, 8.3 short names on Windows); the watch must
    // canonicalize instead of rejecting it
    let events = watch_collect(
        &picker,
        tmp.path().to_str().unwrap(),
        WatchOptions::default(),
    );

    fs::write(base.join("via-alias.txt"), "x\n").unwrap();
    assert!(
        wait_for(
            || events
                .lock()
                .iter()
                .any(|e| e.path == base.join("via-alias.txt")),
            Duration::from_secs(10)
        ),
        "non-canonical base-dir pattern must receive events, got {:?}",
        events.lock()
    );
}

#[test]
fn invalid_patterns_are_rejected() {
    let tmp = TempDir::new().unwrap();
    let base = fff_search::path_utils::canonicalize(tmp.path()).unwrap();
    seed(&base);
    let (picker, _frecency) = make_watched_picker(&base);

    assert!(
        picker
            .watch(
                "/somewhere/else/**/*.rs",
                WatchOptions::default(),
                |_, _| {}
            )
            .is_err(),
        "absolute glob outside base must be rejected"
    );

    // relative exact path resolves against base
    let got = watch_collect(&picker, "README.md", WatchOptions::default());
    fs::write(base.join("README.md"), "# updated\n").unwrap();
    assert!(
        wait_for(
            || got.lock().iter().any(|e| e.path == base.join("README.md")),
            Duration::from_secs(10)
        ),
        "got {:?}",
        got.lock()
    );
}
