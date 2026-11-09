# FFF core performance CI

Each PR runs FFF's existing Criterion benchmarks against its base revision on
`load`. Fuzzy file search and grep use pinned FFF and VS Code checkouts on ext4
and Btrfs. Each revision also runs selected query parsing, definition matching,
globbing, bigram indexing and SIMD substring search cases.

GitHub-hosted jobs compile both revisions with the base revision's benchmark
sources and the release `zlob` feature. One job at a time runs the resulting
executables on `load` in offline gVisor containers with read-only fixtures.
GitHub Actions updates the PR with Criterion's median timings and fails the
head status when a benchmark slows down by more than 10%.

At 04:07 UTC the worker reruns the same pinned binaries against its saved
reference. A change of more than 10% in either direction opens or updates
the reference drift issue.

## Prepare the worker

Use the runner service account for setup. Register gVisor as the Docker runtime
`fff-perf-gvisor`, create the empty sandbox image, and mount Btrfs at
`$HOME/.local/share/fff-perf/btrfs`. On `load` that mount uses a persistent
8 GiB loop image.

```sh
sudo runsc install --runtime=fff-perf-gvisor
sudo systemctl reload docker
tar -cf - --files-from=/dev/null | docker import - fff-perf-sandbox:1
bash perf/benchmark.sh prepare
```

## Connect GitHub

1. Create the private `dmtrKovalenko/fff-perf` repository. Copy
   [runner.yml](runner.yml) to `.github/workflows/benchmark.yml` on its
   default branch `main`.
2. Register the Linux x64 GitHub Actions runner in
   `$HOME/.local/share/fff-perf/runner` against that private repository.
   Set its label to `fff-perf-load` and start its service.
   [GitHub runner setup](https://docs.github.com/en/actions/how-tos/manage-runners/self-hosted-runners/add-runners)
3. Add `FFF_PERF_RUNNER_TOKEN` as an Actions secret in `dmtrKovalenko/fff`.
   Use a fine-grained PAT scoped to `dmtrKovalenko/fff-perf` with
   **Actions: Read and write**. The public workflow uses its built-in token
   to publish PR statuses, timings and issue alerts.
   [Workflow dispatch permissions](https://docs.github.com/en/rest/actions/workflows#create-a-workflow-dispatch-event)
4. Merge the performance workflow. On a new worker, run **Measure FFF core**
   with `kind=calibrate`, the merged workflow commit as `harness_sha`, and
   `5d21e4bbdf33a09f58975636208e30747bc8d918` as `base_sha`. Calibration
   runs the same executables twice and stores the reference in
   `$HOME/.local/share/fff-perf/core-reference`.
5. Set the public repository's `FFF_PERF_RUNNER_REPO` Actions variable to
   `dmtrKovalenko/fff-perf`. Run **FFF performance** manually and verify the
   daily reference. Verify the status and comment on a PR before making
   `FFF performance` a required status.

When the fixtures, runner settings or benchmark script change, archive the
old `core-reference` directory and run calibration again.
