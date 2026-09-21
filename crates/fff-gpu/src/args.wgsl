const SLOTS: u32 = SLOTS_PLACEHOLDERu;
@group(0) @binding(0) var<storage, read> survivor_count: u32;
@group(0) @binding(1) var<storage, read_write> indirect: array<u32, 9>;
@group(0) @binding(2) var<storage, read_write> hist: array<u32, 4096>;
@group(0) @binding(3) var<storage, read_write> tie_hist: array<u32, 4096>;
@group(0) @binding(4) var<storage, read_write> next_count: u32;

// Turns the survivor count into indirect dispatches (SLOTS, 64 and 4096 per
// group) and clears the top-k histograms and the other set's counter, so no
// blit encoder is needed between passes.
@compute @workgroup_size(256)
fn main(@builtin(local_invocation_id) lid: vec3<u32>) {
    for (var i = lid.x; i < 4096u; i += 256u) {
        hist[i] = 0u;
        tie_hist[i] = 0u;
    }
    if (lid.x != 0u) { return; }
    next_count = 0u;
    let groups = (survivor_count + SLOTS - 1u) / SLOTS;
    let x = min(max(groups, 1u), 32768u);
    indirect[0] = x;
    indirect[1] = (groups + x - 1u) / x;
    indirect[2] = 1u;
    let g64 = (survivor_count + 63u) / 64u;
    let x64 = min(max(g64, 1u), 32768u);
    indirect[3] = x64;
    indirect[4] = (g64 + x64 - 1u) / x64;
    indirect[5] = 1u;
    let g4k = (survivor_count + 4095u) / 4096u;
    let x4k = min(max(g4k, 1u), 32768u);
    indirect[6] = x4k;
    indirect[7] = (g4k + x4k - 1u) / x4k;
    indirect[8] = 1u;
}
