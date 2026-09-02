//! Vibe coded stress test: randomized file + git operations driven against the *real*
//! `BackgroundWatcher`, asserting the git-status invariant after every mutation.
//!
//! ## What this test is trying to catch
//!
//! The background watcher is supposed to keep every indexed file's
//! `git_status` in sync with the actual repository state at all times.
//! There are two code paths that can drift:
//!
//!   1. **Per-file updates** — when regular files are created / modified the
//!      watcher queries `git_status_for_paths(&[changed_file])`. If the git
//!      state changes for files that weren't in the batch (rename detection,
//!      submodule paths, index modifications applied by a sibling process),
//!      those other files keep a stale `git_status` until the next full
//!      rescan is triggered.
//!
//!   2. **Full rescans** — triggered by changes under `.git/` (index, HEAD,
//!      MERGE_HEAD, etc.) and by `.gitignore` edits. A missed event here is
//!      the most common failure mode: the watcher coalesced or dropped the
//!      `.git/index` notification and the picker never learns that e.g. a
//!      `git commit` cleared every `WT_MODIFIED`.
//!
//! ## How the test works
//!
//!  * Spin up a real `FilePicker` with `watch: true` on a fresh temp repo.
//!  * Use `proptest` to generate a sequence of 20–40 randomized ops (heavy on
//!    git mutations: add / commit / reset / stash / gitignore edits).
//!  * After **every** op, poll until the picker's per-file `git_status` agrees
//!    with `git2::Repository::statuses()` verbatim, or bail with a rich diff.
//!  * If there is ever a divergence that doesn't resolve within
//!    `CONVERGE_TIMEOUT`, the test fails with the list of `(path, truth,
//!    picker)` mismatches.
//!
//! The shape of the scenario is shrinkable: on failure proptest will shrink
//! the `Vec<AbstractOp>` so the reported diff corresponds to the smallest
//! surviving sequence.
//!
//! ## Runtime
//!
//! Each scenario does a real filesystem scan + watcher setup (~0.5–1 s) and
//! then runs 20–40 ops each of which waits for event propagation
//! (~100–500 ms on macOS FSEvents). With `cases = 2` the test takes
//! ~30–45 s on a typical dev machine — too slow to run as part of the
//! default `cargo test`, so it's gated behind the `stress` cfg.
//!
//! Run it explicitly with:
//! ```sh
//! RUSTFLAGS="--cfg stress" cargo test -p fff-search --test fuzz_git_watcher_stress -- --nocapture
//! ```
//!
//! Or increase coverage via env:
//! ```sh
//! FFF_STRESS_CASES=8 FFF_STRESS_MAX_OPS=60 \
//!     RUSTFLAGS="--cfg stress" cargo test -p fff-search --test fuzz_git_watcher_stress -- --nocapture
//! ```

#![cfg(stress)]
use fff_search::file_picker::{FFFMode, FilePicker};
use fff_search::grep::{GrepMode, GrepSearchOptions, parse_grep_query};
use fff_search::{
    FilePickerOptions, FuzzySearchOptions, PaginationArgs, QueryParser, SharedFilePicker,
    SharedFrecency,
};
use git2::{Repository, Status, StatusOptions};
use proptest::prelude::*;
use proptest::strategy::ValueTree;
use proptest::test_runner::{
    Config as ProptestConfig, FileFailurePersistence, RngAlgorithm, TestRng, TestRunner,
};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tempfile::TempDir;

/// Upper bound on how long we wait for the picker to converge after any op.
/// The watcher debounce is 50 ms; worst-case FSEvents propagation + queued
/// full-rescan can easily reach several seconds. 15 s is very generous.
const CONVERGE_TIMEOUT: Duration = Duration::from_secs(15);

/// Poll interval while waiting for convergence.
const CONVERGE_POLL: Duration = Duration::from_millis(50);

/// Small pause between back-to-back ops to simulate real user behavior
const PER_OP_SETTLE: Duration = Duration::from_millis(10);

fn stress_cases() -> u32 {
    std::env::var("FFF_STRESS_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2)
}

fn stress_max_ops() -> usize {
    std::env::var("FFF_STRESS_MAX_OPS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(40)
}

fn stress_min_ops() -> usize {
    std::env::var("FFF_STRESS_MIN_OPS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20)
}

/// A single randomized action. The runtime interprets abstract handles
/// (`idx`) by modulo against the *current* list of live files, so the same
/// abstract op sequence is always runnable regardless of which files exist.
#[derive(Debug, Clone)]
enum AbstractOp {
    CreateFile {
        seed: u32,
        content_seed: u32,
    },
    EditFile {
        idx: usize,
        content_seed: u32,
    },
    DeleteFile {
        idx: usize,
    },
    RenameFile {
        idx: usize,
        new_seed: u32,
    },
    CreateSubdirFile {
        dir_seed: u16,
        file_seed: u16,
        content_seed: u32,
    },
    GitignoreAppend {
        pattern_seed: u16,
    },
    GitAddAll,
    GitCommit {
        msg_seed: u16,
    },
    GitResetHard,
    GitStashThenPop,
    Touch {
        idx: usize,
    }, // rewrite with same content; should still fire Modify
    Noop,
}

fn op_strategy() -> impl Strategy<Value = AbstractOp> {
    // Weights tuned to mirror a real editing session rather than a
    // contrived git-heavy workload. In descending order:
    //
    //   * edits dominate (~60%) — a real user bangs on Save all day
    //   * creates are moderate (~15%) — new files appear occasionally
    //   * deletes and renames are rare (~10%) — destructive ops happen
    //     but far less often than edits
    //   * explicit git operations are the rarest (~8%) — users stage /
    //     commit / reset in bursts between long stretches of editing
    //
    // The file-mutation path still exercises the per-path git-status
    // update on every op via the watcher, so the git-status updater
    // is *always* under load — the rare explicit `GitAddAll` /
    // `GitCommit` / `GitResetHard` ops layer in coverage of the
    // `.git/index` event → full-rescan path specifically. A 40-op
    // scenario fires that path ~3 times, which is enough to keep the
    // race between worktree writes and `.git/*` updates in rotation.
    prop_oneof![
        40 => (any::<usize>(), any::<u32>()).prop_map(|(i, c)| AbstractOp::EditFile {
            idx: i, content_seed: c,
        }),
        5  => any::<usize>().prop_map(|i| AbstractOp::Touch { idx: i }),
        8  => (any::<u32>(), any::<u32>()).prop_map(|(a, b)| AbstractOp::CreateFile {
            seed: a, content_seed: b,
        }),
        3  => (any::<u16>(), any::<u16>(), any::<u32>()).prop_map(
            |(d, f, c)| AbstractOp::CreateSubdirFile {
                dir_seed: d, file_seed: f, content_seed: c,
            }
        ),
        4  => any::<usize>().prop_map(|i| AbstractOp::DeleteFile { idx: i }),
        3  => (any::<usize>(), any::<u32>()).prop_map(|(i, s)| AbstractOp::RenameFile {
            idx: i, new_seed: s,
        }),
        1  => Just(AbstractOp::GitAddAll),
        1  => any::<u16>().prop_map(|m| AbstractOp::GitCommit { msg_seed: m }),
        1  => Just(AbstractOp::GitStashThenPop),
        1  => Just(AbstractOp::GitResetHard),
        1  => any::<u16>().prop_map(|p| AbstractOp::GitignoreAppend { pattern_seed: p }),
        2  => Just(AbstractOp::Noop),
    ]
}

fn ops_strategy() -> impl Strategy<Value = Vec<AbstractOp>> {
    ops_strategy_bounded(stress_min_ops(), stress_max_ops())
}

fn ops_strategy_bounded(min: usize, max: usize) -> impl Strategy<Value = Vec<AbstractOp>> {
    prop::collection::vec(op_strategy(), min..=max)
}

const DEFAULT_STRESS_SEED: u64 = 0xDEAD_BEEF_CAFE_BABE;

fn proptest_config() -> ProptestConfig {
    ProptestConfig {
        cases: stress_cases(),
        // Cap shrinking because each shrink iteration replays the whole
        // scenario (seconds of real IO). Proptest's default 4096 is wild.
        max_shrink_iters: 16,
        // `fork: true` would isolate runs but swallows the panic payload
        // and printed diff through `rusty-fork`'s wire format — we want
        // the convergence report visible on stderr, so we keep in-process.
        // The watcher is fully torn down between cases via `Drop`.
        fork: false,
        // Integration tests don't live alongside a `lib.rs`/`main.rs`, so
        // proptest's default `SourceParallel` persistence strategy can't
        // locate the source tree and logs a noisy warning on every run.
        // Pin the regression file to an explicit path next to this test
        // so failing seeds get checked in and reproduced in CI.
        failure_persistence: Some(Box::new(FileFailurePersistence::Direct(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fuzz_git_watcher_stress.proptest-regressions",
        )))),
        ..ProptestConfig::default()
    }
}

proptest! {
    #![proptest_config(proptest_config())]

    /// Random-seeded scenario. Proptest picks a fresh seed every run from
    /// system entropy, so CI sees different trajectories on every build
    /// while known-bad seeds remain pinned in the regressions file.
    #[test]
    fn stress_random(ops in ops_strategy()) {
        run_stress_scenario(&ops);
    }
}

/// Deterministic scenario keyed off `FFF_STRESS_SEED` (or
/// [`DEFAULT_STRESS_SEED`] if the env var is not set). Runs the same
/// proptest `ops_strategy()` but uses a ChaCha RNG seeded by the expanded
/// u64, so the exact case sequence is reproducible across machines and CI
/// runs.
///
/// When this test panics, the panic message includes the seed so you can
/// re-run the failing case locally via:
///
/// ```sh
/// RUSTFLAGS="--cfg stress" FFF_STRESS_SEED=0xDEADBEEFCAFEBABE \
///     cargo test -p fff-search --test fuzz_git_watcher_stress seeded -- --nocapture
/// ```
#[test]
fn stress_seeded() {
    let seed = parse_stress_seed();
    let seed_bytes = expand_u64_seed(seed);

    eprintln!("stress_seeded: using deterministic seed {seed:#018x}");

    let mut config = proptest_config();
    // The seeded run should never write to the shared regressions file —
    // its failures are reproducible from the env var alone, and polluting
    // the shared file with the deterministic seed would mask regressions
    // for the random run that actually needs persistence.
    config.failure_persistence = Some(Box::new(FileFailurePersistence::Off));

    let rng = TestRng::from_seed(RngAlgorithm::ChaCha, &seed_bytes);
    let mut runner = TestRunner::new_with_rng(config, rng);
    let strategy = ops_strategy();

    // Mimic `proptest!`'s case loop: run `Config::cases` independent draws
    // from the strategy, failing the test as soon as any one scenario
    // diverges. We drive this ourselves because `runner.run()` can't
    // accept a non-Fn closure, and our scenario runner is side-effectful.
    for case_idx in 0..runner.config().cases {
        let tree = strategy
            .new_tree(&mut runner)
            .expect("ops_strategy::new_tree");
        let ops = tree.current();
        eprintln!(
            "  case {}/{}: {} ops",
            case_idx + 1,
            runner.config().cases,
            ops.len()
        );
        run_stress_scenario(&ops);
    }
}

/// Pinned deterministic regression for the git-status divergence found on
/// Windows CI (run 28264744320): after a `GitCommit` the picker retained stale
/// `INDEX_*` bits because a pre-commit per-path status snapshot was applied
/// after the post-commit full rescan.
///
/// The op sequence is regenerated from the proptest seed persisted in the
/// regressions file (`cc 2c9d...`) using the CI op bounds (30..=60) that were
/// in effect when the failure was found. The fingerprint assertion fails
/// loudly if `ops_strategy()` ever changes shape — a changed strategy would
/// silently decode the same seed into a *different* scenario, turning this
/// regression guard into a no-op.
#[test]
fn stress_regression_stale_index_after_commit() {
    let ops = ops_from_chacha_seed(REGRESSION_SEED_HEX, 30, 60);
    assert_eq!(
        (ops.len(), fingerprint_ops(&ops)),
        (59, 0xc73f_16ce_b249_78eb),
        "ops_strategy() changed shape: the pinned seed no longer decodes to \
         the original Windows-CI scenario. Either revert the strategy change \
         or re-pin this regression (the original literal op list is in git \
         history of this file).",
    );
    run_stress_scenario(&ops);
}

/// 32-byte ChaCha seed persisted by proptest for the Windows CI failure
/// (the `cc 2c9d...` entry in the regressions file).
const REGRESSION_SEED_HEX: &str =
    "2c9d1ea2efbf6161f84b69598e884dbf1bde6039c70625adde0374817e20e2ea";

/// Regenerate an op sequence from a persisted proptest ChaCha seed by
/// replaying `ops_strategy()` the same way proptest does for regressions.
/// `min`/`max` must match the `FFF_STRESS_{MIN,MAX}_OPS` bounds that were
/// in effect when the seed was persisted — the strategy's value tree
/// depends on them.
fn ops_from_chacha_seed(seed_hex: &str, min: usize, max: usize) -> Vec<AbstractOp> {
    let seed_bytes: Vec<u8> = (0..seed_hex.len() / 2)
        .map(|i| u8::from_str_radix(&seed_hex[2 * i..2 * i + 2], 16).expect("valid hex seed"))
        .collect();
    let mut config = proptest_config();
    config.failure_persistence = Some(Box::new(FileFailurePersistence::Off));
    let rng = TestRng::from_seed(RngAlgorithm::ChaCha, &seed_bytes);
    let mut runner = TestRunner::new_with_rng(config, rng);
    ops_strategy_bounded(min, max)
        .new_tree(&mut runner)
        .expect("ops_strategy::new_tree")
        .current()
}

/// FNV-1a over the debug repr of the ops; stable across platforms and runs.
fn fingerprint_ops(ops: &[AbstractOp]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in format!("{ops:?}").bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Parse `FFF_STRESS_SEED` as either decimal or `0x`-prefixed hex.
fn parse_stress_seed() -> u64 {
    match std::env::var("FFF_STRESS_SEED") {
        Ok(raw) => {
            let trimmed = raw.trim();
            if let Some(hex) = trimmed
                .strip_prefix("0x")
                .or_else(|| trimmed.strip_prefix("0X"))
            {
                u64::from_str_radix(hex, 16)
                    .unwrap_or_else(|e| panic!("FFF_STRESS_SEED={raw:?} is not valid hex: {e}"))
            } else {
                trimmed
                    .parse::<u64>()
                    .unwrap_or_else(|e| panic!("FFF_STRESS_SEED={raw:?} is not a valid u64: {e}"))
            }
        }
        Err(_) => DEFAULT_STRESS_SEED,
    }
}

/// Expand a u64 into the 32-byte seed ChaCha20 wants, by repeating the
/// little-endian bytes four times. The cycle is deliberate: two different
/// u64 seeds produce completely different byte sequences, so collisions
/// across the expansion are irrelevant in practice.
fn expand_u64_seed(seed: u64) -> [u8; 32] {
    let le = seed.to_le_bytes();
    let mut out = [0u8; 32];
    for i in 0..4 {
        out[i * 8..(i + 1) * 8].copy_from_slice(&le);
    }
    out
}

#[derive(Debug)]
struct Live {
    /// Path relative to the repo root (forward slashes on all platforms).
    relative: String,
    abs: PathBuf,
}

fn run_stress_scenario(ops: &[AbstractOp]) {
    // Opt-in tracing so `RUST_LOG=fff_search=debug` shows the watcher /
    // refresh / update trail when debugging a failing run. Uses
    // `try_init` so proptest can run many scenarios in the same process
    // without double-initialising the subscriber.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                // Default to DEBUG for fff crates so CI failures on
                // flaky OSes (Windows) include the watcher/event trail
                // without needing to re-run with RUST_LOG set.
                .unwrap_or_else(|_| {
                    tracing_subscriber::EnvFilter::new(
                        "warn,fff_search=debug,notify=debug,notify_debouncer_full=debug",
                    )
                }),
        )
        .with_test_writer()
        .try_init();

    let tmp = TempDir::new().unwrap();
    let base = tmp.path().canonicalize().unwrap();

    seed_repo(&base);

    let (shared_picker, _frecency) = start_watched_picker(&base);
    wait_ready(&shared_picker);

    // Reconcile `live` from disk: after `git reset --hard` or similar the
    // worktree can change under us, so we resync at every step instead of
    // trusting our in-memory list.
    let mut live: Vec<Live> = get_baseline_status_from_git(&base);

    // Sleep past the coarse (1 s) mtime tick so subsequent edits definitely
    // advance mtime. Not strictly required for git status correctness but
    // matches what real users experience.
    std::thread::sleep(Duration::from_millis(1100));

    for (step, op) in ops.iter().enumerate() {
        apply_op(op, &base, &mut live);

        // Re-sync live from disk — ops like GitResetHard / GitStashThenPop
        // may have created or removed files behind our back.
        live = get_baseline_status_from_git(&base);
        std::thread::sleep(PER_OP_SETTLE);

        if let Err(err) = converge_git_status(&shared_picker, &base, &live) {
            panic!(
                "\n──────────────────────────────────────────────────────────\n\
                 ❌ Picker git_status diverged from repository truth.\n\
                 ──────────────────────────────────────────────────────────\n\
                 step:    {step}\n\
                 op:      {op:?}\n\
                 scenario ops:\n{trace}\n\
                 ──────────────────────────────────────────────────────────\n\
                 {err}\n",
                trace = format_ops_trace(ops, step),
            );
        }
    }
}

fn format_ops_trace(ops: &[AbstractOp], up_to_and_including: usize) -> String {
    let mut s = String::new();
    for (i, op) in ops.iter().enumerate().take(up_to_and_including + 1) {
        s.push_str(&format!("   [{i:>3}] {op:?}\n"));
    }
    s
}

fn seed_repo(base: &Path) {
    fs::create_dir_all(base.join("src")).unwrap();
    fs::write(base.join("README.md"), "# seed\n").unwrap();
    fs::write(base.join("src/main.rs"), "fn main() {}\n").unwrap();
    fs::write(base.join("src/lib.rs"), "// lib\n").unwrap();
    fs::write(base.join(".gitignore"), "*.log\ntmp/\n").unwrap();

    git(base, &["init", "-b", "main"]);
    git(base, &["config", "user.email", "fuzz@fff.test"]);
    git(base, &["config", "user.name", "fuzz"]);
    // Disable rename detection noise — libgit2 still computes ranks, but
    // keeping the porcelain behaviour deterministic helps with diff reading.
    git(base, &["config", "status.renames", "false"]);
    git(base, &["add", "-A"]);
    git(base, &["commit", "-m", "seed", "--no-gpg-sign"]);
}

fn apply_op(op: &AbstractOp, base: &Path, live: &mut [Live]) {
    use AbstractOp::*;

    match op {
        CreateFile { seed, content_seed } => {
            let rel = format!("f_{seed:08x}.rs");
            let abs = base.join(&rel);
            if abs.exists() {
                return;
            }
            fs::write(&abs, content_for(*content_seed, &rel)).unwrap();
        }
        EditFile { idx, content_seed } => {
            if live.is_empty() {
                return;
            }
            let i = idx % live.len();
            let abs = &live[i].abs;
            if !abs.is_file() {
                return;
            }
            // Body must differ so git sees WT_MODIFIED (not just atime touch).
            let body = format!(
                "// edited seed={content_seed:08x}\n{}",
                content_for(*content_seed, &live[i].relative)
            );
            fs::write(abs, body).unwrap();
        }
        Touch { idx } => {
            // Rewrite with the same bytes we currently hold on disk. Still
            // generates a Modify event; git status stays the same if the
            // content matches the index, or WT_MODIFIED if it differs.
            if live.is_empty() {
                return;
            }
            let i = idx % live.len();
            let abs = &live[i].abs;
            if let Ok(contents) = fs::read(abs) {
                let _ = fs::write(abs, contents);
            }
        }
        DeleteFile { idx } => {
            if live.is_empty() {
                return;
            }
            let i = idx % live.len();
            let _ = fs::remove_file(&live[i].abs);
        }
        RenameFile { idx, new_seed } => {
            if live.is_empty() {
                return;
            }
            let i = idx % live.len();
            let old = &live[i].abs;
            if !old.is_file() {
                return;
            }
            let new_rel = format!("r_{new_seed:08x}.rs");
            let new_abs = base.join(&new_rel);
            if new_abs.exists() {
                return;
            }
            let _ = fs::rename(old, &new_abs);
        }
        CreateSubdirFile {
            dir_seed,
            file_seed,
            content_seed,
        } => {
            let rel = format!("d_{dir_seed:04x}/inside_{file_seed:04x}.rs");
            let abs = base.join(&rel);
            if abs.exists() {
                return;
            }
            fs::create_dir_all(abs.parent().unwrap()).unwrap();
            fs::write(&abs, content_for(*content_seed, &rel)).unwrap();
        }
        GitignoreAppend { pattern_seed } => {
            // Introduce NEW ignore patterns for a namespace not used by any
            // other op so we never accidentally re-ignore a file under test.
            let pattern = format!("__ignored_{pattern_seed:x}/\n");
            let gi = base.join(".gitignore");
            let mut cur = fs::read_to_string(&gi).unwrap_or_default();
            cur.push_str(&pattern);
            fs::write(&gi, cur).unwrap();
        }
        GitAddAll => {
            git_allow_fail(base, &["add", "-A"]);
        }
        GitCommit { msg_seed } => {
            // `--allow-empty` so we don't depend on there actually being
            // staged changes; this still bumps HEAD and rewrites .git/index.
            let _ = git_output(
                base,
                &[
                    "commit",
                    "-m",
                    &format!("fuzz-{msg_seed:x}"),
                    "--allow-empty",
                    "--allow-empty-message",
                    "--no-gpg-sign",
                ],
            );
        }
        GitResetHard => {
            git_allow_fail(base, &["reset", "--hard"]);
        }
        GitStashThenPop => {
            git_allow_fail(base, &["add", "-A"]);
            let stash = git_output(base, &["stash", "push", "-u", "-m", "fuzz"]);
            let had_stash = stash
                .as_ref()
                .map(|o| {
                    o.status.success()
                        && !String::from_utf8_lossy(&o.stdout).contains("No local changes")
                })
                .unwrap_or(false);
            if had_stash {
                git_allow_fail(base, &["stash", "pop"]);
            }
        }
        Noop => {}
    }
}

/// Block until the picker's git status agrees with `git2::Repository::statuses`.
///
/// Poll until the picker's git-status view matches libgit2's truth AND
/// the real-query probe succeeds, or bail with a rich diff.
///
/// On timeout, returns an `Err` describing every disagreement.
fn converge_git_status(
    shared_picker: &SharedFilePicker,
    base: &Path,
    live: &[Live],
) -> Result<(), String> {
    let deadline = Instant::now() + CONVERGE_TIMEOUT;
    let mut last_mismatches: Vec<Mismatch>;
    let mut last_probe_err: Option<String> = None;

    loop {
        let truth = read_truth_status(base);
        let picker_view = read_picker_status(shared_picker);
        last_mismatches = diff_statuses(&truth, &picker_view);

        // Real-query probe: one random live file per round via fuzzy + grep.
        // We only consider the probe authoritative once the main git-status
        // enumeration agrees — otherwise a probe failure might just be
        // the same debouncer-lag we're already waiting out.
        let probe = if last_mismatches.is_empty() {
            probe_real_queries(shared_picker, live)
        } else {
            None
        };

        match (last_mismatches.is_empty(), probe) {
            (true, None) | (true, Some(Ok(()))) => return Ok(()),
            (true, Some(Err(msg))) => last_probe_err = Some(msg),
            _ => {}
        }

        if Instant::now() >= deadline {
            let mut report = if !last_mismatches.is_empty() {
                format_mismatches(&last_mismatches, shared_picker)
            } else {
                String::from("git_status enumeration converged, but real-query probe failed:\n")
            };
            if let Some(probe_msg) = last_probe_err {
                report.push_str("\n── real-query probe ──\n");
                report.push_str(&probe_msg);
                report.push('\n');
            }
            report.push_str(&debug_dump_environment(
                base,
                shared_picker,
                &last_mismatches,
            ));
            return Err(report);
        }
        std::thread::sleep(CONVERGE_POLL);
    }
}

/// On-failure diagnostic dump. Includes:
///   * base path (raw + OS encoding) so we can spot UNC/`\\?\` prefixes on Windows
///   * picker's own `base_path()` for comparison
///   * direct libgit2 `status_file` probes for every mismatched path
///   * full picker enumeration (first 20 rows) so we can see what keys
///     the picker is returning vs. what git2 reports
fn debug_dump_environment(
    base: &Path,
    shared_picker: &SharedFilePicker,
    mismatches: &[Mismatch],
) -> String {
    let mut s = String::new();
    s.push_str("\n── diagnostic dump ──\n");
    s.push_str(&format!(
        "test base path (display)   : {}\n",
        base.display()
    ));
    s.push_str(&format!("test base path (debug)     : {:?}\n", base));
    s.push_str(&format!(
        "test base path (os bytes)  : {:?}\n",
        base.as_os_str()
    ));

    if let Ok(guard) = shared_picker.read()
        && let Some(picker) = guard.as_ref()
    {
        s.push_str(&format!(
            "picker base_path (display) : {}\n",
            picker.base_path().display()
        ));
        s.push_str(&format!(
            "picker base_path (debug)   : {:?}\n",
            picker.base_path()
        ));
    }

    // Probe libgit2 directly for each mismatched path.
    if let Ok(repo) = Repository::open(base) {
        s.push_str("\nlibgit2 status_file probes (per mismatch path):\n");
        for m in mismatches {
            let path = match m {
                Mismatch::Disagree { path, .. } => path,
                Mismatch::ExtraInPicker { path, .. } => path,
            };
            match repo.status_file(std::path::Path::new(path)) {
                Ok(st) => s.push_str(&format!("  • {path} -> {st:?}\n")),
                Err(e) => {
                    s.push_str(&format!(
                        "  • {path} -> ERROR {} (class={:?}, code={:?})\n",
                        e.message(),
                        e.class(),
                        e.code()
                    ));
                }
            }
        }
    } else {
        s.push_str("\nlibgit2: could not open repo at base path\n");
    }

    // Dump the raw picker enumeration for comparison — up to 20 rows.
    s.push_str("\npicker enumeration (first 20 entries, with byte-repr of each relative path):\n");
    if let Ok(guard) = shared_picker.read()
        && let Some(picker) = guard.as_ref()
    {
        let parser = QueryParser::default();
        let parsed = parser.parse("");
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
        for (i, f) in result.items.iter().take(20).enumerate() {
            let raw = f.relative_path(picker);
            let norm = normalize(raw.clone());
            s.push_str(&format!(
                "  [{i:>2}] raw={raw:?} norm={norm:?} status={:?}\n",
                f.git_status
            ));
        }
        s.push_str(&format!("  (total: {} items)\n", result.items.len()));
    }

    s
}

#[derive(Debug)]
enum Mismatch {
    /// Git knows about this path with `truth`, picker has `picker` (or None = missing).
    Disagree {
        path: String,
        truth: Status,
        picker: Option<Option<Status>>,
    },
    /// Picker has a non-clean entry for a path git doesn't report at all.
    ExtraInPicker { path: String, picker: Status },
}

fn read_truth_status(base: &Path) -> BTreeMap<String, Status> {
    let repo = Repository::open(base).expect("open repo for truth");
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_unmodified(true)
        .exclude_submodules(true);
    let statuses = repo.statuses(Some(&mut opts)).expect("read statuses");

    let mut out = BTreeMap::new();
    for entry in statuses.iter() {
        if let Ok(p) = entry.path() {
            // git2 returns forward-slash paths; accept as-is.
            out.insert(p.to_string(), entry.status());
        }
    }
    out
}

fn read_picker_status(shared: &SharedFilePicker) -> BTreeMap<String, Option<Status>> {
    let guard = shared.read().expect("picker read lock");
    let picker = guard.as_ref().expect("picker initialized");

    // Use the user-facing `fuzzy_search` API with an empty query to enumerate
    // every file a user could ever see in the picker. An empty fuzzy query
    // falls through to the frecency-only scoring path which returns ALL
    // non-deleted files (base + overflow). A very large `limit` makes sure
    // we don't silently paginate anything away for scenarios with many files.
    //
    // This intentionally mirrors what a real Neovim user observes — soft-
    // deleted tombstones, files filtered out by search-side predicates, etc.
    // are all excluded from the picker's user-facing view.
    let parser = QueryParser::default();
    let parsed = parser.parse("");
    let result = picker.fuzzy_search(
        &parsed,
        None,
        FuzzySearchOptions {
            max_threads: 1,
            pagination: PaginationArgs {
                offset: 0,
                limit: 100_000,
            },
            ..Default::default()
        },
    );

    let mut out = BTreeMap::new();
    for f in &result.items {
        out.insert(normalize(f.relative_path(picker)), f.git_status);
    }
    out
}

fn normalize(s: String) -> String {
    #[cfg(windows)]
    {
        if s.contains('\\') {
            return s.replace('\\', "/");
        }
    }
    s
}

/// Probe a single file by name via the public `fuzzy_search` API and return
/// its status, or `None` if the file is not findable. Used for diagnostics
/// when the top-level enumeration reports a disagreement — confirms that
/// the mismatch reproduces via the exact user-facing lookup path.
fn probe_single_file_status(shared: &SharedFilePicker, relative: &str) -> Option<Option<Status>> {
    let guard = shared.read().ok()?;
    let picker = guard.as_ref()?;
    let parser = QueryParser::default();
    let parsed = parser.parse(relative);
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
        .find(|f| normalize(f.relative_path(picker)) == relative)
        .map(|f| f.git_status)
}

// ═══════════════════════════════════════════════════════════════════════════
// Real-query probes
// ═══════════════════════════════════════════════════════════════════════════
//
// These exercise the **user-facing** fuzzy + grep surfaces with queries that
// look like what a human actually types, on top of a live file picked at
// random from the on-disk truth. They run once per convergence round in
// addition to the primary git-status enumeration check, and add coverage
// for three paths the empty-query enumeration never touches:
//
//   * fuzzy matching with a non-empty query (bigram prefilter + score)
//   * path-constraint fuzzy queries ("foo src/")
//   * live grep (mmap content cache + bigram overlay for overflow files)
//
// An atomic counter drives the rotation across rounds so the probe spreads
// its attention across every live file over the course of a scenario.
static PROBE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Read a file on disk and pull out the `FFF_STRESS_MARKER_<hex>` token
/// that [`content_for`] embedded in it. Returns `None` for files we never
/// wrote (seed README.md, .gitignore) — the caller skips the grep probe
/// in that case but still runs the fuzzy probe.
fn extract_marker(abs: &Path) -> Option<String> {
    let content = fs::read_to_string(abs).ok()?;
    let start = content.find("FFF_STRESS_MARKER_")?;
    // Marker is exactly `FFF_STRESS_MARKER_` + 8 hex chars.
    const MARKER_LEN: usize = "FFF_STRESS_MARKER_".len() + 8;
    if start + MARKER_LEN > content.len() {
        return None;
    }
    let marker = &content[start..start + MARKER_LEN];
    // Sanity: the trailing 8 chars must all be hex.
    if !marker.as_bytes()[MARKER_LEN - 8..]
        .iter()
        .all(|b| b.is_ascii_hexdigit())
    {
        return None;
    }
    Some(marker.to_string())
}

/// File-stem → fuzzy search query. Strips extension + leading path
/// components so the probe feeds the picker a typical "I know roughly
/// what I'm looking for" query.
fn stem_for_query(relative: &str) -> String {
    PathBuf::from(relative)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// Run a one-shot fuzzy_search and return the matched items' relative
/// paths paired with their `git_status`. Uses only public APIs.
fn fuzzy_search_items(shared: &SharedFilePicker, query: &str) -> Vec<(String, Option<Status>)> {
    let guard = match shared.read() {
        Ok(g) => g,
        Err(_) => return Vec::new(),
    };
    let Some(picker) = guard.as_ref() else {
        return Vec::new();
    };
    let parser = QueryParser::default();
    let parsed = parser.parse(query);
    let result = picker.fuzzy_search(
        &parsed,
        None,
        FuzzySearchOptions {
            max_threads: 1,
            pagination: PaginationArgs {
                offset: 0,
                limit: 500,
            },
            ..Default::default()
        },
    );
    result
        .items
        .iter()
        .map(|f| (normalize(f.relative_path(picker)), f.git_status))
        .collect()
}

/// Run live grep (plain-text mode) and return the unique set of matched
/// file paths. A file appears in the set iff at least one line inside it
/// matches the query. Uses only public APIs.
fn grep_plain_matches(shared: &SharedFilePicker, query: &str) -> Vec<String> {
    let guard = match shared.read() {
        Ok(g) => g,
        Err(_) => return Vec::new(),
    };
    let Some(picker) = guard.as_ref() else {
        return Vec::new();
    };
    let parsed = parse_grep_query(query);
    let opts = GrepSearchOptions {
        max_file_size: 10 * 1024 * 1024,
        max_matches_per_file: 200,
        smart_case: true,
        file_offset: 0,
        page_limit: 500,
        mode: GrepMode::PlainText,
        time_budget_ms: 0,
        enforce_time_budget: false,
        before_context: 0,
        after_context: 0,
        classify_definitions: false,
        trim_whitespace: false,
        abort_signal: None,
    };
    let result = picker.grep(&parsed, &opts);
    // `GrepResult::files` is the already-deduplicated list of files that
    // contained at least one match — exactly what we want.
    result
        .files
        .iter()
        .map(|f| normalize(f.relative_path(picker)))
        .collect()
}

/// Run live grep (fuzzy mode) and return matched file paths.
/// Exercises the `fuzzy_grep_search` code path which resolves content
/// via arena pointers — the path that was silently broken for overflow
/// files before the overflow_arena fix.
fn grep_fuzzy_matches(shared: &SharedFilePicker, query: &str) -> Vec<String> {
    let guard = match shared.read() {
        Ok(g) => g,
        Err(_) => return Vec::new(),
    };
    let Some(picker) = guard.as_ref() else {
        return Vec::new();
    };
    let parsed = parse_grep_query(query);
    let opts = GrepSearchOptions {
        max_file_size: 10 * 1024 * 1024,
        max_matches_per_file: 200,
        smart_case: true,
        file_offset: 0,
        page_limit: 500,
        mode: GrepMode::Fuzzy,
        time_budget_ms: 0,
        enforce_time_budget: false,
        before_context: 0,
        after_context: 0,
        classify_definitions: false,
        trim_whitespace: false,
        abort_signal: None,
    };
    let result = picker.grep(&parsed, &opts);
    result
        .files
        .iter()
        .map(|f| normalize(f.relative_path(picker)))
        .collect()
}

/// Run live grep (regex mode) and return matched file paths.
fn grep_regex_matches(shared: &SharedFilePicker, query: &str) -> Vec<String> {
    let guard = match shared.read() {
        Ok(g) => g,
        Err(_) => return Vec::new(),
    };
    let Some(picker) = guard.as_ref() else {
        return Vec::new();
    };
    let parsed = parse_grep_query(query);
    let opts = GrepSearchOptions {
        max_file_size: 10 * 1024 * 1024,
        max_matches_per_file: 200,
        smart_case: true,
        file_offset: 0,
        page_limit: 500,
        mode: GrepMode::Regex,
        time_budget_ms: 0,
        enforce_time_budget: false,
        before_context: 0,
        after_context: 0,
        classify_definitions: false,
        trim_whitespace: false,
        abort_signal: None,
    };
    let result = picker.grep(&parsed, &opts);
    result
        .files
        .iter()
        .map(|f| normalize(f.relative_path(picker)))
        .collect()
}

/// Report from [`probe_real_queries`]. `None` means "nothing to probe this
/// round" (empty live set). `Some(Err)` means a probe disagreed with truth
/// — convergence should not treat this as success.
type ProbeOutcome = Option<Result<(), String>>;

/// Real-query verification: for one rotated-live-file per round, run a
/// fuzzy search by stem and (when a marker is present) a grep for its
/// embedded token. Asserts both return the file *and* that its
/// `git_status` matches the picker's main view.
///
/// Return values:
///   * `None`         — nothing to probe (no live files).
///   * `Some(Ok(()))` — the probe agreed with truth on both surfaces.
///   * `Some(Err(s))` — diagnostic describing the disagreement.
fn probe_real_queries(shared: &SharedFilePicker, live: &[Live]) -> ProbeOutcome {
    if live.is_empty() {
        return None;
    }
    let idx = (PROBE_COUNTER.fetch_add(1, Ordering::Relaxed) as usize) % live.len();
    let target = &live[idx];

    // --- Fuzzy probe: search by file stem ---
    let stem = stem_for_query(&target.relative);
    // Tiny stems (< 2 chars) are rejected by the fuzzy scorer and
    // surface via the frecency fallback — skip the assertion in that
    // case, nothing meaningful to verify.
    if stem.len() >= 2 {
        let fuzzy_hits = fuzzy_search_items(shared, &stem);
        let found = fuzzy_hits.iter().find(|(p, _)| p == &target.relative);
        if found.is_none() {
            return Some(Err(format!(
                "fuzzy_search({stem:?}) did not return expected live file {:?}\n\
                 got {} results; first few: {:?}",
                target.relative,
                fuzzy_hits.len(),
                fuzzy_hits.iter().take(5).collect::<Vec<_>>(),
            )));
        }
    }

    // --- Grep probe: search for the content marker using a randomly
    // rotated grep strategy. Each round picks one of PlainText / Fuzzy /
    // Regex so over many rounds all three code paths get exercised,
    // including the overflow-arena resolution that was previously broken
    // in fuzzy grep.
    if let Some(marker) = extract_marker(&target.abs) {
        let probe_round = PROBE_COUNTER.load(Ordering::Relaxed);
        let (mode_name, matches) = match probe_round % 3 {
            0 => ("plain", grep_plain_matches(shared, &marker)),
            1 => ("fuzzy", grep_fuzzy_matches(shared, &marker)),
            _ => ("regex", grep_regex_matches(shared, &marker)),
        };
        if !matches.contains(&target.relative) {
            return Some(Err(format!(
                "grep[{mode_name}]({marker:?}) did not return expected live file {:?}\n\
                 got {} matched files; first few: {:?}",
                target.relative,
                matches.len(),
                matches.iter().take(5).collect::<Vec<_>>(),
            )));
        }
    }

    Some(Ok(()))
}

/// Returns the full list of disagreements. Empty means "in sync".
///
/// We deliberately exclude a few categories from being considered bugs:
///   * Paths that git marks as `WT_DELETED` / `INDEX_DELETED` but the picker
///     has already dropped — this is by design (deleted files leave the
///     index immediately).
///   * Paths that git marks as `IGNORED` but the picker has already dropped.
///   * Paths under `.git/` — never tracked by the picker.
fn diff_statuses(
    truth: &BTreeMap<String, Status>,
    picker: &BTreeMap<String, Option<Status>>,
) -> Vec<Mismatch> {
    let mut out = Vec::new();

    for (path, &truth_status) in truth {
        if path.starts_with(".git/") || path == ".git" {
            continue;
        }
        match picker.get(path) {
            Some(&p) => {
                if !status_equivalent(truth_status, p) {
                    out.push(Mismatch::Disagree {
                        path: path.clone(),
                        truth: truth_status,
                        picker: Some(p),
                    });
                }
            }
            None => {
                // Tolerate picker not having deleted-from-disk files.
                let only_absence_reasons =
                    Status::WT_DELETED | Status::INDEX_DELETED | Status::IGNORED;
                if !truth_status.intersects(only_absence_reasons) {
                    out.push(Mismatch::Disagree {
                        path: path.clone(),
                        truth: truth_status,
                        picker: None,
                    });
                }
            }
        }
    }

    for (path, &p) in picker {
        if path.starts_with(".git/") || path == ".git" {
            continue;
        }
        if !truth.contains_key(path) {
            // Picker thinks a file exists that git has never heard of.
            // Only a bug if the picker also thinks it has a non-clean
            // status for it — otherwise it's a transient during which the
            // picker has indexed a file before the next truth snapshot.
            let non_clean = match p {
                None => false,
                Some(s) => !(s.is_empty() || s == Status::CURRENT),
            };
            if non_clean {
                out.push(Mismatch::ExtraInPicker {
                    path: path.clone(),
                    picker: p.unwrap_or(Status::CURRENT),
                });
            }
        }
    }

    out
}

/// `None` and `Some(CURRENT|empty)` are both "clean"; otherwise the bitsets
/// must match exactly.
fn status_equivalent(truth: Status, picker: Option<Status>) -> bool {
    let picker_bits = picker.unwrap_or(Status::CURRENT);
    let truth_clean = truth.is_empty() || truth == Status::CURRENT;
    let picker_clean = picker_bits.is_empty() || picker_bits == Status::CURRENT;
    if truth_clean && picker_clean {
        return true;
    }
    truth == picker_bits
}

fn format_mismatches(mismatches: &[Mismatch], shared: &SharedFilePicker) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "{} mismatch(es) after {} of wait:\n",
        mismatches.len(),
        humantime(CONVERGE_TIMEOUT),
    ));
    for m in mismatches {
        match m {
            Mismatch::Disagree {
                path,
                truth,
                picker,
            } => {
                let probe = probe_single_file_status(shared, path);
                s.push_str(&format!(
                    "  • {path}\n      truth       : {}\n      picker(enum): {}\n      picker(probe): {}\n",
                    format_status(Some(*truth)),
                    match picker {
                        Some(p) => format_status(*p),
                        None => "<missing from enumeration>".into(),
                    },
                    match probe {
                        Some(p) => format_status(p),
                        None => "<not returned by fuzzy_search>".into(),
                    }
                ));
            }
            Mismatch::ExtraInPicker { path, picker } => {
                let probe = probe_single_file_status(shared, path);
                s.push_str(&format!(
                    "  • {path}\n      truth        : <missing from repo>\n      picker(enum) : {}\n      picker(probe): {}\n",
                    format_status(Some(*picker)),
                    match probe {
                        Some(p) => format_status(p),
                        None => "<not returned by fuzzy_search>".into(),
                    }
                ));
            }
        }
    }
    s
}

fn format_status(s: Option<Status>) -> String {
    match s {
        None => "None (= clean)".into(),
        Some(st) if st.is_empty() || st == Status::CURRENT => "CURRENT (= clean)".into(),
        Some(st) => format!("{st:?}"),
    }
}

fn humantime(d: Duration) -> String {
    format!("{:.1}s", d.as_secs_f64())
}

fn get_baseline_status_from_git(base: &Path) -> Vec<Live> {
    let mut out = Vec::new();
    let repo = match Repository::open(base) {
        Ok(r) => r,
        Err(_) => return out,
    };
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_unmodified(true)
        .exclude_submodules(true);
    let statuses = match repo.statuses(Some(&mut opts)) {
        Ok(s) => s,
        Err(_) => return out,
    };
    for entry in statuses.iter() {
        if let Ok(p) = entry.path() {
            let abs = base.join(p);
            // Must be a real file *right now* — ignore stale WT_DELETED rows.
            if abs.is_file() {
                out.push(Live {
                    relative: p.to_string(),
                    abs,
                });
            }
        }
    }
    out
}

/// Content marker embedded in every generated file body. The probing
/// layer reads this back via `grep` to exercise the live-grep path
/// (which hits the bigram index / mmap cache / overflow content store —
/// code paths the empty-query fuzzy enumeration never touches).
///
/// Format: `FFF_STRESS_MARKER_<32-bit hex>` — namespaced so a repo
/// search for "MARKER" finds nothing from this test outside files we
/// wrote ourselves, which keeps the grep assertion unambiguous.
fn marker_for(seed: u32) -> String {
    format!("FFF_STRESS_MARKER_{seed:08x}")
}

fn content_for(seed: u32, name: &str) -> String {
    let marker = marker_for(seed);
    // Keep it small but distinct so adjacent edits produce different SHAs.
    // The marker appears twice — once in a comment, once in a string
    // literal — so a grep for it always has at least two hits per file
    // and one of them is guaranteed to be on its own line for easy
    // assertion.
    format!(
        "// file: {name}\n\
         // seed: {seed:08x}\n\
         // anchor: {marker}\n\
         pub fn anchor_{seed:x}() {{ let _ = \"{marker}\"; }}\n"
    )
}

fn start_watched_picker(base: &Path) -> (SharedFilePicker, SharedFrecency) {
    let shared_picker = SharedFilePicker::default();
    let shared_frecency = SharedFrecency::noop();

    FilePicker::new_with_shared_state(
        shared_picker.clone(),
        shared_frecency.clone(),
        FilePickerOptions {
            base_path: base.to_string_lossy().to_string(),
            enable_mmap_cache: false,
            enable_content_indexing: false,
            mode: FFFMode::Neovim,
            watch: true,
            ..Default::default()
        },
    )
    .expect("FilePicker::new_with_shared_state");

    (shared_picker, shared_frecency)
}

fn wait_ready(p: &SharedFilePicker) {
    assert!(
        p.wait_for_scan(Duration::from_secs(15)),
        "timed out waiting for initial scan"
    );
    assert!(
        p.wait_for_watcher(Duration::from_secs(15)),
        "timed out waiting for watcher"
    );
    // macOS FSEvents sometimes delivers a burst of "warmup" events right
    // after the stream opens — let them drain before we start fuzzing.
    std::thread::sleep(Duration::from_millis(200));
}

fn git_env() -> [(&'static str, &'static str); 4] {
    [
        ("GIT_AUTHOR_NAME", "fuzz"),
        ("GIT_AUTHOR_EMAIL", "fuzz@fff.test"),
        ("GIT_COMMITTER_NAME", "fuzz"),
        ("GIT_COMMITTER_EMAIL", "fuzz@fff.test"),
    ]
}

fn git(base: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(base)
        .envs(git_env())
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn git_allow_fail(base: &Path, args: &[&str]) {
    let _ = Command::new("git")
        .args(args)
        .current_dir(base)
        .envs(git_env())
        .output();
}

fn git_output(base: &Path, args: &[&str]) -> Option<std::process::Output> {
    Command::new("git")
        .args(args)
        .current_dir(base)
        .envs(git_env())
        .output()
        .ok()
}

// ═══════════════════════════════════════════════════════════════════════════
// Merge-conflict scenario
// ═══════════════════════════════════════════════════════════════════════════
//
// The fuzz alphabet above intentionally avoids branch operations — merge
// conflicts need a very specific topology (two divergent edits to the same
// hunk) that random sampling would almost never hit. This scripted test
// fills that gap with a deterministic end-to-end conflict flow:
//
//   1. seed a single tracked file `conflict.rs`
//   2. branch to `feature`, rewrite the file's inner expression, commit
//   3. back on `main`, rewrite the same hunk differently, commit
//   4. `git merge feature` — leaves `.git/MERGE_HEAD` + conflict markers
//   5. picker must observe `Status::CONFLICTED` for `conflict.rs`, and the
//      same real-query surfaces (fuzzy + grep for `<<<<<<<`) must return
//      the conflicted file
//   6. resolve by writing the merged content, `git add`, `git commit`
//   7. picker must converge back to `Status::CURRENT`
//
// On top of the `--cfg stress` gate the test inherits (so it never runs
// under plain `cargo test`), this is cross-platform: it uses libgit2
// internals + the `git` CLI, both of which work identically on macOS
// FSEvents, Linux inotify, and Windows ReadDirectoryChangesW (if we ever
// add Windows to the matrix). The convergence window is a bit larger than
// the fuzz case because `git merge` touches several `.git/*` files at
// once, producing a heftier event burst.

/// Convergence timeout for the conflict flow. `git merge` fires more FS
/// events than a plain `git add` — `.git/ORIG_HEAD`, `.git/MERGE_HEAD`,
/// `.git/MERGE_MSG`, the worktree file itself — so the debouncer has
/// more work than a one-off refresh. 30 s is still very generous.
const CONFLICT_CONVERGE_TIMEOUT: Duration = Duration::from_secs(30);

#[test]
fn stress_merge_conflict_convergence() {
    // Opt-in tracing for parity with the other stress tests.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                // Default to DEBUG for fff crates so CI failures on
                // flaky OSes (Windows) include the watcher/event trail
                // without needing to re-run with RUST_LOG set.
                .unwrap_or_else(|_| {
                    tracing_subscriber::EnvFilter::new(
                        "warn,fff_search=debug,notify=debug,notify_debouncer_full=debug",
                    )
                }),
        )
        .with_test_writer()
        .try_init();

    let tmp = TempDir::new().expect("mktemp");
    let base = tmp.path().canonicalize().expect("canonicalize tmp");

    seed_conflict_repo(&base);

    let (shared_picker, _frecency) = start_watched_picker(&base);
    wait_ready(&shared_picker);

    // ─────────────────────────────────────────────────────────────────
    // Stage 1: create divergent commits on `feature` and `main`
    // ─────────────────────────────────────────────────────────────────
    //
    // `conflict.rs` on `main` contains `BASE` text at line 2. We rewrite
    // that line two different ways on two branches, so a merge can't
    // pick a side automatically.
    //
    // Each rewrite is preceded by a 1.1 s sleep so the file's mtime
    // advances past the previous write's — the watcher's mmap cache
    // invalidation is mtime-triggered at 1 s granularity, and without
    // the sleep a follow-up grep on the picker sees stale content from
    // whichever variant was written last-but-one.

    std::thread::sleep(Duration::from_millis(1100));
    git(&base, &["checkout", "-b", "feature"]);
    fs::write(
        base.join("conflict.rs"),
        "fn flavour() {\n    \"FEATURE_VARIANT\"\n}\n",
    )
    .unwrap();
    git(&base, &["add", "conflict.rs"]);
    git(
        &base,
        &["commit", "-m", "feature: rewrite", "--no-gpg-sign"],
    );

    std::thread::sleep(Duration::from_millis(1100));
    git(&base, &["checkout", "main"]);
    fs::write(
        base.join("conflict.rs"),
        "fn flavour() {\n    \"MAIN_VARIANT\"\n}\n",
    )
    .unwrap();
    git(&base, &["add", "conflict.rs"]);
    git(&base, &["commit", "-m", "main: rewrite", "--no-gpg-sign"]);

    // Also sleep before the merge itself so the post-merge worktree
    // write lands in a fresh second.
    std::thread::sleep(Duration::from_millis(1100));

    // Let the divergent commits settle in the picker before we merge.
    expect_file_status(
        &shared_picker,
        &base,
        "conflict.rs",
        |s| {
            // Post-commit: should be clean (CURRENT or None = no row).
            s.is_none() || s.unwrap().is_empty() || s.unwrap().contains(Status::CURRENT)
        },
        Duration::from_secs(10),
        "pre-merge clean",
    )
    .expect("pre-merge state should be clean");

    // ─────────────────────────────────────────────────────────────────
    // Stage 2: trigger the conflicting merge
    // ─────────────────────────────────────────────────────────────────

    // `git merge feature` returns non-zero on conflict — that's fine.
    // We don't use `git(..)` (asserts success) — conflict IS the happy
    // path for this test.
    let merge = git_output(&base, &["merge", "feature", "--no-edit", "--no-gpg-sign"])
        .expect("git merge didn't launch");
    assert!(
        !merge.status.success(),
        "expected `git merge feature` to conflict, but it succeeded:\nstdout:{}\nstderr:{}",
        String::from_utf8_lossy(&merge.stdout),
        String::from_utf8_lossy(&merge.stderr)
    );

    // libgit2 marks both sides' modifications with `CONFLICTED`. We
    // wait for the picker to match.
    expect_file_status(
        &shared_picker,
        &base,
        "conflict.rs",
        |s| s.is_some_and(|st| st.contains(Status::CONFLICTED)),
        CONFLICT_CONVERGE_TIMEOUT,
        "CONFLICTED after merge",
    )
    .expect("picker must surface CONFLICTED after merge");

    // ─────────────────────────────────────────────────────────────────
    // Stage 3: real-query surfaces must still work in conflict state
    // ─────────────────────────────────────────────────────────────────

    // Fuzzy search by stem returns the conflicted file.
    let fuzzy_hits = fuzzy_search_items(&shared_picker, "conflict");
    assert!(
        fuzzy_hits.iter().any(|(p, _)| p == "conflict.rs"),
        "fuzzy_search(\"conflict\") during conflict state returned: {:?}",
        fuzzy_hits
    );

    // Grep for the diff conflict marker `<<<<<<<` — it's literally in
    // the worktree file right now, so live grep must find it.
    // The watcher rewrites the file on-disk during merge, so the mmap
    // cache needs to have been invalidated for this to succeed.
    let grep_hits = grep_plain_matches(&shared_picker, "<<<<<<< ");
    assert!(
        grep_hits.contains(&"conflict.rs".to_string()),
        "grep(\"<<<<<<< \") during conflict state returned: {:?}",
        grep_hits
    );

    // ─────────────────────────────────────────────────────────────────
    // Stage 4: resolve + commit, expect return to clean
    // ─────────────────────────────────────────────────────────────────

    // `on_create_or_modify` invalidates the mmap cache only when `mtime`
    // advances, and mtime has 1 s resolution on every filesystem we
    // ship on. `git merge` in stage 2 just wrote to `conflict.rs`; if
    // we re-write it within the same second the cache keeps its
    // pre-resolve bytes (complete with `<<<<<<<` markers) and grep will
    // keep finding them. Sleep past the mtime tick before re-writing.
    // This matches the pattern in `fuzz_file_operations.rs`.
    std::thread::sleep(Duration::from_millis(1100));

    fs::write(
        base.join("conflict.rs"),
        "fn flavour() {\n    \"RESOLVED_VARIANT\"\n}\n",
    )
    .unwrap();
    git(&base, &["add", "conflict.rs"]);
    git(
        &base,
        &[
            "commit",
            "-m",
            "resolve merge",
            "--no-gpg-sign",
            "--no-edit",
        ],
    );

    expect_file_status(
        &shared_picker,
        &base,
        "conflict.rs",
        |s| s.is_none() || s.unwrap().is_empty() || s.unwrap().contains(Status::CURRENT),
        CONFLICT_CONVERGE_TIMEOUT,
        "CURRENT after resolve",
    )
    .expect("picker must converge back to CURRENT after conflict resolution");

    // Sanity: conflict markers are gone from both the worktree AND the
    // picker's view (grep for `<<<<<<<` now returns 0 hits).
    //
    // The worktree check is an independent truth source — if it fails,
    // the test harness itself is buggy (the resolve write didn't apply).
    // The picker check can lag briefly: `expect_file_status` returned
    // once `.git/index` events made git_status = CURRENT, but the
    // `conflict.rs` Modify event from our resolve `fs::write` can land
    // in a separate debounced batch that hasn't been processed yet.
    // Poll until the mmap cache is flushed rather than asserting once.
    let on_disk = fs::read_to_string(base.join("conflict.rs")).unwrap();
    assert!(
        !on_disk.contains("<<<<<<<"),
        "worktree still has conflict markers after resolve — test harness bug\n\
         on-disk content:\n{on_disk}"
    );
    let deadline = Instant::now() + CONFLICT_CONVERGE_TIMEOUT;
    loop {
        let grep_hits = grep_plain_matches(&shared_picker, "<<<<<<< ");
        if !grep_hits.contains(&"conflict.rs".to_string()) {
            break;
        }
        if Instant::now() >= deadline {
            panic!(
                "picker grep(\"<<<<<<< \") after resolve still returns conflict.rs \
                 after {} — mmap/overlay cache is stale\n\
                 (worktree on disk has no conflict markers — this is a real \
                  content-invalidation bug)\n\
                 last grep hits: {:?}",
                humantime(CONFLICT_CONVERGE_TIMEOUT),
                grep_hits,
            );
        }
        std::thread::sleep(CONVERGE_POLL);
    }
}

/// Seed a repo with a single file that we'll later produce a conflict in.
fn seed_conflict_repo(base: &Path) {
    fs::write(base.join("README.md"), "# merge conflict test\n").unwrap();
    fs::write(
        base.join("conflict.rs"),
        "fn flavour() {\n    \"BASE\"\n}\n",
    )
    .unwrap();
    git(base, &["init", "-b", "main"]);
    git(base, &["config", "user.email", "fuzz@fff.test"]);
    git(base, &["config", "user.name", "fuzz"]);
    // Conflict behaviour must be deterministic regardless of merge.tool.
    git(base, &["config", "merge.conflictstyle", "merge"]);
    git(base, &["add", "-A"]);
    git(base, &["commit", "-m", "seed", "--no-gpg-sign"]);
}

/// Poll the picker for `relative` until `predicate` holds on its
/// `git_status`, or the timeout expires. On expiry returns an `Err` with
/// the last observed status + truth status for diagnostics.
fn expect_file_status(
    shared: &SharedFilePicker,
    base: &Path,
    relative: &str,
    predicate: impl Fn(Option<Status>) -> bool,
    timeout: Duration,
    what: &str,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    let mut last_picker;
    let mut last_truth;
    loop {
        let picker_status = probe_single_file_status(shared, relative).flatten();
        let truth_status = read_truth_status(base).get(relative).copied();
        last_picker = picker_status;
        last_truth = truth_status;
        if predicate(picker_status) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "timed out after {} waiting for `{relative}` to satisfy `{what}`\n\
                 last picker status: {:?}\n\
                 last truth status : {:?}",
                humantime(timeout),
                last_picker,
                last_truth,
            ));
        }
        std::thread::sleep(CONVERGE_POLL);
    }
}
