use std::fs;
use std::path::Path;
use tempfile::TempDir;

use fff_search::FilePickerOptions;
use fff_search::file_picker::FilePicker;
use fff_search::grep::{GrepMode, GrepSearchOptions, parse_grep_query};

const FILE_COUNT: usize = 10_000;
const NEEDLE: &str = "needle-in-a-haystack";

// With `enforce_time_budget` a zero-match search must stop at the budget and
// hand back a resume cursor instead of scanning every candidate. Issue #826.
#[test]
fn zero_match_search_stops_at_enforced_time_budget() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(tmp.path(), None);

    let parsed = parse_grep_query("zzz-absent-literal");
    let result = picker.grep(&parsed, &budget_opts(GrepMode::PlainText, true));

    assert_eq!(result.matches.len(), 0, "sanity: query must not match");
    assert_eq!(result.filtered_file_count, FILE_COUNT);
    assert!(
        result.total_files_searched < result.filtered_file_count,
        "budget expired but all {} candidate files were searched",
        result.filtered_file_count
    );
    assert!(
        result.next_file_offset > 0,
        "budget expired with no resume cursor"
    );
}

// Default stays as it always was: plain/regex ignores the budget until matches
// exist, so a zero-match query still scans everything and reports no cursor.
#[test]
fn zero_match_search_ignores_unenforced_time_budget() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(tmp.path(), None);

    let parsed = parse_grep_query("zzz-absent-literal");
    let result = picker.grep(&parsed, &budget_opts(GrepMode::PlainText, false));

    assert_eq!(result.matches.len(), 0, "sanity: query must not match");
    assert_eq!(result.total_files_searched, result.filtered_file_count);
    assert_eq!(result.next_file_offset, 0);
}

#[test]
fn zero_match_fuzzy_search_stops_at_time_budget() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(tmp.path(), None);

    let parsed = parse_grep_query("zzzabsentfuzzy");
    let result = picker.grep(&parsed, &budget_opts(GrepMode::Fuzzy, false));

    assert_eq!(result.matches.len(), 0, "sanity: query must not match");
    assert!(
        result.total_files_searched < result.filtered_file_count,
        "budget expired but all {} candidate files were searched",
        result.filtered_file_count
    );
    assert!(
        result.next_file_offset > 0,
        "budget expired with no resume cursor"
    );
}

// The resume cursor must not skip files: paging a budget-limited search to
// exhaustion has to find a needle that lives past the first page.
#[test]
fn budget_resume_cursor_does_not_skip_files() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(tmp.path(), Some(FILE_COUNT - 1));

    let parsed = parse_grep_query(NEEDLE);
    let mut opts = budget_opts(GrepMode::PlainText, true);
    let mut found = 0usize;
    let mut pages = 0usize;

    loop {
        let result = picker.grep(&parsed, &opts);
        found += result.matches.len();
        pages += 1;

        assert!(pages < 5_000, "paging did not terminate");
        if result.next_file_offset == 0 {
            break;
        }
        assert!(
            result.next_file_offset > opts.file_offset,
            "cursor did not advance: {} -> {}",
            opts.file_offset,
            result.next_file_offset
        );
        opts.file_offset = result.next_file_offset;
    }

    assert_eq!(
        found, 1,
        "resume cursor skipped the file holding the needle"
    );
}

// 10k files of 4KiB filler. `needle_at` gets NEEDLE appended so paging can be
// checked for skipped files.
fn create_picker(base: &Path, needle_at: Option<usize>) -> FilePicker {
    let filler = format!("{}\n", "x".repeat(4 * 1024));
    for i in 0..FILE_COUNT {
        let path = base.join(format!("file-{i}.txt"));
        if needle_at == Some(i) {
            fs::write(&path, format!("{filler}{NEEDLE}\n")).unwrap();
        } else {
            fs::write(&path, &filler).unwrap();
        }
    }
    let mut picker = FilePicker::new(FilePickerOptions {
        base_path: base.to_string_lossy().to_string(),
        enable_mmap_cache: false,
        watch: false,
        ..Default::default()
    })
    .expect("Failed to create FilePicker");
    picker.collect_files().expect("Failed to collect files");
    picker
}

fn budget_opts(mode: GrepMode, enforce_time_budget: bool) -> GrepSearchOptions {
    GrepSearchOptions {
        max_file_size: 1024 * 1024,
        max_matches_per_file: 200,
        smart_case: true,
        file_offset: 0,
        page_limit: 500,
        mode,
        time_budget_ms: 5,
        enforce_time_budget,
        before_context: 0,
        after_context: 0,
        classify_definitions: false,
        trim_whitespace: false,
        abort_signal: None,
    }
}
