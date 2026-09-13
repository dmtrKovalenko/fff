//! Proptest-driven fuzz test against real GitHub repos with a live watcher.
//!
//! Clones real repository, runs the simulated close to real user sereies of file system ewvents and
//! verifies that fff can still find the correct files. Test cases are randomized and preserved
//! using proptest
#![cfg(stress)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use proptest::prelude::*;
use proptest::test_runner::{Config as ProptestConfig, FileFailurePersistence};

use fff_search::file_picker::{FFFMode, FilePicker, is_known_binary_extension};
use fff_search::grep::{GrepMode, GrepSearchOptions, parse_grep_query};
use fff_search::{FilePickerOptions, SharedFilePicker, SharedFrecency};

const REPO_POOL: &[(&str, &str)] = &[
    ("dmtrKovalenko/fff", "fff"),
    ("BurntSushi/ripgrep", "ripgrep"),
    ("sharkdp/fd", "fd"),
    ("ogham/exa", "exa"),
    ("casey/just", "just"),
    ("ajeetdsouza/zoxide", "zoxide"),
    ("helix-editor/helix", "helix"),
    ("astral-sh/ruff", "ruff"),
    ("biomejs/biome", "biome"),
    ("denoland/deno_lint", "deno_lint"),
    ("nickel-lang/nickel", "nickel"),
    ("typst/typst", "typst"),
    ("gleam-lang/gleam", "gleam"),
    ("pretzelhammer/rust-blog", "rust-blog"),
    ("tokio-rs/mini-redis", "mini-redis"),
];

const CACHE_DIR: &str = "/tmp/fff_fuzz_repos";
/// Fixed settle time for watcher event propagation.
const WATCHER_SETTLE: Duration = Duration::from_millis(100);
/// Maximum time to wait for watcher to process all pending events.
const CONVERGE_TIMEOUT: Duration = Duration::from_secs(30);

fn fuzz_cases() -> u32 {
    std::env::var("FFF_FUZZ_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2)
}

fn fuzz_max_ops() -> usize {
    std::env::var("FFF_FUZZ_MAX_OPS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30)
}

fn fuzz_min_ops() -> usize {
    std::env::var("FFF_FUZZ_MIN_OPS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(15)
}

fn ensure_repo_cloned(repo_url: &str, local_name: &str) -> PathBuf {
    let cache = PathBuf::from(CACHE_DIR);
    fs::create_dir_all(&cache).unwrap();
    let repo_path = cache.join(local_name);
    if repo_path.join(".git").exists() {
        return repo_path;
    }

    let full_url = format!("https://github.com/{}.git", repo_url);
    eprintln!("  Cloning {} ...", full_url);
    let out = Command::new("git")
        .args(["clone", "--depth=1", "--single-branch", &full_url])
        .arg(&repo_path)
        .output()
        .expect("git clone failed");
    assert!(
        out.status.success(),
        "git clone {} failed: {}",
        full_url,
        String::from_utf8_lossy(&out.stderr)
    );
    repo_path
}

fn copy_repo_to_workdir(cached: &Path, workdir: &Path) {
    let out = Command::new("cp")
        .args(["-r"])
        .arg(cached)
        .arg(workdir)
        .output()
        .expect("cp -r failed");
    assert!(
        out.status.success(),
        "cp -r failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn collect_text_files(base: &Path) -> Vec<PathBuf> {
    // Use `git ls-files` without --cached to get only files that are both
    // tracked AND not gitignored. Files like Cargo.lock that are committed
    // but in .gitignore would appear with --cached but the fff picker skips
    // them during walk (respects .gitignore), causing false test failures.
    let out = Command::new("git")
        .args(["ls-files", "--others", "--exclude-standard", "-z"])
        .current_dir(base)
        .output()
        .unwrap();

    // Get tracked files that aren't ignored
    let tracked = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(base)
        .output()
        .unwrap();

    // Check which tracked files are actually ignored
    let ignored_check = Command::new("git")
        .args(["check-ignore", "--stdin", "-z"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .current_dir(base)
        .spawn();

    let mut ignored_set: std::collections::HashSet<String> = std::collections::HashSet::new();
    if let Ok(mut child) = ignored_check {
        use std::io::Write;
        if let Some(ref mut stdin) = child.stdin {
            let _ = stdin.write_all(&tracked.stdout);
        }
        if let Ok(output) = child.wait_with_output() {
            for path in output.stdout.split(|&b| b == 0) {
                if !path.is_empty() {
                    if let Ok(s) = std::str::from_utf8(path) {
                        ignored_set.insert(s.to_string());
                    }
                }
            }
        }
    }

    // Combine: tracked non-ignored non-binary files
    let mut files: Vec<PathBuf> = Vec::new();
    for path in tracked.stdout.split(|&b| b == 0) {
        if path.is_empty() {
            continue;
        }
        let Ok(s) = std::str::from_utf8(path) else {
            continue;
        };
        if ignored_set.contains(s) {
            continue;
        }
        let full = base.join(s);
        if full.is_file() && !is_known_binary_extension(&full) {
            files.push(full);
        }
    }
    files
}

/// Edit a file by injecting a marker line at a deterministic position,
/// preserving the rest of the content. Returns the original line that was
/// replaced so it can be restored on revert.
fn inject_marker(path: &Path, marker: &str, seed: u32) -> Option<String> {
    let content = fs::read_to_string(path).ok()?;
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        fs::write(path, format!("// {marker}\n")).ok()?;
        return Some(String::new());
    }

    // Pick a stable line position based on seed and file length
    let line_idx = seed as usize % lines.len();
    let original_line = lines[line_idx].to_string();

    let mut result = String::with_capacity(content.len() + marker.len() + 10);
    for (i, line) in lines.iter().enumerate() {
        if i == line_idx {
            result.push_str(&format!("// {marker}"));
        } else {
            result.push_str(line);
        }
        result.push('\n');
    }
    fs::write(path, &result).ok()?;
    Some(original_line)
}

/// Revert a file by restoring the original line at the same position
/// where inject_marker placed the marker.
fn revert_marker(path: &Path, marker: &str, original_line: &str) {
    let Ok(content) = fs::read_to_string(path) else {
        return;
    };
    let marker_line = format!("// {marker}");
    let result: String = content
        .lines()
        .map(|l| if l == marker_line { original_line } else { l })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    let _ = fs::write(path, result);
}

fn grep_opts(mode: GrepMode) -> GrepSearchOptions {
    GrepSearchOptions {
        max_file_size: 10 * 1024 * 1024,
        max_matches_per_file: 200,
        smart_case: true,
        file_offset: 0,
        page_limit: 500,
        mode,
        time_budget_ms: 5000,
        before_context: 0,
        after_context: 0,
        classify_definitions: false,
        trim_whitespace: false,
        abort_signal: None,
    }
}

fn grep_finds(picker: &FilePicker, query: &str, mode: GrepMode) -> bool {
    let parsed = parse_grep_query(query);
    let result = picker.grep(&parsed, &grep_opts(mode));
    !result.matches.is_empty()
}

fn grep_file_list(picker: &FilePicker, query: &str, mode: GrepMode) -> Vec<String> {
    let parsed = parse_grep_query(query);
    let result = picker.grep(&parsed, &grep_opts(mode));
    result
        .files
        .iter()
        .map(|f| f.relative_path(picker))
        .collect()
}

// ═══════════════════════════════════════════════════════════════════════════
// Infrastructure
// ═══════════════════════════════════════════════════════════════════════════

fn wait_for_bigram(sp: &SharedFilePicker) {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        std::thread::sleep(Duration::from_millis(50));
        let ready = sp
            .read()
            .ok()
            .map(|g| {
                g.as_ref()
                    .map_or(false, |p| !p.is_scan_active() && p.bigram_index().is_some())
            })
            .unwrap_or(false);
        if ready {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "Timed out waiting for bigram index"
        );
    }
}

fn epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

struct TrackedFile {
    relative: String,
    marker: String,
    /// The original line content that was replaced, for revert
    original_line: String,
    is_created: bool,
    last_write_sec: u64,
}

fn run_scenario(ops: &[Op]) {
    // Stream fff logs at info+ level by default. Override with RUST_LOG.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,fff_search=info")),
        )
        .with_test_writer()
        .try_init();

    // Allow forcing a specific repo via env for reproduction
    let repo_idx = std::env::var("FFF_FUZZ_REPO_IDX")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or_else(|| ops.len() % REPO_POOL.len());
    let (repo_url, local_name) = REPO_POOL[repo_idx];
    eprintln!("=== fuzz_real_repos: repo={repo_url} ops={} ===", ops.len());

    let scenario_start = Instant::now();
    let cached = ensure_repo_cloned(repo_url, local_name);
    let tmp = tempfile::TempDir::new().unwrap();
    let workdir = tmp.path().join(local_name);
    copy_repo_to_workdir(&cached, &workdir);

    // Ensure target/ is gitignored
    let gitignore = workdir.join(".gitignore");
    let mut gi = fs::read_to_string(&gitignore).unwrap_or_default();
    if !gi.contains("target/") {
        gi.push_str("\ntarget/\n");
        fs::write(&gitignore, &gi).unwrap();
    }

    let shared_picker = SharedFilePicker::default();
    FilePicker::new_with_shared_state(
        shared_picker.clone(),
        SharedFrecency::noop(),
        FilePickerOptions {
            base_path: workdir.to_string_lossy().to_string(),
            enable_mmap_cache: true,
            enable_content_indexing: true,
            watch: true,
            mode: FFFMode::Neovim,
            ..Default::default()
        },
    )
    .expect("FilePicker init");

    let t0 = Instant::now();
    wait_for_bigram(&shared_picker);
    let bigram_ms = t0.elapsed().as_secs_f64() * 1000.0;

    {
        let guard = shared_picker.read().unwrap();
        let picker = guard.as_ref().unwrap();
        let file_count = picker.get_files().len();
        eprintln!("  indexed {file_count} files, bigram ready in {bigram_ms:.0}ms");
    }

    // Advance mtime past scan timestamp
    std::thread::sleep(Duration::from_millis(1100));

    let mut tracked: Vec<TrackedFile> = Vec::new();
    let mut dead_markers: Vec<String> = Vec::new();
    let mut ignored_markers: Vec<String> = Vec::new();
    let mut ops_since_verify: usize = 0;
    let mut text_files: Option<Vec<PathBuf>> = None;

    for (op_idx, op) in ops.iter().enumerate() {
        match op {
            Op::CreateFile { seed } => {
                let name = format!("fff_fuzz_new_{seed:08x}.rs");
                let marker = format!("FFF_FUZZ_NEW_{seed:08x}");
                // Marker appears only once on its own line
                let content = format!("// {marker}\nfn placeholder() {{}}\n");
                fs::write(workdir.join(&name), content).unwrap();
                tracked.push(TrackedFile {
                    relative: name,
                    marker,
                    original_line: String::new(),
                    is_created: true,
                    last_write_sec: epoch_secs(),
                });
                ops_since_verify += 1;
            }
            Op::EditTracked { seed } => {
                if tracked.is_empty() {
                    continue;
                }
                let idx = *seed as usize % tracked.len();
                if tracked[idx].last_write_sec >= epoch_secs() {
                    std::thread::sleep(Duration::from_millis(1100));
                }
                let new_marker = format!("FFF_FUZZ_EDIT_{seed:08x}");
                let path = workdir.join(&tracked[idx].relative);
                // Replace the line containing our old marker with the new one
                let old_marker_line = format!("// {}", tracked[idx].marker);
                let content = fs::read_to_string(&path).unwrap_or_default();
                let new_content = content
                    .lines()
                    .map(|l| {
                        if l == old_marker_line {
                            format!("// {new_marker}")
                        } else {
                            l.to_string()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
                    + "\n";
                fs::write(&path, new_content).unwrap();

                dead_markers.push(tracked[idx].marker.clone());
                tracked[idx].marker = new_marker;
                tracked[idx].last_write_sec = epoch_secs();
                ops_since_verify += 1;
            }
            Op::EditRandom { seed } => {
                let files = text_files.get_or_insert_with(|| collect_text_files(&workdir));
                if files.is_empty() {
                    continue;
                }
                let target = &files[*seed as usize % files.len()];
                let relative = target
                    .strip_prefix(&workdir)
                    .unwrap()
                    .to_string_lossy()
                    .to_string();

                if let Some(t) = tracked.iter().find(|t| t.relative == relative) {
                    if t.last_write_sec >= epoch_secs() {
                        std::thread::sleep(Duration::from_millis(1100));
                    }
                }

                let marker = format!("FFF_FUZZ_RAND_{seed:08x}");

                // If already tracked, replace old marker line
                if let Some(pos) = tracked.iter().position(|t| t.relative == relative) {
                    let old_marker_line = format!("// {}", tracked[pos].marker);
                    let content = fs::read_to_string(target).unwrap_or_default();
                    let new_content = content
                        .lines()
                        .map(|l| {
                            if l == old_marker_line {
                                format!("// {marker}")
                            } else {
                                l.to_string()
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                        + "\n";
                    fs::write(target, new_content).unwrap();
                    dead_markers.push(tracked[pos].marker.clone());
                    tracked[pos].marker = marker;
                    tracked[pos].last_write_sec = epoch_secs();
                } else {
                    // First edit: inject marker at a deterministic line
                    let original = inject_marker(target, &marker, *seed).unwrap_or_default();
                    tracked.push(TrackedFile {
                        relative,
                        marker,
                        original_line: original,
                        is_created: false,
                        last_write_sec: epoch_secs(),
                    });
                }
                ops_since_verify += 1;
            }
            Op::DeleteTracked => {
                if tracked.is_empty() {
                    continue;
                }
                let removed = tracked.swap_remove(0);
                let abs = workdir.join(&removed.relative);
                if abs.exists() {
                    if removed.is_created {
                        fs::remove_file(&abs).ok();
                    } else {
                        let _ = Command::new("git")
                            .args(["rm", "-f", &removed.relative])
                            .current_dir(&workdir)
                            .output();
                    }
                }
                dead_markers.push(removed.marker);
                text_files = None; // invalidate cache after deletion
                ops_since_verify += 1;
            }
            Op::RevertTracked => {
                // Revert a non-created tracked file using `git checkout`
                // (restores original content, marker disappears)
                let revertable = tracked.iter().position(|t| !t.is_created);
                let Some(idx) = revertable else { continue };

                if tracked[idx].last_write_sec >= epoch_secs() {
                    std::thread::sleep(Duration::from_millis(1100));
                }

                let _ = Command::new("git")
                    .args(["checkout", "--", &tracked[idx].relative])
                    .current_dir(&workdir)
                    .output();

                let reverted = tracked.swap_remove(idx);
                dead_markers.push(reverted.marker);
                text_files = None; // invalidate cache after revert
                ops_since_verify += 1;
            }
            Op::IgnoredBurst { count, seed } => {
                let dir = workdir.join("target/debug/build");
                fs::create_dir_all(&dir).unwrap();
                for i in 0..*count {
                    let marker = format!("FFF_IGN_{seed:08x}_{i}");
                    fs::write(
                        dir.join(format!("ign_{seed:08x}_{i}.rs")),
                        format!("// {marker}\nfn {marker}() {{}}\n"),
                    )
                    .unwrap();
                    ignored_markers.push(marker);
                }
                ops_since_verify += 1;
            }
            Op::Verify => {
                if tracked.is_empty() && dead_markers.is_empty() {
                    continue;
                }

                // Poll until the watcher has propagated all pending events:
                // all live markers findable, all dead markers gone, no ignored leaks.
                let modes = [
                    (GrepMode::PlainText, "Plain"),
                    (GrepMode::Regex, "Regex"),
                    (GrepMode::Fuzzy, "Fuzzy"),
                ];
                let (mode, mode_name) = modes[op_idx % modes.len()];

                let deadline = Instant::now() + CONVERGE_TIMEOUT;
                let mut last_failure: Option<String> = None;

                loop {
                    std::thread::sleep(WATCHER_SETTLE);

                    // Write trigger to force a watcher batch
                    let trigger = workdir.join("fff_fuzz_trigger.rs");
                    let _ = fs::write(&trigger, format!("// trigger {}\n", op_idx));

                    std::thread::sleep(WATCHER_SETTLE);

                    let mut all_ok = true;

                    // Check live markers (drop lock between each grep)
                    for tf in &tracked {
                        let guard = shared_picker.read().unwrap();
                        let picker = guard.as_ref().unwrap();
                        let found = grep_finds(picker, &tf.marker, mode);
                        drop(guard);
                        if !found {
                            last_failure = Some(format!(
                                "{mode_name} grep for {:?} in {:?} not found\n\
                                 is_created={} exists={} on_disk_has_marker={}",
                                tf.marker,
                                tf.relative,
                                tf.is_created,
                                workdir.join(&tf.relative).exists(),
                                fs::read_to_string(workdir.join(&tf.relative))
                                    .map(|c| c.contains(&tf.marker))
                                    .unwrap_or(false),
                            ));
                            all_ok = false;
                            break;
                        }
                    }

                    // Check dead markers (only sample a few per iteration to
                    // avoid holding the lock too long with many dead markers)
                    if all_ok {
                        let sample_size = dead_markers.len().min(20);
                        for dead in dead_markers.iter().take(sample_size) {
                            let guard = shared_picker.read().unwrap();
                            let picker = guard.as_ref().unwrap();
                            let found = grep_finds(picker, dead, GrepMode::PlainText);
                            drop(guard);
                            if found {
                                last_failure = Some(format!("dead marker {dead:?} still findable"));
                                all_ok = false;
                                break;
                            }
                        }
                    }

                    // Check ignored markers (sample first 5)
                    if all_ok {
                        for ig in ignored_markers.iter().take(5) {
                            let guard = shared_picker.read().unwrap();
                            let picker = guard.as_ref().unwrap();
                            let files = grep_file_list(picker, ig, GrepMode::PlainText);
                            drop(guard);
                            if !files.is_empty() {
                                last_failure =
                                    Some(format!("ignored marker {ig:?} found in {files:?}"));
                                all_ok = false;
                                break;
                            }
                        }
                    }

                    if all_ok {
                        eprintln!(
                            "  op[{op_idx}] verify OK: {mode_name} mode, {} live, {} dead, {} ignored",
                            tracked.len(),
                            dead_markers.len(),
                            ignored_markers.len(),
                        );
                        break;
                    }

                    if Instant::now() >= deadline {
                        panic!(
                            "op[{op_idx}] verify TIMEOUT after {CONVERGE_TIMEOUT:?}:\n  {}\n  ops_since_last_verify={}",
                            last_failure.unwrap_or_default(),
                            ops_since_verify,
                        );
                    }
                }

                ops_since_verify = 0;
            }
        }
    }

    // Final convergence: poll until everything is consistent
    let deadline = Instant::now() + CONVERGE_TIMEOUT;
    loop {
        std::thread::sleep(WATCHER_SETTLE);
        let guard = shared_picker.read().unwrap();
        let picker = guard.as_ref().unwrap();

        let live_ok = tracked
            .iter()
            .all(|tf| grep_finds(picker, &tf.marker, GrepMode::PlainText));
        let dead_ok = dead_markers
            .iter()
            .all(|d| !grep_finds(picker, d, GrepMode::PlainText));
        drop(guard);

        if live_ok && dead_ok {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "final verify TIMEOUT: live_ok={live_ok} dead_ok={dead_ok}"
        );
    }

    // Teardown
    shared_picker.wait_for_indexing_complete(Duration::from_secs(30));
    if let Ok(mut guard) = shared_picker.write() {
        if let Some(mut picker) = guard.take() {
            picker.stop_background_monitor();
        }
    }

    eprintln!(
        "  PASSED: {} ops, {} tracked, {} dead, {} ignored ({:.1}s)",
        ops.len(),
        tracked.len(),
        dead_markers.len(),
        ignored_markers.len(),
        scenario_start.elapsed().as_secs_f64(),
    );
}

// ================
// Proptest harness
// =================
//
fn proptest_config() -> ProptestConfig {
    ProptestConfig {
        cases: fuzz_cases(),
        max_shrink_iters: 0,
        fork: false,
        failure_persistence: Some(Box::new(FileFailurePersistence::Direct(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fuzz_real_repos.proptest-regressions",
        )))),
        ..ProptestConfig::default()
    }
}

#[derive(Debug, Clone)]
enum Op {
    CreateFile { seed: u32 },
    EditTracked { seed: u32 },
    EditRandom { seed: u32 },
    DeleteTracked,
    RevertTracked,
    IgnoredBurst { count: u8, seed: u32 },
    Verify,
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        12 => any::<u32>().prop_map(|s| Op::CreateFile { seed: s }),
        18 => any::<u32>().prop_map(|s| Op::EditTracked { seed: s }),
        18 => any::<u32>().prop_map(|s| Op::EditRandom { seed: s }),
        8 => Just(Op::DeleteTracked),
        10 => Just(Op::RevertTracked),
        9 => (1u8..20, any::<u32>()).prop_map(|(c, s)| Op::IgnoredBurst { count: c, seed: s }),
        // Explicit verification rounds
        25 => Just(Op::Verify),
    ]
}

fn ops_strategy() -> impl Strategy<Value = Vec<Op>> {
    let min = fuzz_min_ops();
    let max = fuzz_max_ops();
    prop::collection::vec(op_strategy(), min..=max)
}

proptest! {
    #![proptest_config(proptest_config())]

    #[test]
    fn fuzz_real_repos_proptest(ops in ops_strategy()) {
        run_scenario(&ops);
    }
}
