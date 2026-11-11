use std::fs;

use fff_search::file_picker::FilePicker;
use fff_search::{
    Error, FFFQuery, FilePickerOptions, FuzzySearchOptions, IndexEntry, LayerBuilder,
    LayerSnapshot, MAX_OVERLAY_LAYERS, PaginationArgs, QueryParser, SESSION_LAYER,
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

fn write(base: &std::path::Path, relative_path: &str) {
    let path = base.join(relative_path);
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    fs::write(&path, format!("contents of {relative_path}\n")).expect("write");
}

fn names(picker: &FilePicker, id: u8) -> Vec<String> {
    let mut names: Vec<String> = picker
        .files_in_layer(id)
        .iter()
        .filter(|f| !f.is_deleted())
        .map(|f| f.relative_path(picker))
        .collect();
    names.sort();
    names
}

fn layer(label: &str, paths: &[&str]) -> LayerSnapshot {
    let mut builder = LayerBuilder::new(Some(label));
    for path in paths {
        assert!(builder.push(&IndexEntry::new(path)));
    }
    builder.finish()
}

#[test]
fn watcher_files_stay_in_the_session_layer_after_create_layer() {
    let tmp = TempDir::new().expect("tempdir");
    let base = tmp.path();
    fs::write(base.join("scanned.rs"), "scanned\n").expect("write");

    let mut picker = picker(base);
    add(&mut picker, base, "one.rs");

    let second = picker.create_layer(Some("second")).expect("create layer");
    assert_eq!(second, SESSION_LAYER + 1);
    assert_eq!(picker.current_layer(), second);
    add(&mut picker, base, "two.rs");
    picker
        .add_entries([IndexEntry::new("virtual/second.rs")])
        .unwrap();

    let third = picker.create_layer(None).expect("create layer");
    add(&mut picker, base, "three.rs");
    picker
        .add_entries([IndexEntry::new("virtual/third.rs")])
        .unwrap();

    assert_eq!(
        names(&picker, SESSION_LAYER),
        ["one.rs", "three.rs", "two.rs"]
    );
    assert_eq!(names(&picker, second), ["virtual/second.rs"]);
    assert_eq!(names(&picker, third), ["virtual/third.rs"]);
    assert_eq!(picker.current_layer(), third);

    let layers = picker.layers();
    let ids: Vec<u8> = layers.iter().map(|l| l.id).collect();
    assert_eq!(ids, [SESSION_LAYER, second, third]);
    assert_eq!(layers[0].label.as_deref(), Some("session"));
    assert_eq!(layers[1].label.as_deref(), Some("second"));
    let open: Vec<bool> = layers.iter().map(|l| l.is_open).collect();
    assert_eq!(open, [true, false, true]);
}

#[test]
fn search_resolves_paths_from_every_layer() {
    let tmp = TempDir::new().expect("tempdir");
    let base = tmp.path();
    fs::write(base.join("alpha_scanned.rs"), "scanned\n").expect("write");

    let mut picker = picker(base);
    add(&mut picker, base, "alpha_session.rs");
    picker.create_layer(Some("second")).expect("create layer");
    picker
        .add_entries([IndexEntry::new("alpha_second.rs")])
        .unwrap();
    picker
        .import_layer(layer("third", &["nested/alpha_third.rs"]))
        .expect("import layer");
    add(&mut picker, base, "nested/alpha_late.rs");

    let found = search_paths(&picker, "alpha");
    for expected in [
        "alpha_scanned.rs",
        "alpha_session.rs",
        "alpha_second.rs",
        "nested/alpha_third.rs",
        "nested/alpha_late.rs",
    ] {
        assert!(
            found.contains(&expected.to_string()),
            "{expected} missing from {found:?}"
        );
    }
}

#[test]
fn the_layer_cap_fails_without_changing_any_layer() {
    let tmp = TempDir::new().expect("tempdir");
    let base = tmp.path();
    fs::write(base.join("scanned.rs"), "scanned\n").expect("write");

    let mut picker = picker(base);
    let big = picker.create_layer(Some("big")).expect("create layer");
    let paths: Vec<String> = (0..100).map(|i| format!("big/{i}.rs")).collect();
    picker
        .add_entries(paths.iter().map(|p| IndexEntry::new(p)))
        .unwrap();
    let small = picker.create_layer(Some("small")).expect("create layer");
    picker
        .add_entries([IndexEntry::new("small/one.rs")])
        .unwrap();
    assert_eq!(MAX_OVERLAY_LAYERS, 3);

    assert!(matches!(
        picker.create_layer(Some("extra")),
        Err(Error::LayerLimit)
    ));
    assert!(matches!(
        picker.import_layer(layer("extra", &["extra.rs"])),
        Err(Error::LayerLimit)
    ));

    let labels: Vec<Option<String>> = picker.layers().iter().map(|l| l.label.clone()).collect();
    assert_eq!(
        labels,
        [
            Some("session".into()),
            Some("big".into()),
            Some("small".into())
        ]
    );
    assert_eq!(names(&picker, big).len(), 100);
    assert_eq!(names(&picker, small), ["small/one.rs"]);
    // The open layer keeps accepting paths.
    picker
        .add_entries([IndexEntry::new("small/two.rs")])
        .unwrap();
    assert_eq!(names(&picker, small), ["small/one.rs", "small/two.rs"]);
}

#[test]
fn watcher_additions_never_renumber_or_relabel_layers() {
    let tmp = TempDir::new().expect("tempdir");
    let base = tmp.path();
    fs::write(base.join("scanned.rs"), "scanned\n").expect("write");

    let mut picker = picker(base);
    let a = picker.import_layer(layer("a", &["a/x.rs"])).unwrap();
    let b = picker.import_layer(layer("b", &["b/y.rs"])).unwrap();
    assert_eq!((a, b), (2, 3));

    add(&mut picker, base, "new_on_disk.rs");

    let labels: Vec<(u8, Option<String>)> = picker
        .layers()
        .iter()
        .map(|l| (l.id, l.label.clone()))
        .collect();
    assert_eq!(
        labels,
        [
            (SESSION_LAYER, Some("session".into())),
            (a, Some("a".into())),
            (b, Some("b".into()))
        ]
    );
    assert_eq!(names(&picker, SESSION_LAYER), ["new_on_disk.rs"]);
    assert_eq!(names(&picker, a), ["a/x.rs"]);
    assert_eq!(names(&picker, b), ["b/y.rs"]);
    let saved: Vec<String> = picker.export_layer(b).unwrap().relative_paths().collect();
    assert_eq!(saved, ["b/y.rs"]);
    // No open layer and no free id: explicit paths are refused, not merged.
    assert!(matches!(
        picker.add_entries([IndexEntry::new("c.rs")]),
        Err(Error::LayerLimit)
    ));
}

#[test]
fn growing_an_older_layer_moves_the_layers_above_it() {
    let tmp = TempDir::new().expect("tempdir");
    let base = tmp.path();
    fs::write(base.join("scanned.rs"), "scanned\n").expect("write");

    let mut picker = picker(base);
    let generated = picker.create_layer(Some("generated")).unwrap();
    picker.add_entries([IndexEntry::new("gen/a.rs")]).unwrap();
    let imported = picker
        .import_layer(layer("imported", &["pre/x.rs", "pre/sub/y.rs"]))
        .unwrap();
    add(&mut picker, base, "sess/one.rs");
    add(&mut picker, base, "sess/deep/two.rs");

    // The import keeps the created layer open, so these land in it.
    assert_eq!(picker.current_layer(), generated);
    let added = picker
        .add_entries([
            IndexEntry::new("gen/b.rs"),
            IndexEntry::new("gen/new/c.rs"),
            IndexEntry::new("sess/from_gen.rs"),
            IndexEntry::new("pre/sub/from_gen.rs"),
        ])
        .unwrap();
    assert_eq!(added, 4);

    assert_eq!(
        names(&picker, generated),
        [
            "gen/a.rs",
            "gen/b.rs",
            "gen/new/c.rs",
            "pre/sub/from_gen.rs",
            "sess/from_gen.rs"
        ]
    );
    assert_eq!(names(&picker, imported), ["pre/sub/y.rs", "pre/x.rs"]);
    assert_eq!(
        names(&picker, SESSION_LAYER),
        ["sess/deep/two.rs", "sess/one.rs"]
    );

    let mut dirs: Vec<String> = picker
        .dirs_in_layer(generated)
        .iter()
        .map(|d| d.relative_path(&picker))
        .collect();
    dirs.sort();
    assert_eq!(dirs, ["gen/", "gen/new/"]);
    let session_dirs: Vec<String> = picker
        .dirs_in_layer(SESSION_LAYER)
        .iter()
        .map(|d| d.relative_path(&picker))
        .collect();
    assert_eq!(session_dirs, ["sess/", "sess/deep/"]);

    for path in [
        "gen/a.rs",
        "gen/new/c.rs",
        "pre/sub/y.rs",
        "pre/sub/from_gen.rs",
        "sess/deep/two.rs",
        "sess/from_gen.rs",
    ] {
        let file = picker.get_file_by_path(base.join(path));
        assert_eq!(
            file.map(|f| f.relative_path(&picker)).as_deref(),
            Some(path)
        );
    }
    add(&mut picker, base, "sess/deep/three.rs");
    assert!(
        picker
            .get_file_by_path(base.join("sess/deep/three.rs"))
            .is_some()
    );
    assert!(search_paths(&picker, "from_gen").len() == 2);
}

#[test]
fn grep_reads_content_from_every_layer() {
    use fff_search::grep::{GrepMode, GrepSearchOptions};

    let tmp = TempDir::new().expect("tempdir");
    let base = tmp.path();
    fs::write(base.join("scanned.rs"), "nothing\n").expect("write");

    let mut picker = picker(base);
    add(&mut picker, base, "session/needle.rs");
    for id in 0..MAX_OVERLAY_LAYERS - 1 {
        picker.create_layer(None).expect("create layer");
        let path = format!("dir{id}/needle.rs");
        write(base, &path);
        let size = fs::metadata(base.join(&path)).unwrap().len();
        picker
            .add_entries([IndexEntry::new(&path).with_metadata(size, 1)])
            .unwrap();
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

    assert_eq!(result.matches.len(), MAX_OVERLAY_LAYERS);
}

#[test]
fn directory_search_covers_every_layer() {
    let tmp = TempDir::new().expect("tempdir");
    let base = tmp.path();
    fs::write(base.join("scanned.rs"), "scanned\n").expect("write");

    let mut picker = picker(base);
    add(&mut picker, base, "widget0/file.rs");
    for i in 1..MAX_OVERLAY_LAYERS {
        picker.create_layer(None).expect("create layer");
        picker
            .add_entries([IndexEntry::new(&format!("widget{i}/file.rs"))])
            .unwrap();
        add(&mut picker, base, &format!("widget{i}s/file.rs"));
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

    for i in 0..MAX_OVERLAY_LAYERS {
        let expected = format!("widget{i}/");
        assert!(dirs.contains(&expected), "{expected} missing from {dirs:?}");
    }
}

#[test]
fn detached_paths_are_checked_after_a_rescan() {
    let root = TempDir::new().unwrap();
    let mut picker = picker(root.path());
    add(&mut picker, root.path(), "virtual/escaped.rs");
    let file = picker.files_in_layer(SESSION_LAYER)[0].clone();
    let dir = picker.dirs_in_layer(SESSION_LAYER)[0].clone();
    assert_eq!(file.relative_path(&picker), "virtual/escaped.rs");
    assert_eq!(dir.relative_path(&picker), "virtual/");

    // The rescan rebuilds the session layer, so the copies no longer resolve.
    picker.collect_files().expect("rescan");
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| file.relative_path(&picker)))
            .is_err()
    );
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| dir.relative_path(&picker)))
            .is_err()
    );
}
