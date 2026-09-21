struct Params {
    needle_len: u32,
    item_count: u32,
    match_score: u32,
    mismatch_penalty: u32,
    gap_open: u32,
    gap_extend: u32,
    prefix_bonus: u32,
    delimiter_bonus: u32,
    capitalization_bonus: u32,
    matching_case_bonus: u32,
    exact_match_bonus: u32,
    thread_count: u32,
    _reserved: u32,
    top_k: u32,
    tie_bucket: u32,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> scores: array<u32>;
@group(0) @binding(2) var<storage, read> survivors: array<vec4<u32>>;
@group(0) @binding(3) var<storage, read> survivor_count: u32;
@group(0) @binding(4) var<storage, read> sel: array<u32, 4>;
@group(0) @binding(5) var<storage, read_write> counter: atomic<u32>;
@group(0) @binding(6) var<storage, read_write> ties: array<vec2<u32>>;
@group(0) @binding(7) var<storage, read> item_meta: array<u32>;

const TIE_CAP: u32 = 16384u;

// Second pass for oversized tie groups: only ids at the threshold whose
// 4096-id bucket is at or below the one holding the k-th id.
@compute @workgroup_size(64)
fn main(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>, @builtin(local_invocation_id) lid: vec3<u32>) {
    let sidx = (wg.y * nwg.x + wg.x) * 64u + lid.x;
    if (sidx >= survivor_count) { return; }
    let id = survivors[sidx].x;
    if (scores[id] != sel[0] || (id >> 12u) > params.tie_bucket) { return; }
    let slot = atomicAdd(&counter, 1u);
    if (slot < TIE_CAP) { ties[slot] = vec2<u32>(id, item_meta[id]); }
}
