@group(0) @binding(0) var<storage, read> scores: array<u32>;
@group(0) @binding(1) var<storage, read> survivors: array<vec4<u32>>;
@group(0) @binding(2) var<storage, read> survivor_count: u32;
@group(0) @binding(3) var<storage, read_write> sel: array<atomic<u32>, 4>;
@group(0) @binding(4) var<storage, read_write> out: array<vec4<u32>>;
@group(0) @binding(5) var<storage, read_write> ties: array<vec2<u32>>;
@group(0) @binding(6) var<storage, read_write> tie_hist: array<atomic<u32>>;
@group(0) @binding(7) var<storage, read> item_meta: array<u32>;

const OUT_CAP: u32 = 1024u;
const TIE_CAP: u32 = 16384u;

// Everything above the threshold goes to `out`; ids exactly at it go to
// `ties`, and the host picks the lowest ids among them.
@compute @workgroup_size(64)
fn main(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>, @builtin(local_invocation_id) lid: vec3<u32>) {
    let sidx = (wg.y * nwg.x + wg.x) * 64u + lid.x;
    if (sidx >= survivor_count) { return; }
    let id = survivors[sidx].x;
    let s = scores[id];
    if (s == 0u) { return; }
    let t = atomicLoad(&sel[0]);
    if (s > t) {
        let slot = atomicAdd(&sel[2], 1u);
        if (slot < OUT_CAP) { out[slot] = vec4<u32>(s, id, item_meta[id], 0u); }
    } else if (s == t) {
        let slot = atomicAdd(&sel[3], 1u);
        if (slot < TIE_CAP) { ties[slot] = vec2<u32>(id, item_meta[id]); }
        atomicAdd(&tie_hist[id >> 12u], 1u);
    }
}
