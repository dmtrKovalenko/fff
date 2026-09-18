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
@group(0) @binding(5) var<storage, read> survivors: array<vec4<u32>>;
@group(0) @binding(6) var<storage, read> survivor_count: u32;

const MAX_NEEDLE: u32 = 32u;
const LANES: u32 = 16u;
const SLOTS: u32 = SLOTS_PLACEHOLDERu;
const WG: u32 = SLOTS * LANES;
const ROWS: u32 = MAX_NEEDLE + 1u;

// Previous chunk per slot: [slot][row][lane] scores and a 16-bit match mask per row.
var<workgroup> adj: array<i32, SLOTS * ROWS * LANES>;
var<workgroup> adj_mm: array<u32, SLOTS * ROWS>;
var<workgroup> row_prev: array<i32, WG>;
var<workgroup> scan_b: array<i32, WG>;
var<workgroup> scan_c: array<i32, WG>;
var<workgroup> mm_sh: array<u32, WG>;
var<workgroup> slot_chunks: array<u32, SLOTS>;

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

fn is_delim(c: u32) -> bool {
    let is_upper = c >= 65u && c <= 90u;
    let is_lower = c >= 97u && c <= 122u;
    let is_digit = c >= 48u && c <= 57u;
    return !(is_upper || is_lower || is_digit || c > 127u);
}

fn slot_mask(base: u32) -> u32 {
    var m = 0u;
    for (var l = 0u; l < LANES; l++) { m |= mm_sh[base + l]; }
    return m;
}

// 16 threads per haystack, one per SIMD lane of frizbee's SSE kernel; four
// haystacks per workgroup. Lane shifts go through workgroup memory.
@compute @workgroup_size(SLOTS_PLACEHOLDER * 16)
fn main(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>, @builtin(local_invocation_id) lid: vec3<u32>) {
    let slot = lid.x / LANES;
    let lane = lid.x % LANES;
    let slot_base = slot * LANES;
    let sidx = (wg.y * nwg.x + wg.x) * SLOTS + slot;
    let valid = sidx < survivor_count;
    let n = min(params.needle_len, MAX_NEEDLE);

    var id = 0u;
    var start = 0u;
    var len = 0u;
    var win_start = 0u;
    var win_end = 0u;
    var chunks = 0u;
    var exact_whole = false;
    if (valid) {
        let sv = survivors[sidx];
        id = sv.x;
        win_start = sv.y;
        win_end = sv.z;
        exact_whole = sv.w != 0u;
        start = items[id].x;
        len = items[id].y;
        chunks = (win_end - win_start + LANES - 1u) / LANES;
    }
    if (lane == 0u) { slot_chunks[slot] = chunks; }

    let match_score = i32(params.match_score + params.mismatch_penalty);
    let mismatch = i32(params.mismatch_penalty);
    let gap_open = i32(params.gap_open - params.gap_extend);
    let gap_extend = i32(params.gap_extend);
    let mcb = i32(params.matching_case_bonus);

    let adj_base = slot * ROWS * LANES;
    let mm_base = slot * ROWS;
    for (var i = lane; i < ROWS * LANES; i += LANES) { adj[adj_base + i] = 0; }
    if (lane == 0u) { for (var i = 0u; i < ROWS; i++) { adj_mm[mm_base + i] = 0u; } }
    workgroupBarrier();

    var max_chunks = 0u;
    for (var q = 0u; q < SLOTS; q++) { max_chunks = max(max_chunks, slot_chunks[q]); }
    var best = 0;
    let lane_bit = 1u << lane;

    for (var ci = 0u; ci < max_chunks; ci++) {
        let in_range = ci < chunks;
        let pos = win_start + ci * LANES + lane;
        var c = 0u;
        if (in_range && pos < win_end) { c = hay_byte(start + pos); }
        var bonus = match_score;
        if (in_range && pos > win_start) {
            let prev_c = hay_byte(start + pos - 1u);
            if (c >= 65u && c <= 90u && prev_c >= 97u && prev_c <= 122u) { bonus += i32(params.capitalization_bonus); }
            if (is_delim(prev_c) && !is_delim(c)) { bonus += i32(params.delimiter_bonus); }
        }
        if (in_range && pos == 0u) { bonus += i32(params.prefix_bonus); }

        var prev_row = 0;
        var up_mm = false;
        var prev_mask = 0u;
        row_prev[lid.x] = 0;
        workgroupBarrier();

        for (var i = 1u; i <= n; i++) {
            let nc = needle_byte(i - 1u);
            let exact = c == nc;
            let m = exact || c == flip_case(nc);

            var d = adj[adj_base + (i - 1u) * LANES + LANES - 1u];
            if (lane > 0u) { d = row_prev[lid.x - 1u]; }
            if (m) { d += bonus; }
            d = max(d - mismatch, 0);
            if (exact) { d += mcb; }
            var u = prev_row - gap_extend;
            if (up_mm) { u -= gap_open; }
            var s = max(d, max(u, 0));

            var mbit = 0u;
            if (m) { mbit = lane_bit; }
            mm_sh[lid.x] = mbit;
            workgroupBarrier();
            // Every lane has consumed row i-1 adjacency; publish this chunk's row i-1.
            adj[adj_base + (i - 1u) * LANES + lane] = prev_row;
            if (lane == 0u) { adj_mm[mm_base + i - 1u] = prev_mask; }
            let row_mask = slot_mask(slot_base);
            let prev_chunk_mask = adj_mm[mm_base + i];

            var g = gap_extend;
            var use_b = true;
            for (var shift = 1u; shift < LANES; shift = shift * 2u) {
                if (use_b) { scan_b[lid.x] = s; } else { scan_c[lid.x] = s; }
                workgroupBarrier();
                var src = 0;
                var src_mm = false;
                if (lane >= shift) {
                    if (use_b) { src = scan_b[lid.x - shift]; } else { src = scan_c[lid.x - shift]; }
                    src_mm = (row_mask & (1u << (lane - shift))) != 0u;
                } else {
                    let al = LANES - shift + lane;
                    src = adj[adj_base + i * LANES + al];
                    src_mm = (prev_chunk_mask & (1u << al)) != 0u;
                }
                var pen = g;
                if (src_mm) { pen += gap_open; }
                s = max(s, max(src - pen, 0));
                g = g * 2;
                use_b = !use_b;
            }

            row_prev[lid.x] = s;
            prev_row = s;
            up_mm = m;
            prev_mask = row_mask;
            workgroupBarrier();
        }

        if (in_range) {
            for (var l = 0u; l < LANES; l++) { best = max(best, row_prev[slot_base + l]); }
        }
        adj[adj_base + n * LANES + lane] = prev_row;
        if (lane == 0u) { adj_mm[mm_base + n] = prev_mask; }
        workgroupBarrier();
    }

    if (valid && lane == 0u) {
        var out = best;
        if (exact_whole && out > 0) { out += i32(params.exact_match_bonus); }
        scores[id] = u32(out);
    }
}
