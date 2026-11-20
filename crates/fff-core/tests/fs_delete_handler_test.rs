//! Reproducer: macOS FSEvents does not deliver Remove events for files
//! deleted from NonRecursive-watched directories when multiple directories
//! are watched via stop/restart cycles.
//!
//! This test watches a temp directory NonRecursively, creates a file,
//! verifies the Create event, deletes the file, and checks whether a
//! Remove (or any) event is delivered.

use notify::event::*;
use notify::{Config, EventKindMask, RecommendedWatcher, RecursiveMode, Watcher};
use std::fs;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

fn setup_temp_git_repo() -> (PathBuf, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().canonicalize().unwrap();

    // Create a git repo like the bun test does
    std::process::Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(&dir)
        .output()
        .unwrap();

    fs::write(dir.join("hello.txt"), "hello\n").unwrap();
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();

    std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(&dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(&dir)
        .output()
        .unwrap();

    (dir, tmp)
}

/// Raw notify watcher: single NonRecursive watch on a directory.
/// Create a file, delete it, check if Remove event is delivered.
#[test]
fn raw_notify_nonrecursive_detects_deletion() {
    let (dir, _tmp) = setup_temp_git_repo();
    let (tx, rx) = mpsc::channel();

    let config = Config::default()
        .with_follow_symlinks(false)
        .with_event_kinds(EventKindMask::CORE);

    let mut watcher = RecommendedWatcher::new(
        move |res: notify::Result<Event>| {
            if let Ok(ev) = res {
                let _ = tx.send(ev);
            }
        },
        config,
    )
    .unwrap();

    // Watch ONLY the root dir NonRecursively (like fff does)
    watcher
        .watch(dir.as_path(), RecursiveMode::NonRecursive)
        .unwrap();

    // Let the watcher stabilize
    std::thread::sleep(Duration::from_millis(500));

    // Drain any startup events
    while rx.try_recv().is_ok() {}

    // Create a file
    let file_path = dir.join("testfile.txt");
    fs::write(&file_path, "content\n").unwrap();

    // Wait for create event
    let mut got_create = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(ev) => {
                eprintln!("  [create phase] event: {:?} paths={:?}", ev.kind, ev.paths);
                if matches!(ev.kind, EventKind::Create(_)) && ev.paths.contains(&file_path) {
                    got_create = true;
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => break,
        }
    }
    assert!(got_create, "Expected Create event for testfile.txt");

    // Drain remaining events from the create
    std::thread::sleep(Duration::from_millis(300));
    while rx.try_recv().is_ok() {}

    // Delete the file
    fs::remove_file(&file_path).unwrap();
    eprintln!("  File deleted: {}", file_path.display());

    // Wait for any event related to the deletion
    let mut got_removal_event = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(ev) => {
                eprintln!("  [delete phase] event: {:?} paths={:?}", ev.kind, ev.paths);
                if ev.paths.contains(&file_path) {
                    got_removal_event = true;
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => break,
        }
    }

    assert!(
        got_removal_event,
        "Expected some event for deleted testfile.txt but got none within 5s"
    );
}

/// Same test but with MULTIPLE NonRecursive watches (base + src + .git)
/// to match what fff actually does. Each watch() call stops/restarts the FSEvents stream.
#[test]
fn raw_notify_multi_nonrecursive_detects_deletion() {
    let (dir, _tmp) = setup_temp_git_repo();
    let (tx, rx) = mpsc::channel();

    let config = Config::default()
        .with_follow_symlinks(false)
        .with_event_kinds(EventKindMask::CORE);

    let mut watcher = RecommendedWatcher::new(
        move |res: notify::Result<Event>| {
            if let Ok(ev) = res {
                let _ = tx.send(ev);
            }
        },
        config,
    )
    .unwrap();

    // Watch multiple directories NonRecursively — EACH call restarts the FSEvents stream
    watcher
        .watch(dir.as_path(), RecursiveMode::NonRecursive)
        .unwrap();
    watcher
        .watch(&dir.join("src"), RecursiveMode::NonRecursive)
        .unwrap();
    watcher
        .watch(&dir.join(".git"), RecursiveMode::NonRecursive)
        .unwrap();

    // Let the watcher stabilize
    std::thread::sleep(Duration::from_millis(500));
    while rx.try_recv().is_ok() {}

    // Create a file in root dir
    let file_path = dir.join("testfile.txt");
    fs::write(&file_path, "content\n").unwrap();

    // Wait for create event
    let mut got_create = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(ev) => {
                eprintln!("  [multi-create] event: {:?} paths={:?}", ev.kind, ev.paths);
                if matches!(ev.kind, EventKind::Create(_)) && ev.paths.contains(&file_path) {
                    got_create = true;
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => break,
        }
    }
    assert!(
        got_create,
        "Expected Create event for testfile.txt with multi-watch"
    );

    // Drain
    std::thread::sleep(Duration::from_millis(300));
    while rx.try_recv().is_ok() {}

    // Delete the file
    fs::remove_file(&file_path).unwrap();
    eprintln!("  File deleted: {}", file_path.display());

    // Wait for any event related to the deletion
    let mut got_removal_event = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(ev) => {
                eprintln!("  [multi-delete] event: {:?} paths={:?}", ev.kind, ev.paths);
                if ev.paths.contains(&file_path) {
                    got_removal_event = true;
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => break,
        }
    }

    assert!(
        got_removal_event,
        "Expected some event for deleted testfile.txt with multi-watch but got none within 5s"
    );
}

/// Test with debouncer (matching exactly what fff uses)
#[test]
fn debounced_nonrecursive_detects_deletion() {
    use notify_debouncer_full::{DebounceEventResult, NoCache, new_debouncer_opt};

    let (dir, _tmp) = setup_temp_git_repo();
    let (tx, rx) = mpsc::channel();

    let config = Config::default()
        .with_follow_symlinks(false)
        .with_event_kinds(EventKindMask::CORE);

    let mut debouncer: notify_debouncer_full::Debouncer<RecommendedWatcher, NoCache> =
        new_debouncer_opt(
            Duration::from_millis(250),
            Some(Duration::from_millis(125)),
            move |result: DebounceEventResult| {
                if let Ok(events) = result {
                    for ev in events {
                        eprintln!(
                            "  [debounced-cb] kind={:?} paths={:?}",
                            ev.event.kind, ev.event.paths
                        );
                        let _ = tx.send(ev);
                    }
                }
            },
            NoCache::new(),
            config,
        )
        .unwrap();

    // Watch like fff does
    debouncer
        .watch(dir.as_path(), RecursiveMode::NonRecursive)
        .unwrap();
    debouncer
        .watch(dir.join("src"), RecursiveMode::NonRecursive)
        .unwrap();
    debouncer
        .watch(dir.join(".git"), RecursiveMode::NonRecursive)
        .unwrap();

    // Longer stabilization — each watch() restarts the FSEvents stream
    std::thread::sleep(Duration::from_secs(1));
    while rx.try_recv().is_ok() {}

    // Create file
    let file_path = dir.join("testfile.txt");
    fs::write(&file_path, "content\n").unwrap();

    let mut got_create = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(ev) => {
                if ev.event.paths.contains(&file_path) {
                    got_create = true;
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => break,
        }
    }
    assert!(got_create, "Expected Create event via debouncer");

    // Wait for debounce to fully flush
    std::thread::sleep(Duration::from_millis(500));
    while rx.try_recv().is_ok() {}

    // Delete
    fs::remove_file(&file_path).unwrap();
    eprintln!("  File deleted: {}", file_path.display());

    // Wait for ANY event for this path
    let mut got_event = false;
    let mut event_kind = String::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(ev) => {
                if ev.event.paths.contains(&file_path) {
                    event_kind = format!("{:?}", ev.event.kind);
                    got_event = true;
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => break,
        }
    }

    assert!(
        got_event,
        "Expected some event for deleted testfile.txt via debouncer but got none within 5s"
    );
    eprintln!("  Got event kind: {}", event_kind);
}
