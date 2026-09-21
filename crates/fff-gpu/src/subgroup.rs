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

// 16 lanes per haystack inside one subgroup; needle length is baked in so the
// row loop unrolls and previous-chunk state stays in registers.
@compute @workgroup_size(LANES)
fn main(
    @builtin(workgroup_id) wg: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
    @builtin(subgroup_invocation_id) lane: u32,
    @builtin(subgroup_size) subgroup_size: u32,
) {
    let sidx = wg.y * nwg.x + wg.x;
    if (sidx >= survivor_count) { return; }
    let sv = survivors[sidx];
    let id = sv.x;
    if (subgroup_size < LANES) {
        if (lane == 0u) { scores[id] = 0xffffffffu; }
        return;
    }
    let win_start = sv.y;
    let win_end = sv.z;
    let start = items[id].x;
    let len = items[id].y;
    let chunks = (win_end - win_start + LANES - 1u) / LANES;

    let match_score = i32(params.match_score + params.mismatch_penalty);
    let mismatch = i32(params.mismatch_penalty);
    let gap_open = i32(params.gap_open - params.gap_extend);
    let gap_extend = i32(params.gap_extend);
    let mcb = i32(params.matching_case_bonus);
    var best = 0;
"#;

pub fn generate(n: usize, lanes: u32) -> String {
    let packed_masks = lanes == 16;
    let lane_mask = if packed_masks {
        "0xffffu"
    } else {
        "0xffffffffu"
    };
    let mut s = format!("const LANES: u32 = {lanes}u;\n");
    s.push_str(HEADER);
    for k in 0..=(n / 2) {
        let _ = writeln!(s, "    var pa{k} = 0u;");
    }
    let mask_regs = if packed_masks { n / 2 + 1 } else { n + 1 };
    for k in 0..mask_regs {
        let _ = writeln!(s, "    var pmm{k} = 0u;");
    }
    for i in 0..n {
        let _ = writeln!(
            s,
            "    let nc{i} = needle_byte({i}u);\n    let fc{i} = flip_case(nc{i});"
        );
    }
    s.push_str(
        r#"
    for (var ci = 0u; ci < chunks; ci++) {
        let pos = win_start + ci * LANES + lane;
        var c = 0u;
        if (pos < win_end) { c = hay_byte(start + pos); }
        var bonus = match_score;
        if (pos > win_start) {
            let prev_c = hay_byte(start + pos - 1u);
            if (c >= 65u && c <= 90u && prev_c >= 97u && prev_c <= 122u) { bonus += i32(params.capitalization_bonus); }
            if (is_delim(prev_c) && !is_delim(c)) { bonus += i32(params.delimiter_bonus); }
        }
        if (pos == 0u) { bonus += i32(params.prefix_bonus); }

        var prev_row = 0;
        var up_mm = false;
        var prev_mask = 0u;
        var s = 0;
"#,
    );
    let unpack = |i: usize, reg: &str| {
        if reg == "pmm" && !packed_masks {
            return format!("pmm{i}");
        }
        let k = i / 2;
        if i.is_multiple_of(2) {
            format!("({reg}{k} & 0xffffu)")
        } else {
            format!("({reg}{k} >> 16u)")
        }
    };
    let store = |i: usize, reg: &str, val: &str| {
        if reg == "pmm" && !packed_masks {
            return format!("pmm{i} = {val};");
        }
        let k = i / 2;
        if i.is_multiple_of(2) {
            format!("{reg}{k} = ({reg}{k} & 0xffff0000u) | u32({val});")
        } else {
            format!("{reg}{k} = ({reg}{k} & 0xffffu) | (u32({val}) << 16u);")
        }
    };
    let shifts: Vec<u32> = (0..)
        .map(|e| 1u32 << e)
        .take_while(|&x| x < lanes)
        .collect();
    for i in 1..=n {
        let p = i - 1;
        let ap = unpack(p, "pa");
        let ai = unpack(i, "pa");
        let pmi = unpack(i, "pmm");
        let store_p = format!(
            "{}\n            {}",
            store(p, "pa", "prev_row"),
            store(p, "pmm", "prev_mask")
        );
        let _ = write!(
            s,
            r#"
        {{
            let exact = c == nc{p};
            let m = exact || c == fc{p};
            let left = subgroupShuffleUp(prev_row, 1u);
            let diag_adj = i32(subgroupShuffle({ap}, LANES - 1u));
            var d = select(left, diag_adj, lane == 0u);
            if (m) {{ d += bonus; }}
            d = max(d - mismatch, 0);
            if (exact) {{ d += mcb; }}
            var u = prev_row - gap_extend;
            if (up_mm) {{ u -= gap_open; }}
            s = max(d, max(u, 0));
            let row_mask = subgroupBallot(m).x & {lane_mask};
"#
        );
        for &shift in &shifts {
            let _ = write!(
                s,
                r#"            {{
                let up_v = subgroupShuffleUp(s, {shift}u);
                let adj_v = i32(subgroupShuffle({ai}, LANES - {shift}u + lane));
                let src = select(up_v, adj_v, lane < {shift}u);
                let up_mm_bit = (row_mask >> ((lane - {shift}u) & 31u)) & 1u;
                let adj_mm_bit = ({pmi} >> (LANES - {shift}u + lane)) & 1u;
                let src_mm = select(up_mm_bit, adj_mm_bit, lane < {shift}u);
                s = max(s, max(src - {shift} * gap_extend - i32(src_mm) * gap_open, 0));
            }}
"#
            );
        }
        let _ = write!(
            s,
            r#"            {store_p}
            prev_row = s;
            up_mm = m;
            prev_mask = row_mask;
        }}
"#
        );
    }
    let store_n = format!(
        "{}\n        {}",
        store(n, "pa", "prev_row"),
        store(n, "pmm", "prev_mask")
    );
    let _ = write!(
        s,
        r#"        {store_n}
        best = max(best, s);
    }}

    best = subgroupMax(best);
    if (lane == 0u) {{
        if (sv.w != 0u && best > 0) {{ best += i32(params.exact_match_bonus); }}
        scores[id] = u32(best);
    }}
}}
"#
    );
    s
}
