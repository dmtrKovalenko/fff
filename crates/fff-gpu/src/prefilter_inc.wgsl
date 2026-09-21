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
    _pad0: u32,
    _pad1: u32,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> needle: array<u32>;
@group(0) @binding(2) var<storage, read> items: array<vec2<u32>>;
@group(0) @binding(3) var<storage, read_write> scores: array<u32>;
@group(0) @binding(4) var<storage, read_write> survivors: array<vec4<u32>>;
@group(0) @binding(5) var<storage, read_write> survivor_count: atomic<u32>;
@group(0) @binding(6) var<storage, read> soa_start: array<u32>;
@group(0) @binding(7) var<storage, read> hay_soa: array<u32>;
@group(0) @binding(8) var<storage, read> survivors_in: array<vec4<u32>>;
@group(0) @binding(9) var<storage, read> count_in: u32;

var<workgroup> wneedle: array<u32, 16>;

const MAX_NEEDLE: u32 = 64u;

fn needle_byte(idx: u32) -> u32 {
    return (wneedle[idx >> 2u] >> ((idx & 3u) * 8u)) & 0xffu;
}

fn flip_case(c: u32) -> u32 {
    if (c >= 97u && c <= 122u) { return c - 32u; }
    if (c >= 65u && c <= 90u) { return c + 32u; }
    return c;
}

fn step(c: u32, j: u32, k: ptr<function, u32>, nc: ptr<function, u32>, fc: ptr<function, u32>,
        n: u32, win_start: ptr<function, u32>, last: u32, flast: u32, last_pos: ptr<function, u32>) {
    if (*k < n && (c == *nc || c == *fc)) {
        if (*k == 0u) { *win_start = j; }
        *k = *k + 1u;
        if (*k < n) { *nc = needle_byte(*k); *fc = flip_case(*nc); }
    }
    if (c == last || c == flast) { *last_pos = j + 1u; }
}

// Needle grew by appending: only the previous needle's survivors can match,
// so rescan those (one thread each) and zero their old scores.
@compute @workgroup_size(64)
fn main(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>, @builtin(local_invocation_id) lid: vec3<u32>) {
    if (lid.x < 16u) { wneedle[lid.x] = needle[lid.x]; }
    workgroupBarrier();
    let sidx = (wg.y * nwg.x + wg.x) * 64u + lid.x;
    if (sidx >= count_in) { return; }
    let id = survivors_in[sidx].x;
    scores[id] = 0u;

    let len = items[id].y;
    let n = min(params.needle_len, MAX_NEEDLE);
    if (len < n) { return; }

    let last = needle_byte(n - 1u);
    let flast = flip_case(last);
    var nc = needle_byte(0u);
    var fc = flip_case(nc);
    var win_start = 0u;
    var last_pos = 0u;
    var k = 0u;
    let base = soa_start[id];
    let words = (len + 3u) >> 2u;
    let full = len >> 2u;
    for (var w = 0u; w < full; w++) {
        let v = hay_soa[base + w * 64u];
        let j = w * 4u;
        step(v & 0xffu, j, &k, &nc, &fc, n, &win_start, last, flast, &last_pos);
        step((v >> 8u) & 0xffu, j + 1u, &k, &nc, &fc, n, &win_start, last, flast, &last_pos);
        step((v >> 16u) & 0xffu, j + 2u, &k, &nc, &fc, n, &win_start, last, flast, &last_pos);
        step(v >> 24u, j + 3u, &k, &nc, &fc, n, &win_start, last, flast, &last_pos);
    }
    if (full < words) {
        let v = hay_soa[base + full * 64u];
        let j = full * 4u;
        for (var b = 0u; j + b < len; b++) {
            step((v >> (b * 8u)) & 0xffu, j + b, &k, &nc, &fc, n, &win_start, last, flast, &last_pos);
        }
    }
    if (k < n) { return; }

    let slot = atomicAdd(&survivor_count, 1u);
    survivors[slot] = vec4<u32>(id, win_start, last_pos, u32(len == n));
}
