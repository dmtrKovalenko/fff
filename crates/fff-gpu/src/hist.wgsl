@group(0) @binding(0) var<storage, read> scores: array<u32>;
@group(0) @binding(1) var<storage, read> survivors: array<vec4<u32>>;
@group(0) @binding(2) var<storage, read> survivor_count: u32;
@group(0) @binding(3) var<storage, read_write> hist: array<atomic<u32>>;

var<workgroup> local: array<atomic<u32>, 4096>;

// Score histogram over survivors, privatized per workgroup: 4096 survivors
// per group land in shared memory, only non-zero bins hit global atomics.
@compute @workgroup_size(256)
fn main(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>, @builtin(local_invocation_id) lid: vec3<u32>) {
    for (var i = lid.x; i < 4096u; i += 256u) { atomicStore(&local[i], 0u); }
    workgroupBarrier();
    let base = (wg.y * nwg.x + wg.x) * 4096u;
    for (var i = lid.x; i < 4096u; i += 256u) {
        let sidx = base + i;
        if (sidx < survivor_count) {
            let s = scores[survivors[sidx].x];
            if (s != 0u) { atomicAdd(&local[min(s, 4095u)], 1u); }
        }
    }
    workgroupBarrier();
    for (var i = lid.x; i < 4096u; i += 256u) {
        let c = atomicLoad(&local[i]);
        if (c != 0u) { atomicAdd(&hist[i], c); }
    }
}
