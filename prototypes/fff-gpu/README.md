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
