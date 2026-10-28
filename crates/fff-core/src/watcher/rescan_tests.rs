use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use notify::Event;
use notify::EventKind;
use notify::event::{
    AccessKind, AccessMode, CreateKind, DataChange, Flag, ModifyKind, RemoveKind, RenameMode,
};
use notify_debouncer_full::DebouncedEvent;
use tempfile::TempDir;

use super::handle_debounced_events;
use crate::constants::MAX_OVERFLOW_FILES;
use crate::file_picker::{FFFMode, FilePicker, FilePickerOptions};
use crate::frecency::FrecencyTracker;
use crate::git_status_worker::GitStatusWorker;
use crate::rescan_stats::{RescanReason, RescanStats};
use crate::shared::{SharedFilePicker, SharedFrecency};
use crate::watch::{WatchEvent, WatchEventKind, WatchOptions};
use std::sync::mpsc;

#[test]
fn saving_an_indexed_file_stays_incremental() {
    let f = Fixture::new();
    f.write("src/main.rs", "fn main() {}");
    f.index();

    f.write("src/main.rs", "fn main() { println!(); }");
    let delta = f.feed([modify(f.path("src/main.rs"))]);

    f.assert_no_rescan(&delta, "saving a tracked file");
}

#[test]
fn editor_atomic_save_stays_incremental() {
    let f = Fixture::new();
    f.write("src/main.rs", "fn main() {}");
    f.index();

    // write-to-temp + rename-over-target, the way vim/VSCode/IntelliJ save.
    f.write("src/main.rs", "fn main() { println!(); }");
    let target = f.path("src/main.rs");
    let temp = f.path("src/.main.rs.swp");
    let delta = f.feed([
        DebouncedEvent::new(
            Event::new(EventKind::Create(CreateKind::File)).add_path(temp.clone()),
            Instant::now(),
        ),
        DebouncedEvent::new(
            Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::From)))
                .add_path(temp.clone()),
            Instant::now(),
        ),
        DebouncedEvent::new(
            Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::To)))
                .add_path(target.clone()),
            Instant::now(),
        ),
        DebouncedEvent::new(
            Event::new(EventKind::Remove(RemoveKind::File)).add_path(temp),
            Instant::now(),
        ),
    ]);

    f.assert_no_rescan(&delta, "an atomic editor save");
    assert!(f.is_indexed("src/main.rs"), "target must stay indexed");
}

#[test]
fn creating_and_deleting_files_stays_incremental() {
    let f = Fixture::new();
    f.write("src/main.rs", "fn main() {}");
    f.index();

    f.write("src/added.rs", "pub fn added() {}");
    let created = f.feed([create(f.path("src/added.rs"))]);
    f.assert_no_rescan(&created, "creating a file");
    assert!(f.is_indexed("src/added.rs"));

    f.remove("src/added.rs");
    let removed = f.feed([remove_file(f.path("src/added.rs"))]);
    f.assert_no_rescan(&removed, "deleting a file");
    assert!(!f.is_indexed("src/added.rs"));
}

#[test]
fn deleting_a_directory_stays_incremental() {
    let f = Fixture::new();
    f.write("src/main.rs", "fn main() {}");
    f.write("src/nested/a.rs", "");
    f.write("src/nested/b.rs", "");
    f.index();

    std::fs::remove_dir_all(f.path("src/nested")).unwrap();
    let delta = f.feed([DebouncedEvent::new(
        Event::new(EventKind::Remove(RemoveKind::Folder)).add_path(f.path("src/nested")),
        Instant::now(),
    )]);

    f.assert_no_rescan(&delta, "deleting a directory");
    assert!(!f.is_indexed("src/nested/a.rs"));
    assert!(f.is_indexed("src/main.rs"));
}

#[test]
fn read_only_access_events_are_ignored() {
    let f = Fixture::new();
    f.write("src/main.rs", "fn main() {}");
    f.index();

    // fff's own preview + grep reads generate these; reacting to them would
    // make the picker rescan whenever the user scrolls the result list.
    let path = f.path("src/main.rs");
    let delta = f.feed([
        DebouncedEvent::new(
            Event::new(EventKind::Access(AccessKind::Read)).add_path(path.clone()),
            Instant::now(),
        ),
        DebouncedEvent::new(
            Event::new(EventKind::Access(AccessKind::Open(AccessMode::Read)))
                .add_path(path.clone()),
            Instant::now(),
        ),
        DebouncedEvent::new(
            Event::new(EventKind::Access(AccessKind::Close(AccessMode::Read))).add_path(path),
            Instant::now(),
        ),
    ]);

    f.assert_no_rescan(&delta, "read-only access events");
}

#[test]
fn recreating_the_same_paths_does_not_consume_overflow_capacity() {
    let f = Fixture::new();
    f.write("src/main.rs", "fn main() {}");
    f.index();

    // Recreated paths must reuse their overflow slots.
    for _ in 0..8 {
        for i in 0..200 {
            let rel = format!("gen/out{i}.rs");
            f.write(&rel, "generated");
            f.feed([create(f.path(&rel))]);
        }
        for i in 0..200 {
            let rel = format!("gen/out{i}.rs");
            f.remove(&rel);
            f.feed([remove_file(f.path(&rel))]);
        }
    }

    let delta = f.all_rescans();
    f.assert_no_rescan(&delta, "1600 create/delete cycles over 200 stable paths");
    assert!(
        f.overflow_len() <= 200,
        "each path must claim one overflow slot at most, got {}",
        f.overflow_len()
    );
}

#[test]
fn writes_inside_a_gitignored_directory_stay_incremental() {
    let f = Fixture::with_git();
    f.write(".gitignore", "target/\nnode_modules/\n");
    f.write("src/main.rs", "fn main() {}");
    f.index();

    let mut events = Vec::new();
    for i in 0..64 {
        let rel = format!("target/debug/artifact{i}.o");
        f.write(&rel, "binary");
        events.push(create(f.path(&rel)));
    }

    let delta = f.feed(events);
    f.assert_no_rescan(&delta, "build output written into an ignored directory");
}

#[test]
fn ignored_event_batch_above_index_capacity_stays_incremental() {
    let f = Fixture::with_git();
    f.write(".gitignore", "node_modules/\n");
    f.write("src/main.rs", "fn main() {}");
    f.index();

    let events = (0..MAX_OVERFLOW_FILES + 1)
        .map(|i| {
            let rel = format!("node_modules/pkg/file{i}.js");
            f.write(&rel, "");
            create(f.path(&rel))
        })
        .collect::<Vec<_>>();

    let delta = f.feed(events);
    f.assert_no_rescan(&delta, "ignored events above the index capacity");
    assert_eq!(f.overflow_len(), 0);
}

#[test]
fn repeated_edits_above_index_capacity_stay_incremental() {
    let f = Fixture::new();
    f.write("src/main.rs", "fn main() {}");
    f.index();

    let path = f.path("src/main.rs");
    let events = (0..MAX_OVERFLOW_FILES + 1)
        .map(|_| modify(path.clone()))
        .collect::<Vec<_>>();

    let delta = f.feed(events);
    f.assert_no_rescan(&delta, "repeated edits above the index capacity");
    assert_eq!(f.overflow_len(), 0);
}

#[test]
fn ignore_file_inside_an_ignored_directory_stays_incremental() {
    let f = Fixture::with_git();
    f.write(".gitignore", "node_modules/\n");
    f.write("src/main.rs", "fn main() {}");
    f.index();

    let ignore_files =
        ["left-pad", "lodash", "typescript"].map(|pkg| format!("node_modules/{pkg}/.gitignore"));

    for rel in &ignore_files {
        f.write(rel, "dist\n");
    }
    let delta = f.feed(ignore_files.iter().map(|rel| create(f.path(rel))));
    f.assert_no_rescan(&delta, "creating ignored .gitignore files");

    for rel in &ignore_files {
        f.write(rel, "build\n");
    }
    let delta = f.feed(ignore_files.iter().map(|rel| modify(f.path(rel))));
    f.assert_no_rescan(&delta, "modifying ignored .gitignore files");

    for rel in &ignore_files {
        f.remove(rel);
    }
    let delta = f.feed(ignore_files.iter().map(|rel| remove_file(f.path(rel))));
    f.assert_no_rescan(&delta, "removing ignored .gitignore files");
}

#[test]
fn ignore_file_inside_an_indexed_directory_triggers_a_rescan() {
    let f = Fixture::with_git();
    f.write("src/.gitignore", ".gitignore\ngenerated/\n");
    f.write("src/main.rs", "fn main() {}");
    f.index();

    f.write("src/.gitignore", ".gitignore\ngenerated/\nbuild/\n");
    let delta = f.feed([modify(f.path("src/.gitignore"))]);

    assert_eq!(delta.count(RescanReason::IgnoreFileChanged), 1);
}

#[test]
fn git_internal_churn_stays_incremental() {
    let f = Fixture::with_git();
    f.write("src/main.rs", "fn main() {}");
    f.index();

    let git_dir = f.path(".git");
    let delta = f.feed([
        create(git_dir.join("index.lock")),
        modify(git_dir.join("index")),
        remove_file(git_dir.join("index.lock")),
        modify(git_dir.join("HEAD")),
        modify(git_dir.join("logs/HEAD")),
        modify(git_dir.join("COMMIT_EDITMSG")),
        modify(git_dir.join("refs/heads/main")),
    ]);

    f.assert_no_rescan(&delta, "git writing its own metadata");
}

#[test]
fn changing_the_root_ignore_file_triggers_a_rescan() {
    let f = Fixture::with_git();
    f.write(".gitignore", "target/\n");
    f.write("src/main.rs", "fn main() {}");
    f.index();

    f.write(".gitignore", "target/\nsrc/\n");
    let delta = f.feed([modify(f.path(".gitignore"))]);

    assert_eq!(
        delta.count(RescanReason::IgnoreFileChanged),
        1,
        "the indexed set depends on the root ignore rules, got {delta}"
    );
}

#[test]
fn kernel_event_loss_on_a_directory_triggers_a_rescan() {
    let f = Fixture::new();
    f.write("src/main.rs", "fn main() {}");
    f.index();

    let delta = f.feed([DebouncedEvent::new(
        Event::new(EventKind::Modify(ModifyKind::Any))
            .add_path(f.path("src"))
            .set_flag(Flag::Rescan),
        Instant::now(),
    )]);

    assert_eq!(
        delta.count(RescanReason::KernelEventLoss),
        1,
        "a dropped-events flag over a directory means unknown subtree state, got {delta}"
    );
}

#[test]
fn new_files_above_index_capacity_trigger_a_rescan() {
    let f = Fixture::new();
    f.write("src/main.rs", "fn main() {}");
    f.index();

    let events = (0..MAX_OVERFLOW_FILES + 1)
        .map(|i| {
            let rel = format!("src/bulk{i}.rs");
            f.write(&rel, "");
            create(f.path(&rel))
        })
        .collect::<Vec<_>>();

    let delta = f.feed(events);
    assert_eq!(
        delta.count(RescanReason::IndexUpdateRejected),
        1,
        "new files above the overflow region cannot be applied incrementally, got {delta}"
    );
}

#[test]
fn batch_at_the_overflow_boundary_stays_incremental() {
    let f = Fixture::new();
    f.write("src/main.rs", "fn main() {}");
    f.index();

    let events = (0..MAX_OVERFLOW_FILES)
        .map(|i| {
            let rel = format!("src/bulk{i}.rs");
            f.write(&rel, "");
            create(f.path(&rel))
        })
        .collect::<Vec<_>>();

    let delta = f.feed(events);
    f.assert_no_rescan(&delta, "a batch exactly at the overflow limit");
}

#[test]
fn event_batch_at_four_times_index_capacity_stays_incremental() {
    let f = Fixture::new();
    f.write("src/main.rs", "fn main() {}");
    f.index();

    let path = f.path("src/main.rs");
    let events = (0..MAX_OVERFLOW_FILES * 4)
        .map(|_| modify(path.clone()))
        .collect::<Vec<_>>();

    let delta = f.feed(events);
    f.assert_no_rescan(&delta, "an event batch exactly at the event limit");
}

#[test]
fn event_batch_above_four_times_index_capacity_triggers_a_rescan() {
    let f = Fixture::new();
    f.write("src/main.rs", "fn main() {}");
    f.index();

    let path = f.path("src/main.rs");
    let events = (0..MAX_OVERFLOW_FILES * 4 + 1)
        .map(|_| modify(path.clone()))
        .collect::<Vec<_>>();

    let delta = f.feed(events);
    assert_eq!(
        delta.count(RescanReason::EventBatchOverflow),
        1,
        "an event batch above four times the index capacity must rescan, got {delta}"
    );
}

#[test]
fn repeated_triggers_inside_the_cooldown_collapse_to_one_rescan() {
    let f = Fixture::with_git();
    f.write(".gitignore", "target/\n");
    f.write("src/main.rs", "fn main() {}");
    f.index();

    // Repeated batches during the cooldown must share one walk.
    for round in 0..50 {
        f.write(".gitignore", &format!("target/\n# round {round}\n"));
        f.feed([modify(f.path(".gitignore"))]);
    }

    let stats = f.all_rescans();
    assert_eq!(
        stats.total, 1,
        "50 triggers inside the cooldown must collapse to a single walk, got {stats}"
    );
    assert_eq!(
        stats.throttled, 49,
        "every suppressed request must be accounted for, got {stats}"
    );
}

#[test]
fn an_explicit_request_is_never_throttled() {
    let f = Fixture::with_git();
    f.write(".gitignore", "target/\n");
    f.write("src/main.rs", "fn main() {}");
    f.index();

    // Burn the cooldown with a watcher trigger, then confirm a user-initiated
    // refresh still goes through.
    f.write(".gitignore", "target/\nsrc/\n");
    f.feed([modify(f.path(".gitignore"))]);

    for _ in 0..3 {
        f.picker.trigger_full_rescan_async(&f.frecency).unwrap();
    }

    let stats = f.all_rescans();
    assert_eq!(
        stats.count(RescanReason::Explicit),
        3,
        "explicit refreshes must bypass the throttle, got {stats}"
    );
    assert_eq!(stats.count_throttled(RescanReason::Explicit), 0);
}

#[test]
fn events_after_a_suppressed_kernel_rescan_are_still_applied() {
    let f = Fixture::new();
    f.write("src/main.rs", "fn main() {}");
    f.index();

    f.write("src/added.rs", "pub fn added() {}");
    let delta = f.feed([
        DebouncedEvent::new(
            Event::new(EventKind::Modify(ModifyKind::Data(DataChange::Content)))
                .add_path(f.path("src/main.rs"))
                .set_flag(Flag::Rescan),
            Instant::now(),
        ),
        create(f.path("src/added.rs")),
    ]);

    f.assert_no_rescan(&delta, "a dropped-events flag over a single tracked file");
    assert!(
        f.is_indexed("src/added.rs"),
        "suppressing the rescan must not drop the rest of the batch"
    );
}

#[test]
fn a_throttled_ignore_file_event_is_still_applied_incrementally() {
    let f = Fixture::with_git();
    f.write(".gitignore", "target/\n");
    f.write("src/main.rs", "fn main() {}");
    f.index();

    // Burn the cooldown: deleting .gitignore admits a full rescan.
    f.remove(".gitignore");
    let delta = f.feed([remove_file(f.path(".gitignore"))]);
    assert_eq!(delta.count(RescanReason::IgnoreFileChanged), 1);
    f.picker.wait_for_indexing_complete(Duration::from_secs(10));

    // Recreating it inside the cooldown throttles the rescan, but the file
    // itself must re-enter the index via the incremental fallback.
    f.write(".gitignore", "target/\n__ignored_x/\n");
    let delta = f.feed([create(f.path(".gitignore"))]);
    assert_eq!(delta.total, 0, "the rescan must be throttled, got {delta}");
    assert_eq!(delta.count_throttled(RescanReason::IgnoreFileChanged), 1);
    assert!(
        f.is_indexed(".gitignore"),
        "a throttled ignore-file event must still index the file itself"
    );
}

#[test]
fn renaming_a_file_carries_frecency_and_keeps_the_old_entry() {
    let f = Fixture::with_frecency();
    f.write("src/old.rs", "fn old() {}");
    f.index();

    for _ in 0..3 {
        f.track_access("src/old.rs");
    }
    let before = f.access_score("src/old.rs");
    assert!(before > 0, "precondition: source must be ranked");

    // A real rename so size and mtime are genuinely preserved.
    std::fs::rename(f.path("src/old.rs"), f.path("src/new.rs")).unwrap();
    let delta = f.feed([
        remove_file(f.path("src/old.rs")),
        create(f.path("src/new.rs")),
    ]);

    f.assert_no_rescan(&delta, "a rename");
    assert!(f.is_indexed("src/new.rs"), "destination must be indexed");
    assert_eq!(
        f.access_score("src/new.rs"),
        before,
        "destination inherits the history"
    );
    assert_eq!(
        f.access_score("src/old.rs"),
        before,
        "source keeps its history so old revisions still rank"
    );
    assert!(
        f.indexed_access_score("src/new.rs") > 0,
        "in-memory score must be refreshed, not left at zero until the next git pass"
    );
}

#[test]
fn renaming_a_file_emits_a_single_renamed_event() {
    let f = Fixture::new();
    f.write("src/old.rs", "fn old() {}");
    f.index();
    let events = f.subscribe("**");

    std::fs::rename(f.path("src/old.rs"), f.path("src/new.rs")).unwrap();
    f.feed([
        remove_file(f.path("src/old.rs")),
        create(f.path("src/new.rs")),
    ]);

    let received = events.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(received.len(), 1, "expected one event, got {received:?}");
    assert_eq!(received[0].kind, WatchEventKind::Renamed);
    assert_eq!(received[0].path, f.path("src/new.rs"));
    assert_eq!(
        received[0].from.as_deref(),
        Some(f.path("src/old.rs").as_path())
    );
}

#[test]
fn paired_rename_events_are_trusted_over_the_heuristic() {
    let f = Fixture::new();
    f.write("src/old.rs", "fn old() {}");
    f.index();
    let events = f.subscribe("**");

    std::fs::rename(f.path("src/old.rs"), f.path("src/new.rs")).unwrap();
    // Linux/inotify pairs the halves for us and hands over both paths at once.
    f.feed([DebouncedEvent::new(
        Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::Both)))
            .add_path(f.path("src/old.rs"))
            .add_path(f.path("src/new.rs")),
        Instant::now(),
    )]);

    let received = events.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(received.len(), 1, "expected one event, got {received:?}");
    assert_eq!(received[0].kind, WatchEventKind::Renamed);
    assert_eq!(received[0].path, f.path("src/new.rs"));
    assert_eq!(
        received[0].from.as_deref(),
        Some(f.path("src/old.rs").as_path())
    );
}

#[test]
fn editing_an_indexed_file_is_never_mistaken_for_a_rename() {
    let f = Fixture::new();
    f.write("src/keep.rs", "same size!!");
    f.write("src/gone.rs", "same size!!");
    f.index();
    let events = f.subscribe("**");

    // Both files share a size; copying the mtime across makes their identities
    // collide. `keep.rs` is still indexed, so it is an edit, not a destination.
    let mtime = std::fs::metadata(f.path("src/gone.rs"))
        .unwrap()
        .modified()
        .unwrap();
    std::fs::remove_file(f.path("src/gone.rs")).unwrap();
    std::fs::File::open(f.path("src/keep.rs"))
        .unwrap()
        .set_modified(mtime)
        .unwrap();

    f.feed([
        remove_file(f.path("src/gone.rs")),
        modify(f.path("src/keep.rs")),
    ]);

    let received = events.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(
        received.iter().all(|e| e.kind != WatchEventKind::Renamed),
        "an edit to a live file must not be reported as a rename: {received:?}"
    );
}

#[test]
fn unrelated_delete_and_create_never_share_history() {
    let f = Fixture::with_frecency();
    f.write("src/tracked.rs", "fn tracked() {}");
    f.index();

    for _ in 0..3 {
        f.track_access("src/tracked.rs");
    }
    assert!(f.access_score("src/tracked.rs") > 0);

    f.remove("src/tracked.rs");
    f.write(
        "src/unrelated.rs",
        "fn unrelated() { /* different size */ }",
    );
    f.feed([
        remove_file(f.path("src/tracked.rs")),
        create(f.path("src/unrelated.rs")),
    ]);

    assert_eq!(
        f.access_score("src/unrelated.rs"),
        0,
        "an unrelated file must not inherit anyone's history"
    );
}

#[test]
fn ambiguous_batch_falls_back_to_remove_and_create() {
    let f = Fixture::new();
    f.write("src/a.rs", "same");
    f.write("src/b.rs", "same");
    f.index();
    let events = f.subscribe("**");

    // Two identical files renamed at once: nothing can be paired safely.
    std::fs::rename(f.path("src/a.rs"), f.path("src/x.rs")).unwrap();
    std::fs::rename(f.path("src/b.rs"), f.path("src/y.rs")).unwrap();
    f.feed([
        remove_file(f.path("src/a.rs")),
        remove_file(f.path("src/b.rs")),
        create(f.path("src/x.rs")),
        create(f.path("src/y.rs")),
    ]);

    let received = events.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(
        received.iter().all(|e| e.kind != WatchEventKind::Renamed),
        "ambiguous identities must not be guessed: {received:?}"
    );
    assert_eq!(received.len(), 4, "{received:?}");
}

struct Fixture {
    base: PathBuf,
    picker: SharedFilePicker,
    frecency: SharedFrecency,
    git_workdir: Option<PathBuf>,
    git_worker: Arc<GitStatusWorker>,
    // Dropped last so background work started by a triggered rescan still
    // sees the tree it was asked to walk.
    _tmp: TempDir,
    _frecency_tmp: Option<TempDir>,
}

impl Fixture {
    fn new() -> Self {
        Self::build(false, false)
    }

    fn with_git() -> Self {
        Self::build(true, false)
    }

    // `SharedFrecency::noop()` silently drops `init`, so any test asserting on
    // scores needs a real LMDB-backed tracker instead.
    fn with_frecency() -> Self {
        Self::build(false, true)
    }

    fn build(git: bool, frecency: bool) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let base = crate::path_utils::canonicalize(tmp.path()).unwrap();
        let git_workdir = git.then(|| {
            let status = Command::new("git")
                .args(["init", "-b", "main"])
                .current_dir(&base)
                .output()
                .expect("git init");
            assert!(status.status.success(), "git init failed");
            base.clone()
        });

        let (shared_frecency, frecency_tmp) = if frecency {
            let db = tempfile::tempdir().unwrap();
            let shared = SharedFrecency::default();
            shared
                .init(FrecencyTracker::open(db.path().join("frecency.mdb")).expect("open frecency"))
                .expect("init frecency");
            (shared, Some(db))
        } else {
            (SharedFrecency::noop(), None)
        };

        Self {
            base,
            picker: SharedFilePicker::default(),
            frecency: shared_frecency,
            git_workdir,
            git_worker: GitStatusWorker::new(),
            _tmp: tmp,
            _frecency_tmp: frecency_tmp,
        }
    }

    fn track_access(&self, rel: &str) {
        let guard = self.frecency.read().unwrap();
        guard
            .as_ref()
            .expect("fixture built without frecency")
            .track_access(&self.path(rel))
            .expect("track access");
    }

    fn access_score(&self, rel: &str) -> i64 {
        let guard = self.frecency.read().unwrap();
        guard
            .as_ref()
            .expect("fixture built without frecency")
            .get_access_score(&self.path(rel), FFFMode::Neovim)
    }

    fn indexed_access_score(&self, rel: &str) -> i16 {
        let guard = self.picker.read().unwrap();
        guard
            .as_ref()
            .and_then(|p| p.get_file_by_path(self.path(rel)))
            .map(|file| file.access_frecency_score)
            .unwrap_or(0)
    }

    fn subscribe(&self, pattern: &str) -> mpsc::Receiver<Vec<WatchEvent>> {
        let (sender, receiver) = mpsc::channel();
        self.picker
            .watch_registry()
            .subscribe(
                &self.base,
                pattern,
                WatchOptions::default(),
                Box::new(move |_, events| {
                    let _ = sender.send(events.to_vec());
                }),
            )
            .expect("subscribe");
        receiver
    }

    fn index(&self) {
        let mut picker = FilePicker::new(FilePickerOptions {
            base_path: self.base.to_string_lossy().into_owned(),
            watch: false,
            ..Default::default()
        })
        .unwrap();
        picker.collect_files().unwrap();
        self.picker.rebase_watches(&self.base);
        *self.picker.write().unwrap() = Some(picker);
    }

    fn feed(&self, events: impl IntoIterator<Item = DebouncedEvent>) -> RescanStats {
        let before = self.picker.rescan_stats();
        handle_debounced_events(
            FFFMode::Neovim,
            events.into_iter().collect(),
            &self.base,
            &self.git_workdir,
            &self.picker,
            &self.frecency,
            &self.git_worker,
        );

        self.picker.rescan_stats().since(&before)
    }

    fn assert_no_rescan(&self, delta: &RescanStats, what: &str) {
        assert_eq!(delta.total, 0, "{what} must not trigger a rescan: {delta}");
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.base.join(rel)
    }

    fn write(&self, rel: &str, contents: &str) {
        let path = self.path(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    fn remove(&self, rel: &str) {
        std::fs::remove_file(self.path(rel)).unwrap();
    }

    fn is_indexed(&self, rel: &str) -> bool {
        let guard = self.picker.read().unwrap();
        guard
            .as_ref()
            .and_then(|p| p.get_file_by_path(self.path(rel)))
            .is_some_and(|file| !file.is_deleted())
    }

    fn all_rescans(&self) -> RescanStats {
        self.picker.rescan_stats()
    }

    fn overflow_len(&self) -> usize {
        let guard = self.picker.read().unwrap();
        guard
            .as_ref()
            .map(|p| p.get_overflow_files().len())
            .unwrap_or(0)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // A test that intentionally triggers a rescan leaves a walk running on
        // the background pool; let it finish before the tree disappears.
        self.picker
            .wait_for_indexing_complete(Duration::from_secs(10));
    }
}

fn event(kind: EventKind, path: PathBuf) -> DebouncedEvent {
    DebouncedEvent::new(Event::new(kind).add_path(path), Instant::now())
}

fn create(path: PathBuf) -> DebouncedEvent {
    event(EventKind::Create(CreateKind::File), path)
}

fn modify(path: PathBuf) -> DebouncedEvent {
    event(
        EventKind::Modify(ModifyKind::Data(DataChange::Content)),
        path,
    )
}

fn remove_file(path: PathBuf) -> DebouncedEvent {
    event(EventKind::Remove(RemoveKind::File), path)
}
