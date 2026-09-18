const SLOTS: u32 = SLOTS_PLACEHOLDERu;
@group(0) @binding(0) var<storage, read> survivor_count: u32;
@group(0) @binding(1) var<storage, read_write> indirect: array<u32, 3>;

// Turns the survivor count into a 2D dispatch for the DP pass.
@compute @workgroup_size(1)
fn main() {
    let groups = (survivor_count + SLOTS - 1u) / SLOTS;
    let x = min(max(groups, 1u), 32768u);
    indirect[0] = x;
    indirect[1] = (groups + x - 1u) / x;
    indirect[2] = 1u;
}
