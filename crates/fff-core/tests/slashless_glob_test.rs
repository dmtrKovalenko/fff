use std::fs;
use std::path::Path;
use tempfile::TempDir;

use fff_search::file_picker::FilePicker;
use fff_search::{FilePickerOptions, FuzzySearchOptions, PaginationArgs, QueryParser};

const FILES: &[&str] = &[
    "hash-glibc/lib/libc.so.6",
    "libc.so.1",
    "libc.so.d/readme.md",
    "src/main.rs",
    "deep/nested/odd{name.txt",
];

fn create_picker(base: &Path) -> FilePicker {
    for rel in FILES {
        let path = base.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "x\n").unwrap();
    }
    let mut picker = FilePicker::new(FilePickerOptions {
        base_path: base.to_string_lossy().to_string(),
        enable_mmap_cache: false,
        watch: false,
        ..Default::default()
    })
    .expect("failed to create FilePicker");
    picker.collect_files().expect("failed to collect files");
    picker
}

fn options<'a>() -> FuzzySearchOptions<'a> {
    FuzzySearchOptions {
        max_threads: 1,
        pagination: PaginationArgs {
            offset: 0,
            limit: 200,
        },
        ..Default::default()
    }
}

fn search(picker: &FilePicker, query: &str) -> Vec<String> {
    let parsed = QueryParser::default().parse(query);
    let mut paths: Vec<String> = picker
        .fuzzy_search(&parsed, None, options())
        .items
        .iter()
        .map(|f| f.relative_path(picker))
        .collect();
    paths.sort();
    paths
}

fn glob(picker: &FilePicker, pattern: &str) -> Vec<String> {
    let mut paths: Vec<String> = picker
        .glob(pattern, options())
        .items
        .iter()
        .map(|f| f.relative_path(picker))
        .collect();
    paths.sort();
    paths
}

// A query needs no opt-in: the default search and `glob()` both use the basename rule.
#[test]
fn slashless_globs_match_file_names_at_any_depth_by_default() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(tmp.path());
    let libc = ["hash-glibc/lib/libc.so.6", "libc.so.1"];

    assert_eq!(search(&picker, "libc.so*"), libc);
    assert_eq!(glob(&picker, "libc.so*"), libc);
    assert_eq!(glob(&picker, "src/*"), ["src/main.rs"]);
}

// This pattern takes the globset path in zlob builds too.
#[test]
fn globset_fallback_uses_the_same_basename_rule() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(tmp.path());

    assert_eq!(glob(&picker, "odd[{]*"), ["deep/nested/odd{name.txt"]);
}
