# fff-gpu — GPU fuzzy path matching prototype

Standalone experiment (not part of the workspace): frizbee's Smith-Waterman
file-path scoring ported to a `wgpu` compute pipeline, benchmarked against
`neo_frizbee` on the same index.

```
cd prototypes/fff-gpu
cargo run --release -- <dir> [--sizes 0,200000,1000000] [--kernel item|lanes|sub] [--threads N] [--queries a,b,c]
```

`--sizes` replicates the walked paths under `pkgN/` prefixes up to the target
count. `DBG=1` dumps reference-vs-frizbee score mismatches and exits;
`SHOW=1` prints GPU-vs-reference mismatches inline.

## Pipeline

1. `prefilter.wgsl` — thread per path. Frizbee's 0-typo prefilter: needle must
   be a case-insensitive subsequence; emits the `[first needle[0] .. last
   needle[n-1]]` window and appends survivors via an atomic counter.
2. `args.wgsl` — one thread; turns the survivor count into a 2D indirect dispatch.
3. DP kernel over survivors only. Three variants:
   - `score.wgsl` (`item`) — thread per path, 16-lane SIMD emulated in private
     arrays. Correct but spills to scratch; slowest.
   - `score_lanes.wgsl` (`lanes`) — 16 threads per path, one per lane; shifts
     through workgroup memory with barriers. Works on any subgroup size.
   - `subgroup.rs` (`sub`) — generated per needle length so the row loop
     unrolls; lane shifts via `subgroupShuffle`/`subgroupBallot`, cross-chunk
     state packed two rows per register. Falls back to `lanes` at runtime if
     the driver picks a subgroup narrower than 16 (Intel does for n ≥ 9).

`reference.rs` is a scalar twin of the shader. Scores are compared three ways:
shader vs reference (must be 100%), shader vs frizbee, and top-50 set overlap.

The kernels reproduce frizbee's SSE kernel exactly, including its log-step
horizontal gap scan (which can double-count gap-open across shift steps and
so scores slightly below a textbook Gotoh recurrence). Residual 0.1–0.3%
score disagreements with frizbee are in ties/long-path greedy fallbacks and
never changed a top-50 set in testing.

## Results (Intel UHD 770 iGPU, i9 28 threads, lightsource paths, avg 75 bytes)

Median of 20, full round trip (needle upload → dispatch → score readback).

| paths | query | GPU (sub) | frizbee 28t | frizbee 1t |
|------:|-------|----------:|------------:|-----------:|
| 13K   | main       | 2.9ms  | 0.35ms | |
| 13K   | picker_ui  | 0.60ms | 0.39ms | |
| 200K  | main       | 17ms   | 1.0ms  | 6.4ms |
| 200K  | score      | 31ms   | 1.3ms  | 10.7ms |
| 200K  | picker_ui  | 7.4ms  | 0.6ms  | 2.2ms |
| 200K  | grep_bench | 6.2ms  | 0.5ms  | 2.5ms |
| 1M    | main       | 85ms   | 3.9ms  | |
| 1M    | score      | 152ms  | 5.6ms  | |
| 1M    | picker_ui  | 36ms   | 2.2ms  | |

Top-50 overlap was 50/50 on every query at every size.

## Conclusions

- Porting is feasible and the scoring can be made bit-exact with frizbee.
- On an Intel iGPU it loses to frizbee by ~15–25× against 28 threads and
  ~3× against a *single* AVX2 core. The prefilter pass alone (~5ms / 200K)
  costs more than frizbee's whole search.
- Round-trip floor on this box is ~0.25ms, so even a perfect kernel cannot
  beat the CPU below ~50K paths.
- Where a GPU could plausibly win: discrete cards (roughly 50–100× the ALU
  throughput and native 32-wide subgroups, so the `sub` kernel would apply at
  every needle length) on indexes of ≥1M paths, or content grep where the
  corpus is resident in VRAM. Neither is testable on this machine.
- Kernel lessons: never emulate SIMD lanes in private arrays; keep the
  prefilter thread-per-item and compact before the DP (16 lanes doing the
  same scalar prefilter was the single biggest waste); barriers across more
  than one hardware thread dominate the DP; unrolling on needle length is
  what makes register-resident cross-chunk state possible.

## Results (Apple M4 Max, 16 threads, chromium paths, avg 77 bytes)

Unified memory changes the picture: readback of 1M scores is ~50µs and
submit ~40µs, so GPU time is the kernel itself (~1.1ms prefilter over 80MB
of paths plus 0.3–3.5ms DP depending on survivor count).

| paths | query | GPU (sub) | frizbee 16t |
|------:|-------|----------:|------------:|
| 497K  | main      | 2.5ms | 2.3ms |
| 497K  | picker_ui | 0.6ms | 0.8ms |
| 1M    | main      | 4.7ms | 4.8ms |
| 1M    | score     | 6.6ms | 6.4ms |
| 1M    | picker_ui | 1.1ms | 1.6ms |

GPU and 16 CPU cores are within noise of each other at every size.

### Hybrid split (`--hybrid`)

`hybrid.rs` splits the index: the GPU scores the first `f` of the paths while
frizbee scores the rest on 15 threads, and the two top-50 sets are merged.
1M chromium paths, median of 20:

| query | cpu only | gpu 25% | gpu 40% | gpu 50% | gpu only |
|-------|---------:|--------:|--------:|--------:|---------:|
| main    | 5.2ms | 3.8ms | 3.7ms | 4.2ms | 7.6ms |
| score   | 6.9ms | 5.1ms | 4.7ms | 4.3ms | 9.2ms |
| LICENSE | 4.6ms | 3.6ms | 3.7ms | 3.5ms | 5.7ms |
| picker_ui | 1.6ms | 1.3ms | 1.9ms | 1.0ms | 1.9ms |

Best case is ~1.4× over CPU-only with a 30–40% GPU share. The adaptive
balancer (re-splits from each side's per-path throughput) converges to
~33% within a few queries. The GPU partition runs 20–30% slower while the
CPU is saturated: both draw from the same package power and memory
bandwidth, so the overlap is not free and the ceiling is ~1.5×, not the
sum of the two.

### What the real search costs (`bench_in_memory`, fff-core, 497K chromium files, 16 threads)

`crates/fff-nvim/src/bin/bench_in_memory.rs <dir>` runs `FilePicker::fuzzy_search`
over the in-memory index. Per keystroke, median of 20:

| query | total | frizbee (2 typos) | score loop | sort+paginate | matched |
|-------|------:|------------------:|-----------:|--------------:|--------:|
| main      | 14.1ms | 6.4ms  | 3.7ms | 3.8ms | 494K |
| picker_ui | 10.4ms | 7.5ms  | 1.6ms | 1.3ms | 200K |
| LICENSE   | 17.8ms | 10.1ms | 3.5ms | 2.8ms | 451K |
| srcfilepickerrs | 10.2ms | 8.1ms | 0.2ms | 0.1ms | 25K |

Raw 0-typo frizbee on the same paths is 0.8–3ms, so the product spends most
of the keystroke outside the kernel the GPU replaces: typo tolerance defeats
the prefilter (0-typo survivors for `main` at 1M are 459K, `picker_ui` 6K;
with 2 typos nearly every file matches), and the per-match scoring loop and
sort are single-threaded over ~500K matches.

Scaling the measured DP throughput to all 1M paths (no prefilter, as typo
matching requires) gives ~9ms for `main`, ~19ms for `LICENSE`, ~25ms for
`picker_ui` on the M4 Max, against 9.9 / 15.9 / 13.8ms for 2-typo frizbee
on 16 threads. The GPU does not change the picture for the real search.

## Kernel work on Apple silicon (M4 Max)

Default kernel is now `scalar`; `--kernel sub` keeps the frizbee-exact path.

- `prefilter_v3.wgsl` — paths stored a second time interleaved in 64-wide
  groups (`pack_paths_soa`, length-sorted within 4096-path blocks, 3%
  padding) so a workgroup's loads are contiguous; one u32 per load, one
  pass tracking the subsequence cursor and the last `needle[n-1]` together;
  needle in workgroup memory. 1M paths: 1.1ms → 0.27ms, memory-bound.
- `subgroup.rs` — generated for the hardware width: 32 lanes on Metal
  (`LANES=16` overrides). Row masks stay one u32 per row at 32 lanes. Still
  bit-exact with the lane-parametrized `reference::score`; 99.9% of scores
  equal frizbee's 16-lane NEON result, top-50 unchanged. DP 1.6× faster.
- `scalar.rs` — one thread per survivor, column by column, needle length
  baked in so every row's H and gap state is a register. Horizontal gaps use
  a running max of `source - open·matched + col·extend` instead of frizbee's
  log-step lane scan, so scores follow the textbook recurrence: 4–19% of
  scores come out a few points above frizbee's (frizbee double-counts
  gap-open on gap lengths that are not powers of two). Validated against
  `reference::score_scalar`. DP 2.8× faster than the 32-lane kernel.
- `GPU_TIMING=1` — timestamp queries per pass (prefilter / DP medians). Wall
  clock swings 2–3× with GPU clock state; the harness bursts 10 searches
  before timing, `REPS=n` sets the sample count, and min is printed.
- Tried and dropped: a per-path character-presence bitmask gate (passes
  35–45% of paths on selective queries, adds divergence, net slower).

Chromium paths, median of 100 (1M) / 60 (4M), full round trip:

| paths | query | GPU | frizbee 16t | prefilter | DP |
|------:|-------|----:|------------:|----------:|---:|
| 1M | main      | 1.17ms | 4.89ms | 0.27ms | 0.57ms |
| 1M | score     | 1.38ms | 6.69ms | 0.28ms | 0.75ms |
| 1M | LICENSE   | 1.01ms | 4.41ms | 0.29ms | 0.42ms |
| 1M | picker_ui | 0.61ms | 1.58ms | 0.27ms | 0.07ms |
| 4M | main      | 3.66ms | 18.1ms | | |
| 4M | score     | 4.45ms | 24.9ms | | |
| 4M | LICENSE   | 3.14ms | 17.6ms | | |
| 4M | picker_ui | 1.63ms | 5.95ms | | |

Round-trip floor is ~0.3ms (submit, scheduling, 4MB score copy). Top-50
overlap with frizbee drops on queries with hundreds of thousands of
equal-score matches (`fpr`): the sets differ inside tie groups, not in rank.

Remaining levers, in order: keep the previous keystroke's survivor list on
the GPU and prefilter only that when the needle extends; reduce readback
with a GPU-side threshold/compaction pass so only candidates come back;
typo support (fff's search allows 2), which removes the prefilter's benefit
and makes the DP cost the whole index.

## End to end (`--e2e`)

`e2e.rs` times query string in → sorted top-50 `(score, id)` out, GPU versus
frizbee + a CPU top-k. Two additions made the GPU side self-contained:

- **Top-k on the GPU.** After the DP: a score histogram (`hist.wgsl`,
  privatized per workgroup), a 256-thread suffix scan that finds the k-th
  score (`select.wgsl`), and a compaction that returns everything above it plus
  the ids tied at it (`compact.wgsl`). Ties resolve to the lowest ids on the
  host; a tie group larger than 16K ids costs one more small pass
  (`compact2.wgsl`) driven by a per-4096-id bucket histogram. Readback drops
  from 4MB to ~80KB. Verified equal to top-k over the full score array.
- **Incremental prefilter.** Survivor lists ping-pong between two buffers;
  when the needle extends the previous one, `prefilter_inc.wgsl` rescans only
  the previous survivors. Verified equal to a from-scratch search.
  `NO_INCREMENTAL=1` disables it.
- The args shader also zeroes the histograms and the next counter, so a search
  is one compute pass plus the upload and readback blits.

Warm, 1M chromium paths, median of 30:

| query | GPU e2e | CPU e2e (16t) | |
|-------|--------:|--------------:|--:|
| p         | 2.71ms | 5.51ms | 2.0× |
| main      | 1.52ms | 5.20ms | 3.4× |
| score     | 1.68ms | 6.90ms | 4.1× |
| LICENSE   | 1.34ms | 4.43ms | 3.3× |
| picker_ui | 1.02ms | 1.56ms | 1.5× |

Typing `picker_ui` warm, per keystroke: GPU 3.8 → 0.69ms, CPU 5.4 → 1.6ms.

Warm, 4M paths: `main` 4.2ms vs 20.7ms, `score` 4.8 vs 26.2, `LICENSE` 3.4 vs
18.1, `picker_ui` 1.8 vs 5.9 (3.3–5.4×).

**At typing cadence** (`CADENCE=100`, 100ms idle between keystrokes) both sides
pay for idle cores and a downclocked GPU, and the burst numbers are 3–5×
optimistic for both:

| prefix | GPU e2e | CPU e2e |
|--------|--------:|--------:|
| p        | 10.0ms | 21.7ms |
| pick     | 5.9ms  | 17.6ms |
| picker_  | 4.2ms  | 16.2ms |
| picker_ui| 3.6ms  | 16.4ms |

Cold, command submission runs ~15× slower and the score copy ~7× slower than
warm, which is why the GPU-side top-k matters more than its warm cost
suggests. A user-interactive QoS on the calling thread (`QOS=1`) and a
keep-alive dispatch while idle (`KEEPALIVE=1|2`) changed nothing measurable.

## Home directory (2.29M paths, real tree, M4 Max)

Needles are capped at 64 bytes (`MAX_NEEDLE`); an unchanged truncated needle
still takes the incremental path. Warm, median of 10:

| query | GPU e2e | CPU e2e (16t) | |
|-------|--------:|--------------:|--:|
| main | 2.8ms | 11.5ms | 4.1× |
| src/file_picker.rs | 0.94ms | 3.1ms | 3.3× |
| dev/fff.nvim/prototypes/fff-gpu | 0.99ms | 2.8ms | 2.9× |
| chromium/content/browser/renderer_host | 1.0ms | 3.2ms | 3.1× |

Typing `dev/fff.nvim/lua/fff/picker_ui.lua` at 100ms cadence, per keystroke:

| prefix | GPU e2e | CPU e2e |
|--------|--------:|--------:|
| d | 16.0ms | 29.0ms |
| dev/fff | 10.2ms | 21.3ms |
| dev/fff.nvim | 3.9ms | 15.0ms |
| dev/fff.nvim/lua/fff | 3.3ms | 14.6ms |
| dev/fff.nvim/lua/fff/picker_ui.lua | 3.8ms | 15.1ms |

Once the prefix is selective the GPU settles at ~3.5ms cold per keystroke,
of which the kernels are a small part; the rest is submission and clock
ramp. The CPU side stays at 14–15ms because 16 threads wake from idle.
