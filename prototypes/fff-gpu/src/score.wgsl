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

const MAX_NEEDLE: u32 = 32u;
const LANES: u32 = 16u;

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

// One thread per haystack. Emulates frizbee's 16-lane SSE kernel exactly:
// row-major over 16-byte chunks with a log-step horizontal gap scan.
@compute @workgroup_size(64)
fn main(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>, @builtin(local_invocation_id) lid: vec3<u32>) {
    let id = (wg.y * nwg.x + wg.x) * 64u + lid.x;
    if (id >= params.item_count) { return; }

    let start = items[id].x;
    let len = items[id].y;
    let n = min(params.needle_len, MAX_NEEDLE);

    // Prefilter: needle must be a subsequence; SW runs on the window
    // [first needle[0] .. last needle[n-1]] with bonus state reset at its start.
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
    if (k < n) { scores[id] = 0u; return; }
    var win_end = len;
    let last = needle_byte(n - 1u);
    for (var j = len; j > 0u; j--) {
        let c = hay_byte(start + j - 1u);
        if (c == last || c == flip_case(last)) { win_end = j; break; }
    }

    let match_score = i32(params.match_score + params.mismatch_penalty);
    let mismatch = i32(params.mismatch_penalty);
    let gap_open = i32(params.gap_open - params.gap_extend);
    let gap_extend = i32(params.gap_extend);
    let mcb = i32(params.matching_case_bonus);

    // Previous chunk's full rows (adjacency for diag and the gap scan).
    var adj: array<i32, 528>;
    var adj_mm: array<bool, 528>;
    for (var i = 0u; i < (n + 1u) * LANES; i++) { adj[i] = 0; adj_mm[i] = false; }

    var prev_row: array<i32, 16>;
    var up_mm: array<bool, 16>;
    var row: array<i32, 16>;
    var tmp: array<i32, 16>;
    var mm: array<bool, 16>;
    var chars: array<u32, 16>;
    var bonus: array<i32, 16>;

    var best = 0;
    var prev_is_lower = false;
    var prev_is_delim = false;
    var exact_whole = len == n;
    let win_len = win_end - win_start;
    let chunks = (win_len + LANES - 1u) / LANES;

    for (var ci = 0u; ci < chunks; ci++) {
        let base = win_start + ci * LANES;
        for (var l = 0u; l < LANES; l++) {
            var c = 0u;
            if (base + l < win_end) { c = hay_byte(start + base + l); }
            chars[l] = c;
            let is_upper = c >= 65u && c <= 90u;
            let is_lower = c >= 97u && c <= 122u;
            let is_digit = c >= 48u && c <= 57u;
            let is_delim = !(is_upper || is_lower || is_digit || c > 127u);
            var b = match_score;
            if (is_upper && prev_is_lower) { b += i32(params.capitalization_bonus); }
            if (prev_is_delim && !is_delim) { b += i32(params.delimiter_bonus); }
            if (base + l == 0u) { b += i32(params.prefix_bonus); }
            bonus[l] = b;
            prev_is_lower = is_lower;
            prev_is_delim = is_delim;
            if (exact_whole && base + l < len && c != needle_byte(base + l) && flip_case(c) != needle_byte(base + l)) {
                exact_whole = false;
            }
        }

        for (var l = 0u; l < LANES; l++) { prev_row[l] = 0; up_mm[l] = false; }

        for (var i = 1u; i <= n; i++) {
            let nc = needle_byte(i - 1u);
            let fc = flip_case(nc);
            let ai = i * LANES;
            let diag_adj = adj[(i - 1u) * LANES + LANES - 1u];

            for (var l = 0u; l < LANES; l++) {
                let c = chars[l];
                let exact = c == nc;
                let m = exact || c == fc;
                var d = diag_adj;
                if (l > 0u) { d = prev_row[l - 1u]; }
                if (m) { d += bonus[l]; }
                d = max(d - mismatch, 0);
                if (exact) { d += mcb; }
                var u = prev_row[l] - gap_extend;
                if (up_mm[l]) { u -= gap_open; }
                u = max(u, 0);
                row[l] = max(d, u);
                mm[l] = m;
            }

            var g = gap_extend;
            for (var shift = 1u; shift < LANES; shift = shift * 2u) {
                for (var l = 0u; l < LANES; l++) {
                    var src = 0;
                    var src_mm = false;
                    if (l >= shift) {
                        src = row[l - shift];
                        src_mm = mm[l - shift];
                    } else {
                        src = adj[ai + LANES - shift + l];
                        src_mm = adj_mm[ai + LANES - shift + l];
                    }
                    var pen = g;
                    if (src_mm) { pen += gap_open; }
                    tmp[l] = max(row[l], max(src - pen, 0));
                }
                for (var l = 0u; l < LANES; l++) { row[l] = tmp[l]; }
                g = g * 2;
            }

            // Row i-1's adjacency is no longer needed; publish row i-1 of this chunk.
            for (var l = 0u; l < LANES; l++) {
                adj[(i - 1u) * LANES + l] = prev_row[l];
                adj_mm[(i - 1u) * LANES + l] = up_mm[l];
                prev_row[l] = row[l];
                up_mm[l] = mm[l];
            }
        }
        for (var l = 0u; l < LANES; l++) {
            adj[n * LANES + l] = row[l];
            adj_mm[n * LANES + l] = mm[l];
            best = max(best, row[l]);
        }
    }

    if (exact_whole && best > 0) { best += i32(params.exact_match_bonus); }
    scores[id] = u32(best);
}
