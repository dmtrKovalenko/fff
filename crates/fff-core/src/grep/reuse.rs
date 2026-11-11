use crate::simd_path::ArenaPtr;
use crate::types::{ContentCacheBudget, FileItem, MmapSlot};
use std::fs::File;
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) struct GrepReuse<'a> {
    files: &'a [FileItem],
    previous: &'a [AtomicU64],
    next: &'a [AtomicU64],
    skipped: AtomicUsize,
    started: u64,
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
            started: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        }
    }

    pub(crate) fn files_skipped(&self) -> usize {
        self.skipped.load(Ordering::Relaxed)
    }

    fn skip(&self, index: usize, stamp: u64) {
        self.next[index].store(stamp, Ordering::Relaxed);
        self.skipped.fetch_add(1, Ordering::Relaxed);
    }
}

pub(super) struct GrepMiss<'a> {
    slot: &'a AtomicU64,
    bytes: &'a [u8],
    source: Option<File>,
    started: u64,
}

impl GrepMiss<'_> {
    pub(super) fn record(&self) {
        let stamp = self
            .source
            .as_ref()
            .and_then(|f| disk_stamp(|| f.metadata(), self.bytes.len(), self.started))
            .unwrap_or_else(|| fingerprint(self.bytes));
        self.slot.store(stamp, Ordering::Relaxed);
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
    let Some(reuse) = reuse else {
        return Some((
            file.get_content_for_search(buf, mmap, arena, base, budget, dir)?
                .0,
            None,
        ));
    };
    let idx = reuse
        .files
        .element_offset(file)
        .expect("ranked file belongs to index");
    let old = reuse
        .previous
        .get(idx)
        .map_or(0, |slot| slot.load(Ordering::Relaxed));
    // Zero means unsearched or still eligible. Odd stamps verify bytes, even stamps verify an inode.
    if old != 0
        && old & 1 == 0
        && disk_stamp(
            || std::fs::metadata(file.absolute_path(arena, base)),
            file.size as usize,
            reuse.started,
        ) == Some(old)
    {
        reuse.skip(idx, old);
        return None;
    }

    let (bytes, source) = file.get_content_for_search(buf, mmap, arena, base, budget, dir)?;
    if old & 1 == 1 && fingerprint(bytes) == old {
        reuse.skip(idx, old);
        return None;
    }
    Some((
        bytes,
        Some(GrepMiss {
            slot: &reuse.next[idx],
            bytes,
            source,
            started: reuse.started,
        }),
    ))
}

fn fingerprint(bytes: &[u8]) -> u64 {
    (ahash::RandomState::with_seed(0).hash_one(bytes) << 1) | 1
}

#[cfg(unix)]
fn disk_stamp(
    metadata: impl FnOnce() -> std::io::Result<std::fs::Metadata>,
    size: usize,
    started: u64,
) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    // Cached mappings and recently written files are verified against their actual bytes.
    if size >= crate::constants::MMAP_THRESHOLD as usize {
        return None;
    }
    let meta = metadata().ok()?;
    if !meta.is_file()
        || meta.len() != size as u64
        || meta.ctime() as u64 >= started
        || meta.ctime_nsec() == 0
    {
        return None;
    }
    let version = (
        meta.dev(),
        meta.ino(),
        meta.ctime(),
        meta.ctime_nsec(),
        meta.len(),
    );
    Some((ahash::RandomState::with_seed(0).hash_one(version) << 1).max(2))
}

#[cfg(not(unix))]
fn disk_stamp(
    _: impl FnOnce() -> std::io::Result<std::fs::Metadata>,
    _: usize,
    _: u64,
) -> Option<u64> {
    None
}
