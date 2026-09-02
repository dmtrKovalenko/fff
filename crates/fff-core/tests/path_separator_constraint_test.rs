//! Regression test for https://github.com/dmtrKovalenko/fff/issues/381
//!
//! Directory (`PathSegment`) and file-path (`FilePath`) constraints must
//! return results on every platform. Indexed paths on Windows use native
//! backslash separators, so constraint matching has to accept either `/`
//! or `\\` as a path boundary.

use std::fs;
use std::path::Path;
use tempfile::TempDir;

use fff_search::file_picker::FilePicker;
use fff_search::grep::{GrepMode, GrepSearchOptions, parse_grep_query};
use fff_search::{Constraint, FilePickerOptions, FuzzySearchOptions, PaginationArgs, QueryParser};

fn create_picker(base: &Path, specs: &[(&str, &str)]) -> FilePicker {
    for (rel, contents) in specs {
        let full_path = base.join(rel);
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&full_path, contents).unwrap();
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

fn plain_opts() -> GrepSearchOptions {
    GrepSearchOptions {
        max_file_size: 10 * 1024 * 1024,
        max_matches_per_file: 200,
        smart_case: true,
        file_offset: 0,
        page_limit: 200,
        mode: GrepMode::PlainText,
        time_budget_ms: 0,
        enforce_time_budget: false,
        before_context: 0,
        after_context: 0,
        classify_definitions: false,
        trim_whitespace: false,
        abort_signal: None,
    }
}

fn fuzzy_search_paths(picker: &FilePicker, query: &str) -> Vec<String> {
    let parser = QueryParser::default();
    let parsed = parser.parse(query);
    let result = picker.fuzzy_search(
        &parsed,
        None,
        FuzzySearchOptions {
            max_threads: 1,
            pagination: PaginationArgs {
                offset: 0,
                limit: 200,
            },
            ..Default::default()
        },
    );
    result
        .items
        .iter()
        .map(|f| f.relative_path(picker))
        .collect()
}

/// Treat a relative path as a sequence of components regardless of the
/// native separator so assertions are portable across Linux, macOS, Windows.
fn has_segment(path: &str, segment: &str) -> bool {
    path.split(['/', '\\']).any(|s| s == segment)
}

/// `grep handleRequest src/` — PathSegment constraint must match a nested
/// `src` directory on every platform.
#[test]
fn grep_with_path_segment_constraint_nested() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(
        tmp.path(),
        &[
            ("app/modules/src/services/handler.lua", "handleRequest()\n"),
            ("app/modules/lib/util.lua", "handleRequest()\n"),
            ("src/main.rs", "fn handleRequest() {}\n"),
        ],
    );

    let parsed = parse_grep_query("handleRequest src/");
    let result = picker.grep(&parsed, &plain_opts());

    let matched_paths: Vec<String> = result
        .files
        .iter()
        .map(|f| f.relative_path(&picker))
        .collect();
    assert_eq!(
        result.matches.len(),
        2,
        "expected matches in two src/ files, got {matched_paths:?}"
    );
    for p in &matched_paths {
        assert!(
            has_segment(p, "src"),
            "every matched file must live under a `src` segment, got {p:?}"
        );
    }
}

/// `multi_grep` with a `PathSegment` constraint.
#[test]
fn multi_grep_with_path_segment_constraint() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(
        tmp.path(),
        &[
            (
                "app/modules/src/controller.lua",
                "handleRequest\nprocessJob\n",
            ),
            ("app/modules/lib/helper.lua", "handleRequest\n"),
            ("app/src/legacy.lua", "processJob\n"),
        ],
    );

    let constraints = [Constraint::PathSegment("src")];
    let patterns = ["handleRequest", "processJob"];
    let result = picker.multi_grep(&patterns, &constraints, &plain_opts());

    assert!(
        !result.matches.is_empty(),
        "multi_grep with `src/` constraint should return matches"
    );

    let matched_paths: Vec<String> = result
        .files
        .iter()
        .map(|f| f.relative_path(&picker))
        .collect();
    for p in &matched_paths {
        assert!(
            has_segment(p, "src"),
            "every matched file must live under a `src` segment, got {p:?}"
        );
    }
    assert!(matched_paths.iter().any(|p| p.contains("controller.lua")));
    assert!(matched_paths.iter().any(|p| p.contains("legacy.lua")));
}

/// Fuzzy search (`find_files src/ Controller`) must apply the path-segment
/// filter to paths stored during indexing.
#[test]
fn fuzzy_search_with_path_segment_constraint() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(
        tmp.path(),
        &[
            ("app/modules/src/services/BaseController.lua", "base\n"),
            ("app/modules/src/services/UserController.lua", "user\n"),
            ("app/modules/lib/BaseController.lua", "lib base\n"),
            ("tests/src/MockController.lua", "mock\n"),
        ],
    );

    let results = fuzzy_search_paths(&picker, "src/ Controller");

    assert!(
        !results.is_empty(),
        "fuzzy search with `src/` constraint should return results"
    );
    for p in &results {
        assert!(
            has_segment(p, "src"),
            "every result must live under `src`, got {p:?}"
        );
    }
    assert!(results.iter().any(|p| p.contains("BaseController")));
    assert!(results.iter().any(|p| p.contains("UserController")));
    assert!(results.iter().any(|p| p.contains("MockController")));
}

/// `FilePath` suffix constraint must match stored paths even when components
/// are separated by the platform-native separator during indexing.
#[test]
fn multi_grep_with_file_path_suffix_constraint() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(
        tmp.path(),
        &[
            ("app/modules/src/services/handler.lua", "handleRequest\n"),
            ("other/src/services/handler.lua", "handleRequest\n"),
            ("app/modules/src/services/other.lua", "handleRequest\n"),
        ],
    );

    let constraints = [Constraint::FilePath("services/handler.lua")];
    let patterns = ["handleRequest"];
    let result = picker.multi_grep(&patterns, &constraints, &plain_opts());

    let paths: Vec<String> = result
        .files
        .iter()
        .map(|f| f.relative_path(&picker))
        .collect();
    assert_eq!(
        paths.len(),
        2,
        "expected two matches for services/handler.lua, got {paths:?}"
    );
    for p in &paths {
        let ends_with_services_handler =
            p.ends_with("services/handler.lua") || p.ends_with("services\\handler.lua");
        assert!(
            ends_with_services_handler,
            "matched path must end with services/handler.lua, got {p:?}"
        );
    }
}

#[test]
fn multi_grep_with_missing_file_path_constraint_returns_no_matches() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(tmp.path(), &[("other.lua", "handleRequest\n")]);

    let constraints = [Constraint::FilePath("missing.lua")];
    let result = picker.multi_grep(&["handleRequest"], &constraints, &plain_opts());

    assert!(result.matches.is_empty());
}

/// Glob constraints must match native Windows paths — the picker normalises
/// separators when handing paths to the glob matcher.
#[test]
fn fuzzy_search_with_glob_constraint_matches_on_windows_paths() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(
        tmp.path(),
        &[
            ("app/src/components/Button.lua", "\n"),
            ("app/src/services/handler.lua", "\n"),
            ("app/lib/components/Ignored.lua", "\n"),
        ],
    );

    let results = fuzzy_search_paths(&picker, "**/src/**/*.lua");
    assert!(
        results.iter().any(|p| p.contains("Button.lua")),
        "glob `**/src/**/*.lua` must match files below any `src/`, got {results:?}"
    );
    assert!(
        results.iter().any(|p| p.contains("handler.lua")),
        "glob `**/src/**/*.lua` must match services/handler.lua, got {results:?}"
    );
}
