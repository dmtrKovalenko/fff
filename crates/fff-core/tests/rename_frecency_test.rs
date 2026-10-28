use std::path::{Path, PathBuf};

use fff_search::file_picker::FFFMode;
use fff_search::frecency::FrecencyTracker;
use tempfile::TempDir;

fn tracker() -> (FrecencyTracker, TempDir) {
    let dir = TempDir::new().expect("mktemp frecency db");
    let tracker = FrecencyTracker::open(dir.path().join("frecency.mdb")).expect("open frecency db");
    (tracker, dir)
}

fn score(tracker: &FrecencyTracker, path: &Path) -> i64 {
    tracker.get_access_score(path, FFFMode::Neovim)
}

#[test]
fn copy_history_preserves_source_entry() {
    let (tracker, _dir) = tracker();
    let old = PathBuf::from("/w/src/old.rs");
    let new = PathBuf::from("/w/src/new.rs");

    for _ in 0..3 {
        tracker.track_access(&old).expect("track access");
    }
    let before = score(&tracker, &old);
    assert!(before > 0, "precondition: source must have a score");
    assert_eq!(score(&tracker, &new), 0, "precondition: target is unknown");

    assert!(tracker.copy_history(&old, &new).expect("copy history"));

    assert_eq!(
        score(&tracker, &new),
        before,
        "destination inherits the full history"
    );
    // The whole point of copying rather than moving: checking out a revision
    // where the old path still exists must not have lost anything.
    assert_eq!(
        score(&tracker, &old),
        before,
        "source history must survive the rename"
    );
    assert_eq!(tracker.access_count(&old).unwrap(), 3);
    assert_eq!(tracker.access_count(&new).unwrap(), 3);
}

#[test]
fn copy_history_overwrites_an_existing_target() {
    let (tracker, _dir) = tracker();
    let old = PathBuf::from("/w/src/old.rs");
    let new = PathBuf::from("/w/src/new.rs");

    for _ in 0..2 {
        tracker.track_access(&old).expect("track access");
    }
    for _ in 0..5 {
        tracker.track_access(&new).expect("track access");
    }

    assert!(tracker.copy_history(&old, &new).expect("copy history"));

    assert_eq!(
        tracker.access_count(&new).unwrap(),
        2,
        "the destination takes the source's history as is"
    );
    assert_eq!(
        tracker.access_count(&old).unwrap(),
        2,
        "source is untouched"
    );
}

#[test]
fn copy_history_is_noop_for_unknown_source() {
    let (tracker, _dir) = tracker();
    let old = PathBuf::from("/w/src/never-seen.rs");
    let new = PathBuf::from("/w/src/new.rs");

    assert!(!tracker.copy_history(&old, &new).expect("copy history"));
    assert_eq!(
        tracker.access_count(&new).unwrap(),
        0,
        "no empty entry may be written for the destination"
    );
}

#[test]
fn copy_history_is_noop_for_same_path() {
    let (tracker, _dir) = tracker();
    let path = PathBuf::from("/w/src/file.rs");
    tracker.track_access(&path).expect("track access");

    assert!(!tracker.copy_history(&path, &path).expect("copy history"));
    assert_eq!(tracker.access_count(&path).unwrap(), 1);
}
