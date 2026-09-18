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
    _pad: u32,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> needle: array<u32>;
@group(0) @binding(2) var<storage, read> hay: array<u32>;
@group(0) @binding(3) var<storage, read> items: array<vec2<u32>>;
@group(0) @binding(4) var<storage, read_write> scores: array<u32>;
// Survivors: [id, win_start, win_end, exact_whole]; count in indirect.x (workgroups).
@group(0) @binding(5) var<storage, read_write> survivors: array<vec4<u32>>;
@group(0) @binding(6) var<storage, read_write> survivor_count: atomic<u32>;

const MAX_NEEDLE: u32 = 32u;

fn hay_byte(idx: u32) -> u32 {
    return (hay[idx >> 2u] >> ((idx & 3u) * 8u)) & 0xffu;
}

fn needle_byte(idx: u32) -> u32 {
    return (needle[idx >> 2u] >> ((idx & 3u) * 8u)) & 0xffu;
}

fn flip_case(c: u32) -> u32 {
    if (c >= 97u && c <= 122u) { return c - 32u; }
    if (c >= 65u && c <= 90u) { return c + 32u; }
    return c;
}

// One thread per haystack: frizbee's 0-typo prefilter. Needle must be a
// subsequence; emits the [first needle[0] .. last needle[n-1]] window.
@compute @workgroup_size(64)
fn main(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>, @builtin(local_invocation_id) lid: vec3<u32>) {
    let id = (wg.y * nwg.x + wg.x) * 64u + lid.x;
    if (id >= params.item_count) { return; }
    scores[id] = 0u;

    let start = items[id].x;
    let len = items[id].y;
    let n = min(params.needle_len, MAX_NEEDLE);
    if (len < n) { return; }

    var win_start = 0u;
    var k = 0u;
    for (var j = 0u; j < len && k < n; j++) {
        let c = hay_byte(start + j);
        let nc = needle_byte(k);
        if (c == nc || c == flip_case(nc)) {
            if (k == 0u) { win_start = j; }
            k++;
        }
    }
    if (k < n) { return; }
    let exact_whole = len == n;

    var win_end = len;
    let last = needle_byte(n - 1u);
    for (var j = len; j > 0u; j--) {
        let c = hay_byte(start + j - 1u);
        if (c == last || c == flip_case(last)) { win_end = j; break; }
    }

    let slot = atomicAdd(&survivor_count, 1u);
    survivors[slot] = vec4<u32>(id, win_start, win_end, u32(exact_whole));
}
