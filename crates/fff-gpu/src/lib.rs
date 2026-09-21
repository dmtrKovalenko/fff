mod matcher;
mod scalar;
mod subgroup;

pub use matcher::{GpuMatcher, Kernel, MAX_NEEDLE, Reserve, TopK};

// Bounded heap over a dense score array; ties broken by lower id.
pub fn top_k_scores(scores: &[u32], k: usize) -> Vec<(u32, u32)> {
    let mut heap = std::collections::BinaryHeap::with_capacity(k + 1);
    for (i, &s) in scores.iter().enumerate() {
        if s == 0 {
            continue;
        }
        let key = (s, u32::MAX - i as u32);
        if heap.len() < k {
            heap.push(std::cmp::Reverse(key));
        } else if key > heap.peek().unwrap().0 {
            heap.pop();
            heap.push(std::cmp::Reverse(key));
        }
    }
    heap.into_iter()
        .map(|std::cmp::Reverse((s, i))| (s, u32::MAX - i))
        .collect()
}
