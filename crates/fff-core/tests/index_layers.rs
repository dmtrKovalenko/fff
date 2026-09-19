use std::fs;

use fff_search::file_picker::FilePicker;
use fff_search::{
    FFFQuery, FilePickerOptions, FuzzySearchOptions, MAX_OVERLAY_LAYERS, PaginationArgs,
    QueryParser, SESSION_LAYER,
};
use tempfile::TempDir;

fn picker(base: &std::path::Path) -> FilePicker {
    let mut picker = FilePicker::new(FilePickerOptions {
        base_path: base.to_string_lossy().to_string(),
        watch: false,
        enable_home_dir_scanning: true,
        ..Default::default()
    })
    .expect("create picker");

    picker.collect_files().expect("initial scan");
    picker
}

fn add(picker: &mut FilePicker, base: &std::path::Path, relative_path: &str) {
    let path = base.join(relative_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir");
    }
    fs::write(&path, format!("contents of {relative_path}\n")).expect("write");
    picker
        .handle_create_or_modify(&path)
        .expect("index new file");
}

fn search_paths(picker: &FilePicker, query: &str) -> Vec<String> {
    let parser = QueryParser::default();
    let parsed = parser.parse(query);
    let result = picker.fuzzy_search(
        &parsed,
        None,
        FuzzySearchOptions {
            max_threads: 1,
            pagination: PaginationArgs {
                offset: 0,
                limit: 50,
            },
            ..Default::default()
        },
    );

    result
        .items
        .iter()
        .map(|item| item.relative_path(picker))
        .collect()
}

#[test]
fn watcher_files_land_in_the_session_layer() {
    let tmp = TempDir::new().expect("tempdir");
    let base = tmp.path();
    fs::write(base.join("scanned.rs"), "scanned\n").expect("write");

    let mut picker = picker(base);
    assert_eq!(picker.current_layer(), SESSION_LAYER);

    add(&mut picker, base, "src/session_only.rs");

    let session: Vec<String> = picker
        .files_in_layer(SESSION_LAYER)
        .iter()
        .map(|f| f.relative_path(&picker))
        .collect();

    assert_eq!(session, vec!["src/session_only.rs".to_string()]);
    // Layer 0 is the base scan, untouched by watcher additions.
    let base_files: Vec<String> = picker
        .files_in_layer(0)
        .iter()
        .map(|f| f.relative_path(&picker))
        .collect();
    assert_eq!(base_files, vec!["scanned.rs".to_string()]);
}

#[test]
fn each_layer_owns_the_files_added_after_it_was_created() {
    let tmp = TempDir::new().expect("tempdir");
    let base = tmp.path();
    fs::write(base.join("scanned.rs"), "scanned\n").expect("write");

    let mut picker = picker(base);
    add(&mut picker, base, "one.rs");

    let second = picker.create_layer(Some("second"));
    assert_eq!(second, SESSION_LAYER + 1);
    add(&mut picker, base, "two.rs");

    let third = picker.create_layer(None);
    add(&mut picker, base, "three.rs");

    let names = |id| -> Vec<String> {
        picker
            .files_in_layer(id)
            .iter()
            .map(|f| f.relative_path(&picker))
            .collect()
    };

    assert_eq!(names(SESSION_LAYER), vec!["one.rs".to_string()]);
    assert_eq!(names(second), vec!["two.rs".to_string()]);
    assert_eq!(names(third), vec!["three.rs".to_string()]);
    assert_eq!(picker.current_layer(), third);

    let layers = picker.layers();
    assert_eq!(layers.len(), 3);
    assert_eq!(layers[0].label.as_deref(), Some("session"));
    assert_eq!(layers[1].label.as_deref(), Some("second"));
    assert!(layers[2].is_open);
    assert!(layers.iter().all(|l| l.live_file_count == 1));
}

#[test]
fn search_resolves_paths_from_every_layer() {
    let tmp = TempDir::new().expect("tempdir");
    let base = tmp.path();
    fs::write(base.join("alpha_scanned.rs"), "scanned\n").expect("write");

    let mut picker = picker(base);
    add(&mut picker, base, "alpha_session.rs");
    picker.create_layer(Some("second"));
    add(&mut picker, base, "alpha_second.rs");
    picker.create_layer(Some("third"));
    add(&mut picker, base, "nested/alpha_third.rs");

    let found = search_paths(&picker, "alpha");
    for expected in [
        "alpha_scanned.rs",
        "alpha_session.rs",
        "alpha_second.rs",
        "nested/alpha_third.rs",
    ] {
        assert!(
            found.contains(&expected.to_string()),
            "{expected} missing from {found:?}"
        );
    }
}

#[test]
fn exceeding_the_layer_cap_compacts_without_losing_files() {
    let tmp = TempDir::new().expect("tempdir");
    let base = tmp.path();
    fs::write(base.join("alpha_scanned.rs"), "scanned\n").expect("write");

    let mut picker = picker(base);

    // One file per layer, then keep going past the cap.
    let total = MAX_OVERLAY_LAYERS + 3;
    for i in 0..total {
        add(&mut picker, base, &format!("dir{i}/alpha_{i}.rs"));
        if i + 1 < total {
            picker.create_layer(None);
        }
    }

    assert!(picker.layers().len() <= MAX_OVERLAY_LAYERS);

    // The session layer survives every compaction.
    let session: Vec<String> = picker
        .files_in_layer(SESSION_LAYER)
        .iter()
        .map(|f| f.relative_path(&picker))
        .collect();
    assert_eq!(session, vec!["dir0/alpha_0.rs".to_string()]);

    // Every path is still resolvable through search after the arena rewrite.
    let found = search_paths(&picker, "alpha");
    for i in 0..total {
        let expected = format!("dir{i}/alpha_{i}.rs");
        assert!(
            found.contains(&expected),
            "{expected} missing from {found:?}"
        );
    }

    // Layer ids stay contiguous and equal to their stack position.
    let ids: Vec<u8> = picker.layers().iter().map(|l| l.id).collect();
    assert_eq!(ids, (1..=ids.len() as u8).collect::<Vec<_>>());
    assert_eq!(picker.current_layer(), *ids.last().expect("layers"));
}

#[test]
fn grep_reads_content_of_files_in_compacted_layers() {
    use fff_search::grep::{GrepMode, GrepSearchOptions};

    let tmp = TempDir::new().expect("tempdir");
    let base = tmp.path();
    fs::write(base.join("scanned.rs"), "nothing\n").expect("write");

    let mut picker = picker(base);
    for i in 0..MAX_OVERLAY_LAYERS + 2 {
        add(&mut picker, base, &format!("dir{i}/needle_{i}.rs"));
        picker.create_layer(None);
    }

    let query = FFFQuery::parse("contents of", fff_search::AiGrepConfig);
    let result = picker.grep(
        &query,
        &GrepSearchOptions {
            mode: GrepMode::PlainText,
            page_limit: 50,
            smart_case: true,
            ..Default::default()
        },
    );

    assert_eq!(result.matches.len(), MAX_OVERLAY_LAYERS + 2);
}

#[test]
fn directory_search_survives_compaction() {
    let tmp = TempDir::new().expect("tempdir");
    let base = tmp.path();
    fs::write(base.join("scanned.rs"), "scanned\n").expect("write");

    let mut picker = picker(base);
    for i in 0..MAX_OVERLAY_LAYERS + 2 {
        add(&mut picker, base, &format!("widget{i}/file.rs"));
        picker.create_layer(None);
    }

    let parser = QueryParser::default();
    let parsed = parser.parse("widget");
    let result = picker.fuzzy_search_directories(
        &parsed,
        FuzzySearchOptions {
            max_threads: 1,
            pagination: PaginationArgs {
                offset: 0,
                limit: 50,
            },
            ..Default::default()
        },
    );

    let dirs: Vec<String> = result
        .items
        .iter()
        .map(|d| d.relative_path(&picker))
        .collect();

    for i in 0..MAX_OVERLAY_LAYERS + 2 {
        let expected = format!("widget{i}/");
        assert!(dirs.contains(&expected), "{expected} missing from {dirs:?}");
    }
}
