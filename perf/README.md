# FFF core performance CI

Each PR runs FFF's existing Criterion benchmarks against its base revision on
`load`. Fuzzy file search and grep use pinned FFF and VS Code checkouts on ext4
and Btrfs. Each revision also runs selected query parsing, definition matching,
globbing, bigram indexing and SIMD substring search cases.

GitHub-hosted jobs compile both revisions with the default branch's benchmark
sources and the release `zlob` feature. After the hosted **Spelling** workflow
runs for the current PR commit, one job at a time runs the resulting
executables directly on `load` as the runner service user.
When a timing moves by more than 10%, the worker takes two more measurements
of that benchmark group and uses the median across the three runs. GitHub
Actions updates the PR with the timings and fails the head status when a
benchmark slows down by more than 10%.

At 04:07 UTC the worker reruns the same pinned binaries against its saved
reference. A change of more than 10% in either direction opens or updates
the reference drift issue.

## Prepare the worker

Use the runner service account for setup. Mount Btrfs at
`$HOME/.local/share/fff-perf/btrfs`. On `load` that mount uses a persistent
8 GiB loop image, and the benchmark processes use CPUs 8–11.

```sh
bash perf/benchmark.sh prepare
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
3. Merge the performance workflow. On a new worker, run **FFF performance**
   with `kind=calibrate`. Calibration runs the pinned revision twice and
   stores the reference in `$HOME/.local/share/fff-perf/core-reference-native`.
4. Run **FFF performance** with `kind=reference` to verify the saved
   reference. Verify the status and comment on a PR before making
   `FFF performance` a required status.

When the fixtures, runner settings or benchmark script change, archive the
old `core-reference-native` directory and run calibration again.

The PR comment includes the base and head commit hashes used for the comparison.
