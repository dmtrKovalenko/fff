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
    _pad1: u32,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> hist: array<u32>;
@group(0) @binding(2) var<storage, read_write> sel: array<u32, 4>;

var<workgroup> bins: array<u32, 4096>;
var<workgroup> block: array<u32, 256>;

// Finds the k-th highest score: each thread owns 16 bins, a suffix scan of
// the block sums says how many survivors score above each block, and the
// one thread whose block crosses k writes sel = [threshold, count above, 0, 0].
@compute @workgroup_size(256)
fn main(@builtin(local_invocation_id) lid: vec3<u32>) {
    let t = lid.x;
    var sum = 0u;
    for (var i = 0u; i < 16u; i++) {
        let v = hist[t * 16u + i];
        bins[t * 16u + i] = v;
        sum += v;
    }
    if (t == 0u) { sum -= bins[0]; }
    block[t] = sum;
    workgroupBarrier();
    // Inclusive suffix sum over blocks: block[t] = sum of blocks >= t.
    for (var s = 1u; s < 256u; s <<= 1u) {
        var add = 0u;
        if (t + s < 256u) { add = block[t + s]; }
        workgroupBarrier();
        block[t] += add;
        workgroupBarrier();
    }
    let k = max(params.top_k, 1u);
    var above = 0u;
    if (t + 1u < 256u) { above = block[t + 1u]; }
    let mine = block[t] - above;
    let lo = select(t * 16u, 1u, t == 0u);
    if (above < k && above + mine >= k) {
        var acc = above;
        for (var s = t * 16u + 15u; s >= lo; s--) {
            let c = bins[s];
            if (acc + c >= k) {
                sel[0] = s;
                sel[1] = acc;
                break;
            }
            acc += c;
            if (s == lo) { break; }
        }
    }
    if (t == 0u && block[0] < k) {
        sel[0] = 1u;
        sel[1] = block[0] - bins[1];
    }
    if (t == 0u) {
        sel[2] = 0u;
        sel[3] = 0u;
    }
}
