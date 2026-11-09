# FFF performance CI

GitHub Actions builds the PR head and its base as static Linux MCP binaries on
hosted runners. The private `dmtrKovalenko/fff-perf` repository sends the binaries
to `load`, which measures one job at a time. PR binaries run in an offline
gVisor sandbox with read-only fixture mounts.

Each run covers the pinned FFF and VS Code revisions on ext4 and Btrfs. It
alternates the two binaries across seven pairs and reports median latencies.
`find_files` and `grep` are measured after warmup and fail the check when any lane
slows down by more than 10%. First-file latency is informational. The workflow
updates one comment per PR and archives the measurements.

At 04:07 UTC the same worker runs a pinned FFF release against a fixed local
reference. Drift over 10% in either direction on a gated metric triggers a
GitHub issue alert. Subsequent drift updates go into the same open issue.

## Prepare the worker

Use the same Linux user for fixture setup and the runner service. The worker
needs Python 3.11 or newer, Git, Docker and gVisor. Register gVisor with Docker
as `fff-perf-gvisor`. The sandbox uses an empty image.

```sh
sudo runsc install --runtime=fff-perf-gvisor
sudo systemctl reload docker
tar -cf - --files-from=/dev/null | docker import - fff-perf-sandbox:1
```

Use an ext4 state directory at `$HOME/.local/share/fff-perf` and mount Btrfs at
its `btrfs` subdirectory. On `load`, Btrfs uses a persistent 8 GiB loop image.
The harness checks the mounted filesystem types and the fixture revisions.
Run these commands from the FFF checkout.

```sh
state="$HOME/.local/share/fff-perf"
mkdir -p "$state/reference"
gh release download v0.11.0 --repo dmtrKovalenko/fff \
  --pattern fff-mcp-x86_64-unknown-linux-musl --dir "$state/reference"
python3 perf/benchmark.py prepare
python3 perf/benchmark.py calibrate --output "$state/calibration.json"
python3 perf/benchmark.py reference --output "$state/reference-check.json"
```

`calibrate` runs the same checksum-verified binary on both sides. The daily
run uses the resulting `reference/baseline.json`. Recalibrate explicitly with
`--replace-reference` when changing the worker settings or pinned workloads.

## Connect GitHub

1. Create the private `dmtrKovalenko/fff-perf` repository with default branch
   `main`. Copy [runner.yml](runner.yml) to `.github/workflows/benchmark.yml`
   and commit it there.
2. On `load`, install the Linux x64 GitHub Actions runner in
   `$HOME/.local/share/fff-perf/runner`. Register it in the private repository
   with the label `fff-perf-load`, then install and start it as a service.
   [GitHub runner setup](https://docs.github.com/en/actions/how-tos/manage-runners/self-hosted-runners/add-runners)
3. Add an Actions secret called `FFF_PERF_RUNNER_TOKEN` to
   `dmtrKovalenko/fff`. Use a fine-grained PAT with access to
   `dmtrKovalenko/fff-perf` and its **Actions: Read and write** permission.
   The public workflow uses its built-in token for statuses, PR comments and
   issue alerts. [Workflow dispatch permissions](https://docs.github.com/en/rest/actions/workflows#create-a-workflow-dispatch-event)
4. Merge the FFF workflow and harness into `main`. Run `Measure FFF` manually in
   the private repository with `kind=reference`. After it succeeds, set
   `FFF_PERF_RUNNER_REPO` in the public repository's Actions variables to
   `dmtrKovalenko/fff-perf` and run `FFF performance` manually to verify
   dispatch and reporting.
5. Verify the `FFF performance` status and comment on a PR. Make that status
   required on `main` once the hosted build and same-binary measurements pass
   on `load`.
