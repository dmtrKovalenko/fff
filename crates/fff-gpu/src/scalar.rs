use std::fmt::Write;

const HEADER: &str = r#"struct Params {
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
@group(0) @binding(2) var<storage, read> hay: array<u32>;
@group(0) @binding(3) var<storage, read> items: array<vec2<u32>>;
@group(0) @binding(4) var<storage, read_write> scores: array<u32>;
@group(0) @binding(5) var<storage, read> survivors: array<vec4<u32>>;
@group(0) @binding(6) var<storage, read> survivor_count: u32;
@group(0) @binding(7) var<storage, read_write> item_meta: array<u32>;
@group(0) @binding(8) var<storage, read> fname_off: array<u32>;
@group(0) @binding(9) var<storage, read> boost: array<i32>;

const NEG: i32 = -1000000;

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

// One thread per survivor, column by column; needle length is baked in so
// every row's state lives in registers. Horizontal gaps use a running
// max of (source - open*matched + col*extend) instead of a lane scan.
@compute @workgroup_size(64)
fn main(
    @builtin(workgroup_id) wg: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
    @builtin(local_invocation_id) lid: vec3<u32>,
) {
    let sidx = (wg.y * nwg.x + wg.x) * 64u + lid.x;
    if (sidx >= survivor_count) { return; }
    let sv = survivors[sidx];
    let id = sv.x;
    let win_start = sv.y;
    let win_end = sv.z;
    let start = items[id].x;
    let len = items[id].y;

    let match_score = i32(params.match_score + params.mismatch_penalty);
    let mismatch = i32(params.mismatch_penalty);
    let gap_open = i32(params.gap_open - params.gap_extend);
    let gap_extend = i32(params.gap_extend);
    let mcb = i32(params.matching_case_bonus);
    var best = 0;
    var best_pos = 0u;
    var prev_c = 0u;
    if (win_start > 0u) { prev_c = hay_byte(start + win_start - 1u); }
"#;

pub fn generate(n: usize) -> String {
    let mut s = String::from(HEADER);
    for i in 1..=n {
        let _ = writeln!(s, "    var h{i} = 0;\n    var g{i} = NEG;");
    }
    for i in 0..n {
        let _ = writeln!(
            s,
            "    let nc{i} = needle_byte({i}u);\n    let fc{i} = flip_case(nc{i});"
        );
    }
    s.push_str(
        r#"
    var wi = (start + win_start) >> 2u;
    var word = hay[wi];
    var shift = ((start + win_start) & 3u) * 8u;
    for (var pos = win_start; pos < win_end; pos++) {
        let c = (word >> shift) & 0xffu;
        shift += 8u;
        if (shift == 32u) { wi++; word = hay[wi]; shift = 0u; }
        var bonus = match_score;
        if (c >= 65u && c <= 90u && prev_c >= 97u && prev_c <= 122u) { bonus += i32(params.capitalization_bonus); }
        if (pos > 0u && is_delim(prev_c) && !is_delim(c)) { bonus += i32(params.delimiter_bonus); }
        if (pos == 0u) { bonus += i32(params.prefix_bonus); }
        prev_c = c;
        let col = i32(pos - win_start);
        let colg = col * gap_extend;
        var diag = 0;
        var up = 0;
        var up_mm = false;
        var s = 0;
"#,
    );
    for i in 1..=n {
        let p = i - 1;
        let _ = write!(
            s,
            r#"        {{
            let exact = c == nc{p};
            let m = exact || c == fc{p};
            var d = diag;
            if (m) {{ d += bonus; }}
            d = max(d - mismatch, 0);
            if (exact) {{ d += mcb; }}
            var u = up - gap_extend;
            if (up_mm) {{ u -= gap_open; }}
            let pre = max(d, max(u, 0));
            s = max(pre, max(g{i} - colg, 0));
            g{i} = max(g{i}, pre - select(0, gap_open, m) + colg);
            diag = h{i};
            h{i} = s;
            up = s;
            up_mm = m;
        }}
"#
        );
    }
    s.push_str(
        r#"        if (s > best) { best = s; best_pos = pos; }
    }
    if (sv.w != 0u && best > 0) { best += i32(params.exact_match_bonus); }
    // Same boosts the CPU scorer applies, so the top-k set is picked on the
    // final ordering: filename / exact-filename bonus, frecency + git as a percent.
    var total = best;
    if (best > 0) {
        let n = params.needle_len;
        let fo = fname_off[id];
        let in_fname = best_pos + 1u >= n && best_pos + 1u - n >= fo;
        if (in_fname && n == len - fo) { total += best / 5 * 2; }
        else if (in_fname) { total += min(best / 6, 30); }
        total += best * boost[id] / 100;
    }
    scores[id] = u32(clamp(total, 0, 4095));
    item_meta[id] = best_pos | (sv.w << 16u);
}
"#,
    );
    s
}
