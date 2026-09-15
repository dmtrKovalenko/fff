use std::fs;

use fff_search::file_picker::FilePicker;
use fff_search::{
    FilePickerOptions, FuzzySearchOptions, IndexEntry, LayerBuilder, LayerSnapshot, PaginationArgs,
    QueryParser, SESSION_LAYER, SnapshotLayout,
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

fn live_paths(picker: &FilePicker) -> Vec<String> {
    let mut paths: Vec<String> = picker
        .get_files()
        .iter()
        .filter(|f| !f.is_deleted())
        .map(|f| f.relative_path(picker))
        .collect();
    paths.sort();
    paths
}

fn layer_paths(picker: &FilePicker, layer: u8) -> Vec<String> {
    picker
        .files_in_layer(layer)
        .iter()
        .filter(|f| !f.is_deleted())
        .map(|f| f.relative_path(picker))
        .collect()
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
                limit: 100,
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

fn search_dirs(picker: &FilePicker, query: &str) -> Vec<String> {
    let parser = QueryParser::default();
    let parsed = parser.parse(query);
    let result = picker.fuzzy_search_directories(
        &parsed,
        FuzzySearchOptions {
            max_threads: 1,
            pagination: PaginationArgs {
                offset: 0,
                limit: 100,
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

fn entries(paths: &[String]) -> Vec<IndexEntry<'_>> {
    paths
        .iter()
        .map(|p| IndexEntry::new(p).with_metadata(10, 1_700_000_000))
        .collect()
}

#[test]
fn add_entries_appends_to_the_open_layer_and_skips_duplicates() {
    let tmp = TempDir::new().expect("tempdir");
    let base = tmp.path();
    fs::write(base.join("scanned.rs"), "scanned\n").expect("write");

    let mut picker = picker(base);
    let added = picker
        .add_entries([
            IndexEntry::new("virtual/one.rs"),
            IndexEntry::new("virtual/two.rs"),
            IndexEntry::new("scanned.rs"),
            IndexEntry::new("virtual/one.rs"),
        ])
        .expect("add");
    assert_eq!(added, 2);

    let session = layer_paths(&picker, SESSION_LAYER);
    assert_eq!(session, vec!["virtual/one.rs", "virtual/two.rs"]);
    assert!(search_paths(&picker, "virtual").contains(&"virtual/two.rs".to_string()));
    assert!(search_dirs(&picker, "virtual").contains(&"virtual/".to_string()));

    // A second batch on the same open layer dedups against the first one.
    let added = picker
        .add_entries([
            IndexEntry::new("virtual/two.rs"),
            IndexEntry::new("virtual/three.rs"),
        ])
        .expect("add");
    assert_eq!(added, 1);
    assert_eq!(layer_paths(&picker, SESSION_LAYER).len(), 3);
}

#[test]
fn add_entries_grows_past_the_overflow_reserve() {
    let tmp = TempDir::new().expect("tempdir");
    let base = tmp.path();
    fs::write(base.join("scanned.rs"), "scanned\n").expect("write");

    let mut picker = picker(base);
    let paths: Vec<String> = (0..5000)
        .map(|i| format!("gen/d{}/f{i}.rs", i % 37))
        .collect();
    let added = picker.add_entries(entries(&paths)).expect("add");
    assert_eq!(added, paths.len());
    assert_eq!(picker.live_file_count(), paths.len() + 1);

    let found = search_paths(&picker, "f4999");
    assert!(found.contains(&"gen/d4/f4999.rs".to_string()), "{found:?}");
    assert!(picker.get_file_by_path(base.join("gen/d0/f0.rs")).is_some());
}

#[test]
fn imported_layer_is_searchable_and_sealed_lookups_work() {
    let tmp = TempDir::new().expect("tempdir");
    let base = tmp.path();
    fs::write(base.join("scanned.rs"), "scanned\n").expect("write");

    let mut builder = LayerBuilder::new(Some("generated"));
    for i in 0..3000 {
        let path = format!("out/pkg{}/mod{i}.ts", i % 11);
        assert!(builder.push(&IndexEntry::new(&path).with_metadata(i, 0)));
    }
    assert!(!builder.push(&IndexEntry::new("/abs.rs")));
    let snapshot = builder.finish();
    assert_eq!(snapshot.layout(), SnapshotLayout::Sorted);
    assert_eq!(snapshot.file_count(), 3000);

    let mut picker = picker(base);
    let layer = picker.import_layer(snapshot).expect("import");
    assert_eq!(layer, SESSION_LAYER + 1);
    assert_eq!(layer_paths(&picker, layer).len(), 3000);
    assert_eq!(picker.layers()[1].label.as_deref(), Some("generated"));
    assert_eq!(
        picker.current_layer(),
        layer,
        "imported layer becomes the top"
    );

    let found = search_paths(&picker, "mod2999");
    assert!(
        found.contains(&"out/pkg7/mod2999.ts".to_string()),
        "{found:?}"
    );
    assert!(search_dirs(&picker, "pkg10").contains(&"out/pkg10/".to_string()));

    // Sorted sealed layers resolve paths by binary search, e.g. for the
    // watcher re-touching a file that lives there.
    let path = base.join("out/pkg7/mod2999.ts");
    fs::create_dir_all(path.parent().unwrap()).expect("mkdir");
    fs::write(&path, "changed\n").expect("write");
    picker.handle_create_or_modify(&path);
    assert_eq!(layer_paths(&picker, layer).len(), 3000, "no duplicate row");
    assert_eq!(
        picker.get_file_by_path(&path).map(|f| f.size),
        Some("changed\n".len() as u64)
    );

    // A brand new file never reopens the sealed import; it lands in a fresh
    // open layer above it.
    let fresh = base.join("out/pkg0/new.ts");
    fs::create_dir_all(fresh.parent().unwrap()).expect("mkdir");
    fs::write(&fresh, "new\n").expect("write");
    picker.handle_create_or_modify(&fresh);
    assert_eq!(picker.current_layer(), layer + 1);
    assert_eq!(layer_paths(&picker, layer).len(), 3000);
    assert_eq!(layer_paths(&picker, layer + 1), vec!["out/pkg0/new.ts"]);
    let open: Vec<bool> = picker.layers().iter().map(|l| l.is_open).collect();
    assert_eq!(open, [false, false, true]);
}

#[test]
fn add_entries_revives_a_tombstoned_file() {
    let tmp = TempDir::new().expect("tempdir");
    let base = tmp.path();
    fs::write(base.join("scanned.rs"), "scanned\n").expect("write");

    let mut picker = picker(base);
    assert_eq!(
        picker
            .add_entries([IndexEntry::new("virtual/gone.rs")])
            .expect("add"),
        1
    );
    assert!(picker.remove_file_by_path(base.join("virtual/gone.rs")));
    assert!(layer_paths(&picker, SESSION_LAYER).is_empty());

    assert_eq!(
        picker
            .add_entries([IndexEntry::new("virtual/gone.rs")])
            .expect("add"),
        1
    );
    assert_eq!(layer_paths(&picker, SESSION_LAYER), vec!["virtual/gone.rs"]);
    assert_eq!(picker.files_in_layer(SESSION_LAYER).len(), 1, "slot reused");
    assert!(search_paths(&picker, "gone").contains(&"virtual/gone.rs".to_string()));
}

#[test]
fn overlay_round_trips_through_a_file() {
    let tmp = TempDir::new().expect("tempdir");
    let base = tmp.path();
    fs::write(base.join("scanned.rs"), "scanned\n").expect("write");

    let mut picker = picker(base);
    picker.create_layer(Some("persisted"));
    let paths: Vec<String> = (0..500)
        .map(|i| format!("cache/{}/x{i}.js", i % 5))
        .collect();
    picker.add_entries(entries(&paths)).expect("add");
    let layer = picker.current_layer();

    let file = tmp.path().join("layer.fff");
    let written = picker.save_layer(layer, &file).expect("save");
    assert_eq!(written, fs::metadata(&file).expect("meta").len());

    let loaded = LayerSnapshot::load(&file).expect("load");
    assert_eq!(loaded.label(), Some("persisted"));
    assert_eq!(loaded.file_count(), 500);
    let mut loaded_paths: Vec<String> = loaded.relative_paths().collect();
    loaded_paths.sort();
    let mut expected = paths.clone();
    expected.sort();
    assert_eq!(loaded_paths, expected);

    let mut fresh = self::picker(base);
    let id = fresh.import_layer(loaded).expect("import");
    assert_eq!(layer_paths(&fresh, id).len(), 500);
    assert!(search_paths(&fresh, "x499").contains(&"cache/4/x499.js".to_string()));
}

#[test]
fn base_scan_round_trips_through_a_file() {
    let tmp = TempDir::new().expect("tempdir");
    let base = tmp.path();
    for i in 0..200 {
        let path = base.join(format!("src/m{}/file{i}.rs", i % 7));
        fs::create_dir_all(path.parent().unwrap()).expect("mkdir");
        fs::write(&path, format!("fn f{i}() {{}}\n")).expect("write");
    }
    fs::write(base.join("logo.png"), b"\x89PNG").expect("write");

    let scanned = picker(base);
    let snapshot = scanned.export_layer(0).expect("export base");
    assert_eq!(snapshot.layout(), SnapshotLayout::BasePartitioned);
    assert_eq!(snapshot.file_count(), 201);

    let file = tmp.path().join("base.fff");
    snapshot.save(&file).expect("save");
    let loaded = LayerSnapshot::load(&file).expect("load");

    let mut restored = FilePicker::new(FilePickerOptions {
        base_path: base.to_string_lossy().to_string(),
        watch: false,
        enable_home_dir_scanning: true,
        ..Default::default()
    })
    .expect("create picker");
    restored.import_base(loaded, None).expect("import base");

    assert_eq!(live_paths(&restored), live_paths(&scanned));
    let dirs: Vec<String> = restored
        .get_dirs()
        .iter()
        .map(|d| d.relative_path(&restored))
        .collect();
    assert!(dirs.contains(&"src/m6/".to_string()), "{dirs:?}");
    assert_eq!(
        search_paths(&restored, "file199"),
        search_paths(&scanned, "file199")
    );
    assert!(
        restored
            .get_file_by_path(base.join("logo.png"))
            .expect("png")
            .is_binary()
    );

    // The restored base behaves like a scanned one for watcher updates.
    let created = base.join("src/m0/new.rs");
    fs::write(&created, "new\n").expect("write");
    restored.handle_create_or_modify(&created);
    assert_eq!(layer_paths(&restored, SESSION_LAYER), vec!["src/m0/new.rs"]);
}

#[test]
fn explicit_layers_survive_a_rescan() {
    let tmp = TempDir::new().expect("tempdir");
    let base = tmp.path();
    fs::write(base.join("scanned.rs"), "scanned\n").expect("write");

    let mut picker = picker(base);
    picker
        .add_entries([IndexEntry::new("session_only.rs")])
        .expect("add");
    let kept = picker.create_layer(Some("kept"));
    picker
        .add_entries([
            IndexEntry::new("virtual/kept.rs"),
            IndexEntry::new("scanned.rs"),
        ])
        .expect("add");

    fs::write(base.join("added_on_disk.rs"), "new\n").expect("write");
    picker.collect_files().expect("rescan");

    let labels: Vec<Option<String>> = picker.layers().iter().map(|l| l.label.clone()).collect();
    assert!(labels.contains(&Some("kept".to_string())), "{labels:?}");
    let kept = picker
        .layers()
        .iter()
        .find(|l| l.label.as_deref() == Some("kept"))
        .map(|l| l.id)
        .unwrap_or(kept);
    assert_eq!(layer_paths(&picker, kept), vec!["virtual/kept.rs"]);

    let paths = live_paths(&picker);
    assert!(paths.contains(&"added_on_disk.rs".to_string()));
    assert!(paths.contains(&"virtual/kept.rs".to_string()));
    assert_eq!(
        paths.iter().filter(|p| p.as_str() == "scanned.rs").count(),
        1
    );
    // The session layer is rebuilt from disk, not carried over.
    assert!(!paths.contains(&"session_only.rs".to_string()));
}

#[test]
fn export_base_before_scan_is_none_and_unknown_layer_fails() {
    let tmp = TempDir::new().expect("tempdir");
    let picker = FilePicker::new(FilePickerOptions {
        base_path: tmp.path().to_string_lossy().to_string(),
        watch: false,
        enable_home_dir_scanning: true,
        ..Default::default()
    })
    .expect("create picker");

    assert!(picker.export_layer(0).is_none());
    assert!(picker.save_layer(42, tmp.path().join("x.fff")).is_err());
}
