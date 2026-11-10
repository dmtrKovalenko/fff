# FFF core performance CI

Each PR runs FFF's existing Criterion benchmarks against its base revision on
`load`. Fuzzy file search and grep use pinned FFF and VS Code checkouts on ext4
and Btrfs. Each revision also runs selected query parsing, definition matching,
globbing, bigram indexing and SIMD substring search cases.

A PR is measured only when it changes Rust sources or `Cargo.toml` in
`fff-core`, `fff-grep` or `fff-query-parser`, the root `Cargo.toml`,
`Cargo.lock` or `.cargo/config.toml`. Other PRs get a passing
`FFF performance` status marked as skipped.

GitHub-hosted jobs compile both revisions with the default branch's benchmark
sources and the release `zlob` feature. After the hosted **Spelling** workflow
runs for the current PR commit, one job at a time runs the resulting
executables directly on `load` as the runner service user.
When a timing moves by more than 10%, the worker takes two more measurements
of that benchmark group and uses the median across the three runs. GitHub
Actions updates the PR with the timings and fails the head status when a
benchmark slows down by more than 10%. `perf/report.sh` writes that comment:
a 🔴/🟢/⚪ mark per benchmark, a chart of PR time against base, and the
full table.

The chart is drawn with [fframes](https://github.com/dmtrKovalenko/fframes)
by `perf/chart`, a package outside the main workspace. The image is uploaded
as `pr-<number>.png` to the `perf-charts` prerelease, so it stays out of the
git history. To draw one locally:

```sh
cargo run --release --manifest-path perf/chart/Cargo.toml --example chart -- \
  base.bench head.bench chart.png
```

At 04:07 UTC **FFF performance reference** runs directly on `load` with
the saved binaries. A change of more than 10% in either direction opens
or updates the reference drift issue.

## Prepare the worker

Use the runner service account for setup. Mount Btrfs at
`$HOME/.local/share/fff-perf/btrfs`. On `load` that mount uses a persistent
8 GiB loop image, and the benchmark processes use CPUs 8–11.

```sh
bash perf/benchmark.sh prepare
```

Then reserve the benchmark CPUs as root while the runner is idle, passing
the runner's systemd unit:

```sh
sudo bash perf/tune.sh actions.runner.dmtrKovalenko-fff.load.service
```

It keeps every other service off CPUs 8–11, takes their hyperthread siblings
offline, sets the `performance` governor and turns turbo off. The settings
persist across reboots. `benchmark.sh` refuses to measure until they hold.

To see how noisy the worker is, compare one set of binaries with itself a
few times. Every run should report all benchmarks within ±10%:

```sh
bins=$HOME/.local/share/fff-perf/core-reference-native/bins
for i in 1 2 3 4 5; do
  bash perf/benchmark.sh compare "/tmp/fff-noise/$i" "$bins" "$bins"
  bash perf/report.sh /tmp/fff-noise/$i/{base,head}.bench | head -n 1
done
```

## Connect GitHub

1. In `dmtrKovalenko/fff`, set **Settings → Actions → General → Approval
   for running fork pull request workflows from contributors** to
   **Require approval for all external contributors**. Use the PR's
   **Approve workflows to run** button for fork commits you want to benchmark.
   [GitHub fork approvals](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/approve-runs-from-forks)
2. Register the Linux x64 GitHub Actions runner in
   `$HOME/.local/share/fff-perf/runner` against `dmtrKovalenko/fff`.
   Set its label to `fff-perf-load` and start its service.
   [GitHub runner setup](https://docs.github.com/en/actions/how-tos/manage-runners/self-hosted-runners/add-runners)
3. Merge the performance workflows. On a new worker, run **FFF performance**
   manually to calibrate. Calibration runs the pinned revision twice and
   stores the reference in `$HOME/.local/share/fff-perf/core-reference-native`.
4. Run **FFF performance reference** manually to verify the saved
   reference. Verify the status and comment on a PR before making
   `FFF performance` a required status.

When the fixtures, runner settings, CPU tuning or benchmark script change, archive the
old `core-reference-native` directory and run calibration again.
