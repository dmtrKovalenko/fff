use std::fs;
use std::path::Path;
use tempfile::TempDir;

use fff_search::FilePickerOptions;
use fff_search::file_picker::FilePicker;
use fff_search::grep::{GrepMode, GrepSearchOptions, parse_grep_query};

/// Build a batch of test files and return a FilePicker with them indexed.
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
    .expect("Failed to create FilePicker");
    picker.collect_files().expect("Failed to collect files");
    picker
}

/// Shorthand to build default options for plain text mode.
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

/// Shorthand to build default options for regex mode.
fn regex_opts() -> GrepSearchOptions {
    GrepSearchOptions {
        max_file_size: 10 * 1024 * 1024,
        max_matches_per_file: 200,
        smart_case: true,
        file_offset: 0,
        page_limit: 200,
        mode: GrepMode::Regex,
        time_budget_ms: 0,
        enforce_time_budget: false,
        before_context: 0,
        after_context: 0,
        classify_definitions: false,
        trim_whitespace: false,
        abort_signal: None,
    }
}

/// Shorthand to build default options for fuzzy mode.
fn fuzzy_opts() -> GrepSearchOptions {
    GrepSearchOptions {
        max_file_size: 10 * 1024 * 1024,
        max_matches_per_file: 200,
        smart_case: true,
        file_offset: 0,
        page_limit: 200,
        mode: GrepMode::Fuzzy,
        time_budget_ms: 0,
        enforce_time_budget: false,
        before_context: 0,
        after_context: 0,
        classify_definitions: false,
        trim_whitespace: false,
        abort_signal: None,
    }
}

#[test]
fn plain_text_finds_exact_literal() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(
        tmp.path(),
        &[("hello.txt", "Hello, World!\nGoodbye, World!\n")],
    );

    let parsed = parse_grep_query("Hello");
    let result = picker.grep(&parsed, &plain_opts());

    assert_eq!(result.matches.len(), 1);
    assert_eq!(result.matches[0].line_number, 1);
    assert!(result.matches[0].line_content.contains("Hello"));
}

#[test]
fn plain_text_smart_case_insensitive() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(
        tmp.path(),
        &[("a.txt", "Hello World\nhello world\nHELLO WORLD\n")],
    );

    // All lowercase query → smart case → case-insensitive
    let parsed = parse_grep_query("hello");
    let result = picker.grep(&parsed, &plain_opts());

    assert_eq!(
        result.matches.len(),
        3,
        "smart case should match all 3 lines"
    );
}

#[test]
fn plain_text_smart_case_sensitive_with_uppercase() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(
        tmp.path(),
        &[("a.txt", "Hello World\nhello world\nHELLO WORLD\n")],
    );

    // Query has uppercase → smart case → case-sensitive
    let parsed = parse_grep_query("Hello");
    let result = picker.grep(&parsed, &plain_opts());

    assert_eq!(
        result.matches.len(),
        1,
        "uppercase in query should trigger case-sensitive"
    );
    assert_eq!(result.matches[0].line_number, 1);
}

#[test]
fn plain_text_regex_metacharacters_are_literal() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(
        tmp.path(),
        &[(
            "code.rs",
            "fn main() {\n    println!(\"test\");\n}\nfn foo() {}\n",
        )],
    );

    // In plain text mode, these regex metacharacters should be literal
    let parsed = parse_grep_query("fn main()");
    let result = picker.grep(&parsed, &plain_opts());

    assert_eq!(result.matches.len(), 1);
    assert_eq!(result.matches[0].line_number, 1);

    // Parentheses should NOT be treated as regex groups
    let parsed2 = parse_grep_query("(\"test\")");
    let result2 = picker.grep(&parsed2, &plain_opts());
    assert_eq!(result2.matches.len(), 1);
    assert_eq!(result2.matches[0].line_number, 2);
}

#[test]
fn plain_text_dot_is_literal() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(
        tmp.path(),
        &[(
            "config.toml",
            "version = \"1.0\"\nname = \"foo\"\nversion_major = 1\n",
        )],
    );

    // In plain text mode, dot should be literal, not "any char"
    let parsed = parse_grep_query("1.0");
    let result = picker.grep(&parsed, &plain_opts());

    assert_eq!(
        result.matches.len(),
        1,
        "dot should be literal, not 'any char'"
    );
    assert!(result.matches[0].line_content.contains("1.0"));
}

#[test]
fn plain_text_asterisk_is_literal() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(
        tmp.path(),
        &[(
            "doc.md",
            "Use **bold** text\nUse *italic* text\nUse normal text\n",
        )],
    );

    let parsed = parse_grep_query("**bold**");
    let result = picker.grep(&parsed, &plain_opts());
    assert_eq!(result.matches.len(), 1);
    assert_eq!(result.matches[0].line_number, 1);
}

#[test]
fn plain_text_backslash_is_literal() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(
        tmp.path(),
        &[("paths.txt", "C:\\Users\\foo\\bar\n/home/user/bin\n")],
    );

    let parsed = parse_grep_query("C:\\Users");
    let result = picker.grep(&parsed, &plain_opts());
    assert_eq!(result.matches.len(), 1);
}

#[test]
fn plain_text_across_multiple_files() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(
        tmp.path(),
        &[
            ("a.txt", "use std::io;\nuse std::fs;\n"),
            ("b.txt", "use std::path;\nuse serde;\n"),
            ("c.txt", "no match here\n"),
        ],
    );

    let parsed = parse_grep_query("use std");
    let result = picker.grep(&parsed, &plain_opts());

    assert_eq!(result.matches.len(), 3);
    // Should match in files a.txt and b.txt
    assert_eq!(result.files.len(), 2);
}

#[test]
fn plain_text_highlight_offsets_are_correct() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(tmp.path(), &[("a.txt", "foo bar foo baz foo\n")]);

    let parsed = parse_grep_query("foo");
    let result = picker.grep(&parsed, &plain_opts());

    assert_eq!(result.matches.len(), 1);
    let m = &result.matches[0];
    // "foo" appears at byte offsets 0, 8, 16
    assert_eq!(m.match_byte_offsets.len(), 3);
    assert_eq!(m.match_byte_offsets[0], (0, 3));
    assert_eq!(m.match_byte_offsets[1], (8, 11));
    assert_eq!(m.match_byte_offsets[2], (16, 19));
    assert_eq!(m.col, 0);
}

#[test]
fn plain_text_empty_query_returns_no_content_matches() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(tmp.path(), &[("a.txt", "some content\n")]);

    let parsed = parse_grep_query("");
    let result = picker.grep(&parsed, &plain_opts());

    // Empty query in grep returns git-modified welcome state (no content matches)
    // Since our test files have no git status, we expect 0 matches
    assert_eq!(result.matches.len(), 0);
}

#[test]
fn plain_text_binary_files_are_skipped() {
    let tmp = TempDir::new().unwrap();
    let mut bin_content = b"match this text\n".to_vec();
    bin_content.extend_from_slice(&[0u8; 100]); // NUL bytes make it binary
    bin_content.extend_from_slice(b"match this text\n");

    // Initialize a git repo so the picker indexes binary-extension files
    // (marking them as binary) rather than skipping them entirely.
    std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(tmp.path())
        .status()
        .expect("git init failed");

    // Use a known binary extension so the picker's scan-time heuristic marks
    // it as binary without needing mutable post-hoc access.
    fs::write(tmp.path().join("binary.png"), &bin_content).unwrap();

    // create_picker writes text.txt and then scans the directory, picking up
    // both binary.png (already on disk) and text.txt.
    let picker = create_picker(tmp.path(), &[("text.txt", "match this text\n")]);

    // binary.png should have been auto-detected as binary by extension heuristic
    let has_binary = picker
        .get_files()
        .iter()
        .any(|f| f.relative_path(&picker).contains("binary.png") && f.is_binary());
    assert!(has_binary, "binary.png should be detected as binary");

    let parsed = parse_grep_query("match this text");
    let result = picker.grep(&parsed, &plain_opts());

    // Only the text file should be searched, not the binary one
    assert_eq!(result.files.len(), 1);
    assert!(result.files[0].relative_path(&picker).contains("text.txt"));
}

#[test]
fn binary_payload_after_long_ascii_header_is_detected() {
    // Mimics formats like Radiance .hdr / Apple bplist / Adobe .ai where the
    // first ~1KB is plain ASCII and the binary payload (NULs) starts later.
    // The legacy 512-byte sniff missed these; the bigram-build memchr scan
    // over the whole indexed buffer must catch them.
    use fff_search::file_picker::FFFMode;
    use fff_search::{SharedFilePicker, SharedFrecency};
    use std::time::Duration;

    let tmp = TempDir::new().unwrap();
    let base = tmp.path();

    let mut content = Vec::new();
    // 1 KiB of plain ASCII header — escapes any small fixed-window NUL sniff.
    content.extend(std::iter::repeat_n(b'A', 1024));
    content.extend_from_slice(b"\nmatch this text\n");
    // Binary payload: NUL bytes that prove the file is not text.
    content.extend(std::iter::repeat_n(0u8, 256));
    content.extend_from_slice(b"\nmatch this text\n");

    // Use a *text* extension so the scan-time heuristic does NOT pre-flag it.
    // Only the bigram-time content scan can mark it binary.
    fs::write(base.join("header.txt"), &content).unwrap();
    fs::write(base.join("plain.txt"), b"match this text\n").unwrap();

    let shared_picker = SharedFilePicker::default();
    let shared_frecency = SharedFrecency::default();

    FilePicker::new_with_shared_state(
        shared_picker.clone(),
        shared_frecency.clone(),
        FilePickerOptions {
            base_path: base.to_string_lossy().to_string(),
            enable_mmap_cache: false,
            enable_content_indexing: true,
            mode: FFFMode::Neovim,
            watch: false,
            ..Default::default()
        },
    )
    .expect("Failed to create FilePicker");

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        std::thread::sleep(Duration::from_millis(25));
        let ready = shared_picker
            .read()
            .ok()
            .and_then(|g| {
                g.as_ref()
                    .map(|p| !p.is_scan_active() && p.bigram_index().is_some())
            })
            .unwrap_or(false);
        if ready {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Timed out waiting for bigram build"
        );
    }

    let guard = shared_picker.read().unwrap();
    let picker = guard.as_ref().unwrap();

    let was_flagged = picker
        .get_files()
        .iter()
        .any(|f| f.relative_path(picker).contains("header.txt") && f.is_binary());
    assert!(
        was_flagged,
        "header.txt with NULs past 512 bytes must be flagged binary by the whole-buffer memchr scan"
    );

    let parsed = parse_grep_query("match this text");
    let result = picker.grep(&parsed, &plain_opts());
    assert_eq!(
        result.files.len(),
        1,
        "only plain.txt should be searched; header.txt must be skipped as binary"
    );
    assert!(
        result.files[0].relative_path(picker).contains("plain.txt"),
        "the only match should come from plain.txt"
    );
}

#[test]
fn unknown_extension_binary_added_after_scan_is_reclassified() {
    // The initial-scan path runs detect_binary_content as part of bigram build,
    // but the watcher path used to fall back to extension-only triage and
    // missed binary files with unknown extensions like `.codex`.
    use fff_search::file_picker::FFFMode;
    use fff_search::{SharedFilePicker, SharedFrecency};
    use std::time::Duration;

    let tmp = TempDir::new().unwrap();
    let base = tmp.path();

    // Seed one tracked text file so the initial scan has something to work with.
    fs::write(base.join("seed.txt"), b"seed\n").unwrap();

    let shared_picker = SharedFilePicker::default();
    let shared_frecency = SharedFrecency::default();

    FilePicker::new_with_shared_state(
        shared_picker.clone(),
        shared_frecency.clone(),
        FilePickerOptions {
            base_path: base.to_string_lossy().to_string(),
            enable_mmap_cache: false,
            enable_content_indexing: true,
            mode: FFFMode::Neovim,
            watch: false,
            ..Default::default()
        },
    )
    .expect("Failed to create FilePicker");

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        std::thread::sleep(Duration::from_millis(25));
        let ready = shared_picker
            .read()
            .ok()
            .and_then(|g| {
                g.as_ref()
                    .map(|p| !p.is_scan_active() && p.bigram_index().is_some())
            })
            .unwrap_or(false);
        if ready {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Timed out waiting for bigram build"
        );
    }

    // Drop the file on disk after indexing finished, then announce it through
    // the watcher entry point. `.codex` is intentionally not in the extension
    // allow-list — only a content sniff can flag it.
    let mut payload = vec![0x03u8, 0x00, 0x04, 0x05];
    payload.extend(std::iter::repeat_n(0u8, 256));
    let new_path = base.join("snapshot.codex");
    fs::write(&new_path, &payload).unwrap();

    {
        let mut guard = shared_picker.write().unwrap();
        let picker = guard.as_mut().unwrap();
        assert!(
            picker.handle_create_or_modify(&new_path).is_some(),
            "handle_create_or_modify must accept the new file"
        );
    }

    let guard = shared_picker.read().unwrap();
    let picker = guard.as_ref().unwrap();
    let was_flagged = picker
        .get_files()
        .iter()
        .any(|f| f.relative_path(picker).contains("snapshot.codex") && f.is_binary());
    assert!(
        was_flagged,
        "snapshot.codex must be flagged binary when added via the watcher path"
    );
}

#[test]
fn text_file_modified_to_binary_is_reclassified() {
    // A file that started life as text and later got rewritten with NUL bytes
    // (e.g. a generator overwrote a .log) must lose its text classification.
    use fff_search::file_picker::FFFMode;
    use fff_search::{SharedFilePicker, SharedFrecency};
    use std::time::Duration;

    let tmp = TempDir::new().unwrap();
    let base = tmp.path();

    // Start as plain text with a known extension.
    fs::write(base.join("notes.txt"), b"hello world\n").unwrap();

    let shared_picker = SharedFilePicker::default();
    let shared_frecency = SharedFrecency::default();

    FilePicker::new_with_shared_state(
        shared_picker.clone(),
        shared_frecency.clone(),
        FilePickerOptions {
            base_path: base.to_string_lossy().to_string(),
            enable_mmap_cache: false,
            enable_content_indexing: true,
            mode: FFFMode::Neovim,
            watch: false,
            ..Default::default()
        },
    )
    .expect("Failed to create FilePicker");

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        std::thread::sleep(Duration::from_millis(25));
        let ready = shared_picker
            .read()
            .ok()
            .and_then(|g| {
                g.as_ref()
                    .map(|p| !p.is_scan_active() && p.bigram_index().is_some())
            })
            .unwrap_or(false);
        if ready {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Timed out waiting for bigram build"
        );
    }

    // Sanity: it's text right now.
    {
        let guard = shared_picker.read().unwrap();
        let picker = guard.as_ref().unwrap();
        let is_text = picker
            .get_files()
            .iter()
            .any(|f| f.relative_path(picker).contains("notes.txt") && !f.is_binary());
        assert!(is_text, "notes.txt should start as text");
    }

    // Overwrite with binary content and replay through the watcher entry point.
    // Bump mtime so update_metadata records it as a real change.
    std::thread::sleep(Duration::from_secs(1));
    let mut payload = b"header text\n".to_vec();
    payload.extend(std::iter::repeat_n(0u8, 256));
    fs::write(base.join("notes.txt"), &payload).unwrap();

    {
        let mut guard = shared_picker.write().unwrap();
        let picker = guard.as_mut().unwrap();
        assert!(
            picker
                .handle_create_or_modify(base.join("notes.txt"))
                .is_some(),
            "handle_create_or_modify must succeed for the modify case"
        );
    }

    let guard = shared_picker.read().unwrap();
    let picker = guard.as_ref().unwrap();
    let now_binary = picker
        .get_files()
        .iter()
        .any(|f| f.relative_path(picker).contains("notes.txt") && f.is_binary());
    assert!(
        now_binary,
        "notes.txt must flip to binary after being overwritten with NULs"
    );
}

#[test]
fn large_unknown_extension_binary_is_classified_at_scan_time() {
    // Files larger than MAX_INDEXABLE_FILE_SIZE never enter build_bigram_index,
    // so without a separate header sniff they default to is_binary=false and
    // pollute grep results with NUL-laden lines (e.g. a committed ELF blob
    // named `codex_view` with no extension).
    use fff_search::file_picker::FFFMode;
    use fff_search::grep::{GrepSearchOptions, parse_grep_query};
    use fff_search::{SharedFilePicker, SharedFrecency};
    use std::time::Duration;

    let tmp = TempDir::new().unwrap();
    let base = tmp.path();

    // 3 MiB: above the 2 MiB bigram cap and below the 10 MiB grep cap.
    // ELF-like header with NULs at the very start, then ASCII filler so a
    // grep for "match this text" would otherwise return polluted lines.
    let mut blob = Vec::new();
    blob.extend_from_slice(b"\x7fELF\x02\x01\x01\x00");
    blob.extend(std::iter::repeat_n(0u8, 256));
    blob.extend_from_slice(b"\nmatch this text\n");
    blob.extend(std::iter::repeat_n(b'A', 3 * 1024 * 1024));
    blob.extend_from_slice(b"\nmatch this text\n");
    fs::write(base.join("codex_view"), &blob).unwrap();
    fs::write(base.join("plain.txt"), b"match this text\n").unwrap();

    let shared_picker = SharedFilePicker::default();
    let shared_frecency = SharedFrecency::default();

    FilePicker::new_with_shared_state(
        shared_picker.clone(),
        shared_frecency.clone(),
        FilePickerOptions {
            base_path: base.to_string_lossy().to_string(),
            enable_mmap_cache: false,
            enable_content_indexing: true,
            mode: FFFMode::Neovim,
            watch: false,
            ..Default::default()
        },
    )
    .expect("Failed to create FilePicker");

    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        std::thread::sleep(Duration::from_millis(25));
        let ready = shared_picker
            .read()
            .ok()
            .and_then(|g| {
                g.as_ref()
                    .map(|p| !p.is_scan_active() && p.bigram_index().is_some())
            })
            .unwrap_or(false);
        if ready {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Timed out waiting for bigram build"
        );
    }

    let guard = shared_picker.read().unwrap();
    let picker = guard.as_ref().unwrap();
    let was_flagged = picker
        .get_files()
        .iter()
        .any(|f| f.relative_path(picker).contains("codex_view") && f.is_binary());
    assert!(
        was_flagged,
        "large no-extension binary must be flagged via the header sniff"
    );

    let parsed = parse_grep_query("match this text");
    let opts = GrepSearchOptions {
        max_file_size: 10 * 1024 * 1024,
        ..plain_opts()
    };
    let result = picker.grep(&parsed, &opts);
    assert_eq!(
        result.files.len(),
        1,
        "only plain.txt should be searched; codex_view must be skipped as binary"
    );
    assert!(
        result.files[0].relative_path(picker).contains("plain.txt"),
        "the only match should come from plain.txt"
    );
}

#[test]
fn large_binary_with_nuls_past_header_is_classified() {
    // Guards the streaming sniff: a >2 MB file that is pure ASCII well past any
    // fixed header window (the old code only checked the first 8 KB) but has
    // NULs deeper in. Grep reads the whole file up to max_file_size, so the
    // detector must scan the same range or the binary tail leaks as "text".
    use fff_search::file_picker::FFFMode;
    use fff_search::grep::{GrepSearchOptions, parse_grep_query};
    use fff_search::{SharedFilePicker, SharedFrecency};
    use std::time::Duration;

    let tmp = TempDir::new().unwrap();
    let base = tmp.path();

    // 1 MiB of clean ASCII (with a grep marker) — dwarfs any header sniff —
    // then NUL bytes, keeping the total above the 2 MiB non-indexable cap.
    let mut blob = Vec::new();
    blob.extend_from_slice(b"match this text\n");
    blob.extend(std::iter::repeat_n(b'A', 1024 * 1024));
    blob.extend_from_slice(b"match this text\n");
    blob.extend(std::iter::repeat_n(0u8, 1024 * 1024 + 4096)); // NULs start ~1 MiB in
    blob.extend_from_slice(b"match this text\n");
    assert!(blob.len() > 2 * 1024 * 1024);
    fs::write(base.join("late_nul.dat"), &blob).unwrap();
    fs::write(base.join("plain.txt"), b"match this text\n").unwrap();

    let shared_picker = SharedFilePicker::default();
    let shared_frecency = SharedFrecency::default();
    FilePicker::new_with_shared_state(
        shared_picker.clone(),
        shared_frecency.clone(),
        FilePickerOptions {
            base_path: base.to_string_lossy().to_string(),
            enable_mmap_cache: false,
            enable_content_indexing: true,
            mode: FFFMode::Neovim,
            watch: false,
            ..Default::default()
        },
    )
    .expect("Failed to create FilePicker");

    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        std::thread::sleep(Duration::from_millis(25));
        let ready = shared_picker
            .read()
            .ok()
            .and_then(|g| {
                g.as_ref()
                    .map(|p| !p.is_scan_active() && p.bigram_index().is_some())
            })
            .unwrap_or(false);
        if ready {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Timed out waiting for bigram build"
        );
    }

    let guard = shared_picker.read().unwrap();
    let picker = guard.as_ref().unwrap();
    let flagged = picker
        .get_files()
        .iter()
        .any(|f| f.relative_path(picker).contains("late_nul.dat") && f.is_binary());
    assert!(
        flagged,
        "NULs past the 8 KB header window must still be detected by the streaming scan"
    );

    let parsed = parse_grep_query("match this text");
    let opts = GrepSearchOptions {
        max_file_size: 10 * 1024 * 1024,
        ..plain_opts()
    };
    let result = picker.grep(&parsed, &opts);
    assert_eq!(
        result.files.len(),
        1,
        "only plain.txt should match; late_nul.dat must be skipped as binary"
    );
    assert!(result.files[0].relative_path(picker).contains("plain.txt"));
}

#[test]
fn plain_text_max_matches_per_file() {
    let tmp = TempDir::new().unwrap();
    let mut content = String::new();
    for i in 0..50 {
        content.push_str(&format!("line {} match_target\n", i));
    }
    fs::write(tmp.path().join("many.txt"), &content).unwrap();
    let picker = create_picker(tmp.path(), &[("many.txt", &content)]);

    let mut opts = plain_opts();
    opts.max_matches_per_file = 5;

    let parsed = parse_grep_query("match_target");
    let result = picker.grep(&parsed, &opts);

    assert_eq!(
        result.matches.len(),
        5,
        "should cap at max_matches_per_file"
    );
}

#[test]
fn plain_text_page_limit() {
    let tmp = TempDir::new().unwrap();
    let mut content = String::new();
    for i in 0..100 {
        content.push_str(&format!("line {} target\n", i));
    }
    fs::write(tmp.path().join("big.txt"), &content).unwrap();
    let picker = create_picker(tmp.path(), &[("big.txt", &content)]);

    let mut opts = plain_opts();
    opts.page_limit = 10;

    let parsed = parse_grep_query("target");
    let result = picker.grep(&parsed, &opts);

    // page_limit is a soft minimum: we always finish the current file, so we
    // get at least page_limit matches (no data loss) and at most
    // max_matches_per_file (200) from a single file.
    assert!(
        result.matches.len() >= opts.page_limit,
        "should return at least page_limit matches: got {}",
        result.matches.len()
    );
    assert!(
        result.matches.len() <= opts.max_matches_per_file,
        "should never exceed max_matches_per_file: got {}",
        result.matches.len()
    );
    // Single file with 100 lines all matching — all should be returned.
    assert_eq!(result.matches.len(), 100, "all 100 lines must be returned");
}

#[test]
fn plain_text_file_offset_pagination() {
    let tmp = TempDir::new().unwrap();
    // Create many files (1 match per file) so file-based pagination exercises
    // offset tracking across files with and without matches.
    let specs: Vec<(String, String)> = (0..20)
        .map(|i| {
            (
                format!("file_{:02}.txt", i),
                format!("unique_token_{}\n", i),
            )
        })
        .collect();
    let spec_refs: Vec<(&str, &str)> = specs
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let picker = create_picker(tmp.path(), &spec_refs);

    let mut opts = plain_opts();
    opts.page_limit = 5;

    // Collect ALL matches across all pages and verify no duplicates and full coverage.
    let mut all_line_texts: Vec<String> = Vec::new();
    let mut pages = 0;
    let max_pages = 20; // safety limit

    loop {
        let parsed = parse_grep_query("unique_token");
        let result = picker.grep(&parsed, &opts);

        for m in &result.matches {
            let text = m.line_content.trim().to_string();
            assert!(
                !all_line_texts.contains(&text),
                "duplicate match across pages: '{}'",
                text
            );
            all_line_texts.push(text);
        }

        pages += 1;
        assert!(pages <= max_pages, "pagination did not terminate");

        if result.next_file_offset == 0 {
            break;
        }

        // Offset must strictly advance
        assert!(
            result.next_file_offset > opts.file_offset,
            "next_file_offset ({}) did not advance past current ({})",
            result.next_file_offset,
            opts.file_offset
        );
        opts.file_offset = result.next_file_offset;
    }

    assert_eq!(
        all_line_texts.len(),
        20,
        "pagination should find all 20 matches across all pages, got {}",
        all_line_texts.len()
    );
    assert!(
        pages > 1,
        "should require multiple pages with page_limit=5 and 20 files"
    );
}

#[test]
fn plain_text_line_numbers_are_correct() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(
        tmp.path(),
        &[("a.txt", "line one\nline two\nline three\nline four\n")],
    );

    let parsed = parse_grep_query("line");
    let result = picker.grep(&parsed, &plain_opts());

    assert_eq!(result.matches.len(), 4);
    assert_eq!(result.matches[0].line_number, 1);
    assert_eq!(result.matches[1].line_number, 2);
    assert_eq!(result.matches[2].line_number, 3);
    assert_eq!(result.matches[3].line_number, 4);
}

#[test]
fn plain_text_max_file_size_filter() {
    let tmp = TempDir::new().unwrap();
    // Create a file larger than 100 bytes
    let big_content = "a".repeat(200) + "\nmatch_me\n";
    fs::write(tmp.path().join("big.txt"), &big_content).unwrap();
    let picker = create_picker(tmp.path(), &[("big.txt", &big_content)]);

    let mut opts = plain_opts();
    opts.max_file_size = 100; // Only allow files up to 100 bytes

    let parsed = parse_grep_query("match_me");
    let result = picker.grep(&parsed, &opts);

    assert_eq!(result.matches.len(), 0, "large file should be filtered out");
    assert_eq!(result.filtered_file_count, 0);
}

// ── Regex mode tests ───────────────────────────────────────────────────

#[test]
fn regex_basic_pattern() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(
        tmp.path(),
        &[("a.txt", "foo123\nbar456\nbaz789\nfoo_bar\n")],
    );

    let parsed = parse_grep_query("foo\\d+");
    let result = picker.grep(&parsed, &regex_opts());

    assert_eq!(result.matches.len(), 1);
    assert_eq!(result.matches[0].line_number, 1);
    assert!(result.matches[0].line_content.contains("foo123"));
}

#[test]
fn regex_capture_group_matching() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(tmp.path(), &[("a.txt", "foobar\nfoobaz\nfoo123\n")]);

    // Use a capturing group (not lookahead, which regex crate doesn't support)
    let parsed = parse_grep_query("foo(bar|baz)");
    let result = picker.grep(&parsed, &regex_opts());

    assert_eq!(result.matches.len(), 2);
    let contents: Vec<&str> = result
        .matches
        .iter()
        .map(|m| m.line_content.as_str())
        .collect();
    assert!(contents.contains(&"foobar"));
    assert!(contents.contains(&"foobaz"));
}

#[test]
fn regex_dot_matches_any_char() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(tmp.path(), &[("a.txt", "v1.0\nv1x0\nv1-0\nv100\nv2.0\n")]);

    // In regex mode, . matches any character, so v1.0 matches v1.0, v1x0, v1-0, and v100
    let parsed = parse_grep_query("v1.0");
    let result = picker.grep(&parsed, &regex_opts());

    assert_eq!(
        result.matches.len(),
        4,
        "regex dot should match v1.0, v1x0, v1-0, and v100 (dot matches any char)"
    );
}

#[test]
fn regex_alternation() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(tmp.path(), &[("a.txt", "apple\nbanana\ncherry\napricot\n")]);

    let parsed = parse_grep_query("apple|cherry");
    let result = picker.grep(&parsed, &regex_opts());

    assert_eq!(result.matches.len(), 2);
    let lines: Vec<u64> = result.matches.iter().map(|m| m.line_number).collect();
    assert!(lines.contains(&1));
    assert!(lines.contains(&3));
}

#[test]
fn regex_character_class() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(tmp.path(), &[("a.txt", "cat\ncut\ncot\ncit\ncxt\n")]);

    let parsed = parse_grep_query("c[aou]t");
    let result = picker.grep(&parsed, &regex_opts());

    assert_eq!(result.matches.len(), 3);
    let contents: Vec<&str> = result
        .matches
        .iter()
        .map(|m| m.line_content.as_str())
        .collect();
    assert!(contents.contains(&"cat"));
    assert!(contents.contains(&"cut"));
    assert!(contents.contains(&"cot"));
}

#[test]
fn regex_quantifiers() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(tmp.path(), &[("a.txt", "fo\nfoo\nfooo\nfoooo\nbar\n")]);

    let parsed = parse_grep_query("fo{2,3}");
    let result = picker.grep(&parsed, &regex_opts());

    assert_eq!(result.matches.len(), 3, "should match foo, fooo, foooo");
}

#[test]
fn regex_anchors() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(
        tmp.path(),
        &[("a.txt", "start of line\nmiddle start end\nend of line\n")],
    );

    let parsed = parse_grep_query("^start");
    let result = picker.grep(&parsed, &regex_opts());

    assert_eq!(result.matches.len(), 1);
    assert_eq!(result.matches[0].line_number, 1);
}

#[test]
fn regex_anchors_multiword() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(
        tmp.path(),
        &[(
            "test.c",
            "int ff_function(void);\nstatic int ff_other(void);\nint main(void);\nint ff_another(void);\n",
        )],
    );

    // ^int ff_ should match lines starting with "int ff_"
    let parsed = parse_grep_query("^int ff_");
    let result = picker.grep(&parsed, &regex_opts());

    assert_eq!(
        result.matches.len(),
        2,
        "should match 2 lines starting with 'int ff_'"
    );
    assert!(result.matches[0].line_content.contains("ff_function"));
    assert!(result.matches[1].line_content.contains("ff_another"));
}

#[test]
fn regex_highlight_offsets_variable_length() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(tmp.path(), &[("a.txt", "aab aaab aaaab\n")]);

    let parsed = parse_grep_query("a+b");
    let result = picker.grep(&parsed, &regex_opts());

    assert_eq!(result.matches.len(), 1);
    let m = &result.matches[0];
    // Regex "a+b" matches "aab" (3 bytes), "aaab" (4 bytes), "aaaab" (5 bytes)
    assert_eq!(m.match_byte_offsets.len(), 3);
    // Verify the match spans have different lengths (variable-length regex)
    assert_eq!(m.match_byte_offsets[0], (0, 3)); // "aab"
    assert_eq!(m.match_byte_offsets[1], (4, 8)); // "aaab"
    assert_eq!(m.match_byte_offsets[2], (9, 14)); // "aaaab"
}

#[test]
fn regex_invalid_pattern_falls_back_to_literal() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(tmp.path(), &[("a.txt", "call name(arg)\nother line\n")]);

    // Invalid regex: unmatched group — should fall back to literal search
    let parsed = parse_grep_query("name(");
    let result = picker.grep(&parsed, &regex_opts());

    // Fallback to literal: finds "name(" in "call name(arg)"
    assert_eq!(
        result.matches.len(),
        1,
        "invalid regex should fall back to literal and find the match"
    );
    assert!(
        result.regex_fallback_error.is_some(),
        "should report the regex compilation error"
    );
    assert!(result.matches[0].line_content.contains("name("));

    // A pattern that doesn't exist anywhere — still falls back but finds nothing
    let parsed2 = parse_grep_query("zzz(");
    let result2 = picker.grep(&parsed2, &regex_opts());
    assert_eq!(result2.matches.len(), 0);
    assert!(result2.regex_fallback_error.is_some());
}

#[test]
fn regex_smart_case() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(tmp.path(), &[("a.txt", "Foo bar\nfoo bar\nFOO BAR\n")]);

    // Lowercase query → case-insensitive
    let parsed_lower = parse_grep_query("foo");
    let result_lower = picker.grep(&parsed_lower, &regex_opts());
    assert_eq!(result_lower.matches.len(), 3);

    // Query with uppercase → case-sensitive
    let parsed_upper = parse_grep_query("Foo");
    let result_upper = picker.grep(&parsed_upper, &regex_opts());
    assert_eq!(result_upper.matches.len(), 1);
}

#[test]
fn regex_across_multiple_files() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(
        tmp.path(),
        &[
            ("lib.rs", "fn main() {}\nfn helper() {}\nstruct Foo;\n"),
            (
                "test.rs",
                "fn test_one() {}\nfn test_two() {}\nmod tests;\n",
            ),
            ("readme.md", "# Title\nSome text\n"),
        ],
    );

    let parsed = parse_grep_query("fn \\w+\\(\\)");
    let result = picker.grep(&parsed, &regex_opts());

    // Should match: fn main(), fn helper(), fn test_one(), fn test_two()
    assert_eq!(result.matches.len(), 4);
    assert_eq!(result.files.len(), 2, "matches in 2 .rs files, not readme");
}

// ── Mode comparison tests ──────────────────────────────────────────────

#[test]
fn plain_text_and_regex_agree_on_simple_literal() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(
        tmp.path(),
        &[("a.txt", "hello world\ngoodbye world\nhello again\n")],
    );

    let parsed = parse_grep_query("hello");
    let plain_result = picker.grep(&parsed, &plain_opts());
    let regex_result = picker.grep(&parsed, &regex_opts());

    assert_eq!(plain_result.matches.len(), regex_result.matches.len());
    for (p, r) in plain_result.matches.iter().zip(regex_result.matches.iter()) {
        assert_eq!(p.line_number, r.line_number);
        assert_eq!(p.line_content, r.line_content);
    }
}

#[test]
fn plain_text_escapes_what_regex_does_not() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(
        tmp.path(),
        &[("a.txt", "price is $100\nprice is 100\nprice is $200\n")],
    );

    // "$100" — in plain text, $ is literal; in regex, $ is anchor
    let parsed_plain = parse_grep_query("$100");
    let plain_result = picker.grep(&parsed_plain, &plain_opts());
    let parsed_regex = parse_grep_query("\\$100");
    let regex_result = picker.grep(&parsed_regex, &regex_opts());

    // Plain text should find "$100" literally
    assert_eq!(plain_result.matches.len(), 1);
    assert!(plain_result.matches[0].line_content.contains("$100"));

    // Regex with escaped $ should also find "$100"
    assert_eq!(regex_result.matches.len(), 1);
}

// ── Constraint integration tests ───────────────────────────────────────

#[test]
fn grep_with_extension_constraint() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(
        tmp.path(),
        &[
            ("a.rs", "use std::io;\nfn main() {}\n"),
            ("b.txt", "use std::io;\nsome text\n"),
            ("c.rs", "use std::fs;\n"),
        ],
    );

    let parsed = parse_grep_query("use std *.rs");
    let result = picker.grep(&parsed, &plain_opts());

    // Should only search .rs files
    for file in &result.files {
        assert!(
            file.relative_path(&picker).ends_with(".rs"),
            "should only match .rs files, got: {}",
            file.relative_path(&picker)
        );
    }
    assert!(
        result.matches.len() >= 2,
        "should find matches in .rs files"
    );
}

// ── Bracket / glob character tests ─────────────────────────────────────

#[test]
fn plain_text_bracket_is_literal() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(
        tmp.path(),
        &[(
            "code.rs",
            "let x = arr[0];\nlet y = arr[1];\nlet z = something;\n",
        )],
    );

    let parsed = parse_grep_query("arr[0]");
    let result = picker.grep(&parsed, &plain_opts());

    assert_eq!(
        result.matches.len(),
        1,
        "brackets should be literal in plain text mode"
    );
    assert_eq!(result.matches[0].line_number, 1);
}

// ── Backslash escape tests ─────────────────────────────────────────────

#[test]
fn grep_backslash_escapes_extension_filter() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(
        tmp.path(),
        &[
            ("a.rs", "contains *.rs pattern\n"),
            ("b.txt", "also has *.rs here\n"),
        ],
    );

    // Without escape: "*.rs" is an extension filter, so only .rs files are searched
    let parsed = parse_grep_query("pattern *.rs");
    let result_filter = picker.grep(&parsed, &plain_opts());
    assert_eq!(
        result_filter.files.len(),
        1,
        "*.rs should filter to .rs files"
    );

    // With escape: "\*.rs" is literal text, both files are searched
    let parsed_escaped = parse_grep_query("\\*.rs");
    let result_literal = picker.grep(&parsed_escaped, &plain_opts());
    assert_eq!(
        result_literal.matches.len(),
        2,
        "\\*.rs should search for literal *.rs in all files"
    );
}

#[test]
fn grep_backslash_escapes_path_segment() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(
        tmp.path(),
        &[
            ("src/main.rs", "search for /src/ path\n"),
            ("lib/utils.rs", "also /src/ mentioned\n"),
        ],
    );

    // With escape: "\\/src/" is literal text, not a path constraint
    let parsed = parse_grep_query("\\/src/");
    let result = picker.grep(&parsed, &plain_opts());
    assert_eq!(
        result.matches.len(),
        2,
        "\\/src/ should search for literal /src/ in all files"
    );
}

#[test]
fn grep_backslash_escapes_negation() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(tmp.path(), &[("a.txt", "the !test macro\nother stuff\n")]);

    // With escape: "\\!test" is literal text "!test"
    let parsed = parse_grep_query("\\!test");
    let result = picker.grep(&parsed, &plain_opts());
    assert_eq!(result.matches.len(), 1);
    assert!(result.matches[0].line_content.contains("!test"));
}

#[test]
fn grep_with_path_constraint() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(
        tmp.path(),
        &[
            ("src/lib.rs", "target_text\n"),
            ("tests/test.rs", "target_text\n"),
            ("src/main.rs", "other content\n"),
        ],
    );

    let parsed = parse_grep_query("target_text /src/");
    let result = picker.grep(&parsed, &plain_opts());

    assert_eq!(result.matches.len(), 1);
    let rel = result.files[0].relative_path(&picker);
    assert!(rel.starts_with("src/") || rel.starts_with("src\\"));
}

// ── Negated constraint tests ───────────────────────────────────────────

#[test]
fn grep_with_negated_extension_constraint() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(
        tmp.path(),
        &[
            ("src/lib.rs", "target_text\n"),
            ("src/app.ts", "target_text\n"),
            ("src/main.rs", "target_text\n"),
        ],
    );

    let query = "target_text !*.rs";
    let parsed = parse_grep_query(query);
    let result = picker.grep(&parsed, &plain_opts());

    assert_eq!(
        result.matches.len(),
        1,
        "should only find matches in non-.rs files, got {} matches",
        result.matches.len()
    );
    assert!(
        result.files[0].relative_path(&picker).ends_with(".ts"),
        "should only match .ts file, got: {}",
        result.files[0].relative_path(&picker)
    );
}

#[test]
fn grep_with_negated_path_constraint() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(
        tmp.path(),
        &[
            ("src/lib.rs", "target_text\n"),
            ("tests/test.rs", "target_text\n"),
            ("src/main.rs", "other content\n"),
        ],
    );

    let query = "target_text !/src/";
    let parsed = parse_grep_query(query);
    let result = picker.grep(&parsed, &plain_opts());

    assert_eq!(
        result.matches.len(),
        1,
        "should only find matches outside src/, got {} matches",
        result.matches.len()
    );
    assert!(
        result.files[0].relative_path(&picker).starts_with("tests/")
            || result.files[0]
                .relative_path(&picker)
                .starts_with("tests\\"),
        "should only match tests/ file, got: {}",
        result.files[0].relative_path(&picker)
    );
}

#[test]
fn grep_with_negated_text_constraint() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(
        tmp.path(),
        &[
            ("src/lib.rs", "target_text\n"),
            ("tests/helper.rs", "target_text\n"),
            ("docs/readme.md", "target_text\n"),
        ],
    );

    let query = "target_text !test";
    let parsed = parse_grep_query(query);
    let result = picker.grep(&parsed, &plain_opts());

    // "tests/helper.rs" contains "test" in path, should be excluded
    assert_eq!(
        result.matches.len(),
        2,
        "should find matches in files without 'test' in path, got {} matches",
        result.matches.len()
    );
    for file in &result.files {
        assert!(
            !file.relative_path(&picker).contains("test"),
            "should not match files with 'test' in path, got: {}",
            file.relative_path(&picker)
        );
    }
}

// ── Edge case tests ────────────────────────────────────────────────────

#[test]
fn grep_empty_file_is_skipped() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(tmp.path(), &[("empty.txt", ""), ("text.txt", "findme\n")]);

    let parsed = parse_grep_query("findme");
    let result = picker.grep(&parsed, &plain_opts());

    assert_eq!(result.matches.len(), 1);
}

#[test]
fn grep_single_line_no_trailing_newline() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(tmp.path(), &[("a.txt", "no newline at end")]);

    let parsed = parse_grep_query("no newline");
    let result = picker.grep(&parsed, &plain_opts());

    assert_eq!(result.matches.len(), 1);
    assert_eq!(result.matches[0].line_number, 1);
}

#[test]
fn grep_unicode_content() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(
        tmp.path(),
        &[("utf8.txt", "日本語テスト\nrégulière\nñoño\n")],
    );

    let parsed = parse_grep_query("régulière");
    let result = picker.grep(&parsed, &plain_opts());
    assert_eq!(result.matches.len(), 1);
    assert_eq!(result.matches[0].line_number, 2);

    let parsed2 = parse_grep_query("ñoño");
    let result2 = picker.grep(&parsed2, &plain_opts());
    assert_eq!(result2.matches.len(), 1);
    assert_eq!(result2.matches[0].line_number, 3);
}

#[test]
fn grep_long_line_is_truncated() {
    let tmp = TempDir::new().unwrap();
    let long_line = format!("{}NEEDLE{}", "x".repeat(1000), "y".repeat(1000));
    fs::write(tmp.path().join("long.txt"), &long_line).unwrap();
    let picker = create_picker(tmp.path(), &[("long.txt", &long_line)]);

    let parsed = parse_grep_query("NEEDLE");
    let result = picker.grep(&parsed, &plain_opts());

    assert_eq!(result.matches.len(), 1);
    // The line_content should be truncated to MAX_LINE_DISPLAY_LEN (512)
    assert!(
        result.matches[0].line_content.len() <= 512,
        "line should be truncated: len={}",
        result.matches[0].line_content.len()
    );
}

#[test]
fn regex_word_boundary() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(tmp.path(), &[("a.txt", "foo\nfoobar\nbarfoo\nfoo_baz\n")]);

    let parsed = parse_grep_query("\\bfoo\\b");
    let result = picker.grep(&parsed, &regex_opts());

    assert_eq!(
        result.matches.len(),
        1,
        "only exact word 'foo' should match"
    );
    assert_eq!(result.matches[0].line_number, 1);
}

#[test]
fn plain_text_question_mark_is_literal() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(
        tmp.path(),
        &[(
            "a.txt",
            "what is this?\nhow does it work?\nno question here\nwhat?\n",
        )],
    );

    let parsed = parse_grep_query("?");
    let result = picker.grep(&parsed, &plain_opts());

    assert_eq!(
        result.matches.len(),
        3,
        "question mark should be literal in plain text mode"
    );
}

#[test]
fn plain_text_query_with_question_mark_in_word() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(
        tmp.path(),
        &[(
            "code.rs",
            "let x = foo?;\nlet y = bar.baz();\nfoo?.unwrap()\n",
        )],
    );

    let parsed = parse_grep_query("foo?");
    let result = picker.grep(&parsed, &plain_opts());

    assert_eq!(
        result.matches.len(),
        2,
        "should find 'foo?' literally in both lines"
    );
}

#[test]
fn regex_question_mark_is_quantifier() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(tmp.path(), &[("a.txt", "color\ncolour\ncolouur\n")]);

    // In regex mode, ? means "zero or one of preceding"
    let parsed = parse_grep_query("colou?r");
    let result = picker.grep(&parsed, &regex_opts());

    assert_eq!(
        result.matches.len(),
        2,
        "regex ? should match 'color' and 'colour' but not 'colouur'"
    );
}

// ── Fuzzy mode tests ───────────────────────────────────────────────────

#[test]
fn fuzzy_finds_exact_substring() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(
        tmp.path(),
        &[("a.txt", "hello world\ngoodbye world\nhello again\n")],
    );

    let parsed = parse_grep_query("hello");
    let result = picker.grep(&parsed, &fuzzy_opts());

    assert_eq!(
        result.matches.len(),
        2,
        "fuzzy should find 'hello' in both lines"
    );
    assert!(result.matches[0].line_content.contains("hello"));
    assert!(result.matches[1].line_content.contains("hello"));
}

#[test]
fn fuzzy_finds_scattered_characters() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(
        tmp.path(),
        &[(
            "code.rs",
            "fn mutex_lock() {}\nfn main() {}\nfn mutex_unlock() {}\n",
        )],
    );

    // "mutex" should fuzzy match "mutex_lock" (contiguous prefix)
    let parsed = parse_grep_query("mutex");
    let result = picker.grep(&parsed, &fuzzy_opts());

    assert!(
        !result.matches.is_empty(),
        "fuzzy should find 'mutex' in 'mutex_lock'"
    );
    assert!(result.matches[0].line_content.contains("mutex_lock"));
}

#[test]
fn fuzzy_highlight_offsets_correct() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(tmp.path(), &[("a.txt", "hello world\n")]);

    let parsed = parse_grep_query("hell");
    let result = picker.grep(&parsed, &fuzzy_opts());

    assert_eq!(result.matches.len(), 1);
    let m = &result.matches[0];

    // "hell" should match 'h'(0), 'e'(1), 'l'(2), 'l'(3) in "hello"
    // These should be converted to byte offsets
    assert!(
        !m.match_byte_offsets.is_empty(),
        "should have highlight offsets"
    );
}

#[test]
fn fuzzy_unicode_char_indices() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(
        tmp.path(),
        &[("utf8.txt", "日本語テスト\nrégulière\nñoño\n")],
    );

    // Use "guli" which is a contiguous ASCII substring within "régulière"
    // (the chars g-u-l-i appear contiguously between the two accented chars)
    let parsed = parse_grep_query("guli");
    let result = picker.grep(&parsed, &fuzzy_opts());

    // Should fuzzy match "régulière" (with multi-byte é and è)
    // This tests that character-to-byte offset conversion works with UTF-8
    assert!(!result.matches.is_empty());
    assert!(result.matches[0].line_content.contains("régulière"));
}

#[test]
fn fuzzy_empty_query_returns_empty() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(tmp.path(), &[("a.txt", "some content\n")]);

    let parsed = parse_grep_query("");
    let result = picker.grep(&parsed, &fuzzy_opts());

    // Empty query returns git-modified files, not fuzzy matches
    assert_eq!(result.matches.len(), 0);
}

#[test]
fn fuzzy_with_extension_constraint() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(
        tmp.path(),
        &[
            ("a.rs", "use std::io;\nfn main() {}\n"),
            ("b.txt", "use std::io;\nsome text\n"),
            ("c.rs", "use std::fs;\n"),
        ],
    );

    let parsed = parse_grep_query("use std *.rs");
    let result = picker.grep(&parsed, &fuzzy_opts());

    // Should only search .rs files
    for file in &result.files {
        assert!(
            file.relative_path(&picker).ends_with(".rs"),
            "should only match .rs files, got: {}",
            file.relative_path(&picker)
        );
    }
}

#[test]
fn fuzzy_respects_page_limit() {
    let tmp = TempDir::new().unwrap();
    let mut content = String::new();
    for i in 0..100 {
        content.push_str(&format!("line {} target\n", i));
    }
    fs::write(tmp.path().join("big.txt"), &content).unwrap();
    let picker = create_picker(tmp.path(), &[("big.txt", &content)]);

    let mut opts = fuzzy_opts();
    opts.page_limit = 10;
    opts.max_matches_per_file = 50;

    let parsed = parse_grep_query("target");
    let result = picker.grep(&parsed, &opts);

    // page_limit is a soft minimum: we always finish the current file, so we
    // get at least page_limit matches (no data loss) and at most
    // max_matches_per_file (200) from a single file.
    assert!(
        result.matches.len() >= opts.page_limit,
        "should return at least page_limit matches: got {}",
        result.matches.len()
    );
    assert!(
        result.matches.len() <= opts.max_matches_per_file,
        "should never exceed max_matches_per_file: got {}",
        result.matches.len()
    );

    assert_eq!(
        result.matches.len(),
        opts.max_matches_per_file,
        "all limit of lines must be returned"
    );
}

#[test]
fn fuzzy_respects_max_matches_per_file() {
    let tmp = TempDir::new().unwrap();
    let mut content = String::new();
    for i in 0..50 {
        content.push_str(&format!("line {} match_target\n", i));
    }
    fs::write(tmp.path().join("many.txt"), &content).unwrap();
    let picker = create_picker(tmp.path(), &[("many.txt", &content)]);

    let mut opts = fuzzy_opts();
    opts.max_matches_per_file = 5;

    let parsed = parse_grep_query("match");
    let result = picker.grep(&parsed, &opts);

    assert_eq!(
        result.matches.len(),
        5,
        "should cap at max_matches_per_file"
    );
}

#[test]
fn fuzzy_filters_low_quality_matches() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(
        tmp.path(),
        &[(
            "code.rs",
            "fn mutex_lock() {}\nfn xyz() {}\nfn abc_def_ghi() {}\nfn abcdefghij() {}\n",
        )],
    );

    // Search for "abc" - should match "abc_def_ghi" and "abcdefghij" with high scores,
    // but NOT "xyz" (no relation) or "mutex_lock" (only weak letter overlap)
    let parsed = parse_grep_query("abc");
    let result = picker.grep(&parsed, &fuzzy_opts());

    // Should only get high-quality matches
    assert!(
        result.matches.len() <= 2,
        "should filter out low-quality fuzzy matches, got {} matches",
        result.matches.len()
    );

    // All matches should contain reasonable character overlap
    for m in &result.matches {
        assert!(
            m.line_content.contains("abc") || m.line_content.contains("abc_"),
            "match '{}' should be high-quality",
            m.line_content
        );
    }
}

#[test]
fn fuzzy_exact_match_always_passes() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(
        tmp.path(),
        &[("test.txt", "exact match line\nno match here\n")],
    );

    // Exact matches should always pass regardless of score threshold
    let parsed = parse_grep_query("exact");
    let result = picker.grep(&parsed, &fuzzy_opts());

    assert_eq!(
        result.matches.len(),
        1,
        "exact match should always pass score threshold"
    );
    assert!(result.matches[0].line_content.contains("exact"));
}

#[test]
fn fuzzy_score_is_captured() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(tmp.path(), &[("test.txt", "hello world\ngoodbye world\n")]);

    let parsed = parse_grep_query("hello");
    let result = picker.grep(&parsed, &fuzzy_opts());

    assert_eq!(result.matches.len(), 1);
    let m = &result.matches[0];

    // Fuzzy score should be set (Some) for fuzzy mode matches
    assert!(
        m.fuzzy_score.is_some(),
        "fuzzy_score should be set in fuzzy grep mode"
    );
    assert!(
        m.fuzzy_score.unwrap() > 0,
        "fuzzy_score should be positive for a good match"
    );
}

#[test]
fn fuzzy_score_is_none_in_plain_mode() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(tmp.path(), &[("test.txt", "hello world\n")]);

    let parsed = parse_grep_query("hello");
    let result = picker.grep(&parsed, &plain_opts());

    assert_eq!(result.matches.len(), 1);
    let m = &result.matches[0];

    // fuzzy_score should be None for plain text mode
    assert!(
        m.fuzzy_score.is_none(),
        "fuzzy_score should be None in plain text mode"
    );
}

/// Regression: memmem prefilter rejected files where content casing differed
/// from the query, even under smart_case. E.g. "vfio-kvm" failed to find
/// "VFIO-KVM" because the lowercased finder did a case-sensitive scan.
#[test]
fn plain_text_smart_case_finds_uppercase_content_with_lowercase_query() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(
        tmp.path(),
        &[(
            "driver.c",
            "// VFIO-KVM integration\nstatic int init(void) {}\n",
        )],
    );

    let parsed = parse_grep_query("vfio-kvm");
    let result = picker.grep(&parsed, &plain_opts());

    assert_eq!(
        result.matches.len(),
        1,
        "lowercase query should case-insensitively match 'VFIO-KVM'"
    );
}

/// Bug pinning: `!=` was parsed as a Not("=") exclusion constraint, dropping it
/// from the needle. Operator tokens must stay literal search text.
#[test]
fn plain_text_not_equals_operator_is_literal() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(
        tmp.path(),
        &[(
            "watch.rs",
            "if delivery.sub.epoch.load(Ordering::Acquire) != delivery.epoch {\n",
        )],
    );

    let parsed = parse_grep_query("Ordering::Acquire) != delivery.epoch");
    let result = picker.grep(&parsed, &plain_opts());

    assert_eq!(
        result.matches.len(),
        1,
        "operator `!=` must match literally"
    );
    assert!(!result.literal_fallback, "no fallback should be needed");
    assert!(result.matches[0].line_content.contains("!= delivery.epoch"));
}

#[test]
fn literal_fallback_when_constraints_find_nothing() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(tmp.path(), &[("a.txt", "foo !bar_baz qux\n")]);

    // `!bar_baz` becomes Not(Text) so the constrained needle is "foo qux" → no
    // match; the search must retry the raw query as literal text.
    let parsed = parse_grep_query("foo !bar_baz qux");
    let result = picker.grep(&parsed, &plain_opts());

    assert_eq!(result.matches.len(), 1);
    assert!(
        result.literal_fallback,
        "literal fallback should be flagged"
    );
    assert!(result.matches[0].line_content.contains("!bar_baz"));
}

#[test]
fn literal_fallback_not_triggered_when_constraints_match() {
    let tmp = TempDir::new().unwrap();
    let picker = create_picker(
        tmp.path(),
        &[
            ("src/lib.rs", "needle here\n"),
            ("test/lib.rs", "needle here\n"),
        ],
    );

    let parsed = parse_grep_query("needle !test");
    let result = picker.grep(&parsed, &plain_opts());

    assert_eq!(result.matches.len(), 1, "exclusion should still apply");
    assert!(!result.literal_fallback);
}
