#![cfg(feature = "zlob")]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fff_search::file_picker::FilePicker;
use fff_search::{FilePickerOptions, SharedFilePicker, SharedFrecency};
use tempfile::TempDir;

#[test]
fn collect_files_applies_extra_ignore() {
    let (_tmp, base) = init_repo("secrets/\n*.log\n");
    for rel in [
        "main.rs",
        "debug.log",
        "keep.log",
        "secrets/key.env",
        "secrets/deep/token.env",
        "generated/out.rs",
    ] {
        write(&base, rel);
    }

    let mut picker = FilePicker::new(FilePickerOptions {
        base_path: base.to_string_lossy().into_owned(),
        watch: false,
        extra_ignore: vec!["!secrets/".into(), "!keep.log".into(), "generated/".into()],
        ..Default::default()
    })
    .unwrap();
    picker.collect_files().unwrap();

    for rel in [
        "main.rs",
        "keep.log",
        "secrets/key.env",
        "secrets/deep/token.env",
    ] {
        assert!(is_indexed(&picker, rel), "{rel}");
    }
    assert!(
        !is_indexed(&picker, "debug.log"),
        "non-included ignores stay out"
    );
    assert!(
        !is_indexed(&picker, "generated/out.rs"),
        "extra ignores apply"
    );
}

// Dotfiles repos ignore everything with `*`; plain git semantics need both lines.
#[test]
fn extra_ignore_rescues_star_ignored_subtree() {
    let (_tmp, base) = init_repo("*\n");
    for rel in ["nvim/init.lua", "nvim/lua/mappings.lua", "zsh/zshrc"] {
        write(&base, rel);
    }

    let mut picker = FilePicker::new(FilePickerOptions {
        base_path: base.to_string_lossy().into_owned(),
        watch: false,
        extra_ignore: vec!["!nvim/".into(), "!nvim/**".into()],
        ..Default::default()
    })
    .unwrap();
    picker.collect_files().unwrap();

    let mut rels: Vec<String> = picker
        .get_files()
        .iter()
        .map(|f| f.relative_path(&picker))
        .collect();
    rels.sort();
    assert_eq!(rels, vec!["nvim/init.lua", "nvim/lua/mappings.lua"]);
}

// The watcher filters new files with the same rules the walk used.
#[test]
fn watcher_honors_extra_ignore() {
    let (_tmp, base) = init_repo("secrets/\n*.log\n");
    write(&base, "main.rs");
    write(&base, "secrets/key.env");
    fs::create_dir_all(base.join("generated")).unwrap();

    let shared = SharedFilePicker::default();
    FilePicker::new_with_shared_state(
        shared.clone(),
        SharedFrecency::noop(),
        FilePickerOptions {
            base_path: base.to_string_lossy().into_owned(),
            watch: true,
            extra_ignore: vec!["!secrets/".into(), "generated/".into()],
            ..Default::default()
        },
    )
    .unwrap();
    assert!(shared.wait_for_scan(Duration::from_secs(30)));
    assert!(shared.wait_for_watcher(Duration::from_secs(30)));

    assert!(shared_indexed(&shared, "secrets/key.env"));

    write(&base, "debug.log");
    write(&base, "generated/new.rs");
    write(&base, "secrets/new.env");
    assert!(
        wait_until(|| shared_indexed(&shared, "secrets/new.env")),
        "watcher dropped a force-included file"
    );
    assert!(!shared_indexed(&shared, "debug.log"));
    assert!(!shared_indexed(&shared, "generated/new.rs"));
}

fn init_repo(gitignore: &str) -> (TempDir, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let base = fs::canonicalize(tmp.path()).unwrap();
    git2::Repository::init(&base).unwrap();
    fs::write(base.join(".gitignore"), gitignore).unwrap();
    (tmp, base)
}

fn write(base: &Path, rel: &str) {
    let path = base.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, b"x").unwrap();
}

fn is_indexed(picker: &FilePicker, rel: &str) -> bool {
    picker
        .get_files()
        .iter()
        .any(|f| f.relative_path(picker) == rel)
}

fn shared_indexed(shared: &SharedFilePicker, rel: &str) -> bool {
    shared
        .read()
        .ok()
        .and_then(|guard| guard.as_ref().map(|p| is_indexed(p, rel)))
        .unwrap_or(false)
}

fn wait_until(mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}
