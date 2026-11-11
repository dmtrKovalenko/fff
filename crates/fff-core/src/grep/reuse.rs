use crate::simd_path::ArenaPtr;
use crate::types::{ContentCacheBudget, FileItem, MmapSlot};
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

pub(crate) struct GrepReuse<'a> {
    files: &'a [FileItem],
    previous: &'a [AtomicU64],
    next: &'a [AtomicU64],
    skipped: AtomicUsize,
}

impl<'a> GrepReuse<'a> {
    pub(crate) fn new(
        files: &'a [FileItem],
        previous: &'a [AtomicU64],
        next: &'a [AtomicU64],
    ) -> Self {
        Self {
            files,
            previous,
            next,
            skipped: AtomicUsize::new(0),
        }
    }

    pub(crate) fn files_skipped(&self) -> usize {
        self.skipped.load(Ordering::Relaxed)
    }
}

pub(super) struct GrepMiss<'a> {
    slot: &'a AtomicU64,
    bytes: &'a [u8],
}

impl GrepMiss<'_> {
    pub(super) fn record(&self) {
        self.slot.store(fingerprint(self.bytes), Ordering::Relaxed);
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn read_candidate<'a>(
    reuse: Option<&'a GrepReuse<'_>>,
    file: &'a FileItem,
    buf: &'a mut Vec<u8>,
    mmap: &'a mut MmapSlot,
    arena: ArenaPtr,
    base: &Path,
    budget: &ContentCacheBudget,
    dir: Option<&std::fs::File>,
) -> Option<(&'a [u8], Option<GrepMiss<'a>>)> {
    let bytes =
        file.get_content_for_search(buf, mmap, arena, base, budget, dir, reuse.is_some())?;
    let Some(reuse) = reuse else {
        return Some((bytes, None));
    };
    let idx = reuse
        .files
        .element_offset(file)
        .expect("ranked file belongs to index");
    let old = reuse
        .previous
        .get(idx)
        .map_or(0, |slot| slot.load(Ordering::Relaxed));
    if old != 0 && fingerprint(bytes) == old {
        reuse.next[idx].store(old, Ordering::Relaxed);
        reuse.skipped.fetch_add(1, Ordering::Relaxed);
        return None;
    }
    Some((
        bytes,
        Some(GrepMiss {
            slot: &reuse.next[idx],
            bytes,
        }),
    ))
}

fn fingerprint(bytes: &[u8]) -> u64 {
    ahash::RandomState::with_seed(0).hash_one(bytes).max(1)
}
