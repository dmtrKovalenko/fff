use std::fs;

use fff_search::file_picker::FilePicker;
use fff_search::grep::{GrepMode, GrepSearchOptions};
use fff_search::{AiGrepConfig, FFFQuery, FilePickerOptions};
use tempfile::TempDir;

// bug pinning https://github.com/dmtrKovalenko/fff/issues/618
#[test]
fn grep_path_constraint_on_overflow_file_does_not_segfault() {
    let tmp = TempDir::new().expect("tempdir");
    let base = tmp.path();
    let spec_dir = base.join("specs");
    fs::create_dir_all(&spec_dir).expect("mkdir specs");

    let file = spec_dir.join("annotation-plan.md");
    fs::write(&file, "dependency\n").expect("write test file");

    // Intentionally do NOT call `collect_files()`. This leaves the base path
    // arena unset/null. `handle_create_or_modify` then adds the file as an
    // overflow file whose path chunks live in the overflow arena.
    let mut picker = FilePicker::new(FilePickerOptions {
        base_path: base.to_string_lossy().to_string(),
        enable_mmap_cache: false,
        watch: false,
        enable_home_dir_scanning: true,
        ..Default::default()
    })
    .expect("create picker");

    let is_overflow = picker
        .handle_create_or_modify(&file)
        .expect("add overflow file")
        .is_overflow();
    assert!(is_overflow, "file must be added to the overflow arena");

    // Mirrors the `ffgrep({ pattern: "dependency", path: "specs/annotation-plan.md" })`
    // call that crashed: in AI grep mode the path token becomes a FilePath
    // constraint and `dependency` becomes the grep text.
    let parsed = FFFQuery::parse("specs/annotation-plan.md dependency", AiGrepConfig);

    let opts = GrepSearchOptions {
        mode: GrepMode::PlainText,
        page_limit: 20,
        smart_case: true,
        case_mode: None,
        ..Default::default()
    };

    // original issue:
    // Debug builds typically abort with Rust's unsafe precondition check at
    // simd_path.rs: ChunkedString::write_to_string. Release builds may SIGSEGV
    // in memmove from an address like 0x30 (null arena + chunk_index * 16).
    let result = picker.grep(&parsed, &opts);

    assert_eq!(result.files.len(), 1, "the overflow file should match");
    assert_eq!(result.matches.len(), 1, "`dependency` should match once");
}
