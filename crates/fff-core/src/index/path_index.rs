use ahash::{AHashMap, RandomState};
use parking_lot::Mutex;

use crate::simd_path::PATH_BUF_SIZE;

// Ranges shorter than this are scanned linearly instead of being indexed.
const MIN_INDEXED_LEN: usize = 32;

// Lazy hash index over one unsorted append-only slot range; extends itself on
// lookup and rebuilds when the range moves or is reordered.
#[derive(Default)]
pub(crate) struct PathIndex {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    start: usize,
    indexed_upto: usize,
    map: AHashMap<u64, u32>,
    hasher: RandomState,
}

/// Writes the path stored in a slot into the buffer, returning its length.
pub(crate) type SlotReader<'a> = dyn FnMut(usize, &mut [u8; PATH_BUF_SIZE]) -> usize + 'a;

impl PathIndex {
    /// Finds `path` among slots `[start, end)`.
    pub(crate) fn lookup(
        &self,
        start: usize,
        end: usize,
        path: &str,
        read: &mut SlotReader<'_>,
    ) -> Option<usize> {
        if end - start < MIN_INDEXED_LEN {
            return scan(start, end, path, read);
        }

        let mut inner = self.inner.lock();
        if inner.start != start || inner.indexed_upto > end {
            inner.start = start;
            inner.indexed_upto = start;
            inner.map.clear();
        }

        let mut buf = [0u8; PATH_BUF_SIZE];
        for slot in inner.indexed_upto..end {
            let len = read(slot, &mut buf);
            let hash = inner.hasher.hash_one(&buf[..len]);
            inner.map.entry(hash).or_insert(slot as u32);
        }
        inner.indexed_upto = end;

        let hash = inner.hasher.hash_one(path.as_bytes());
        let slot = *inner.map.get(&hash)? as usize;
        drop(inner);

        let len = read(slot, &mut buf);
        if &buf[..len] == path.as_bytes() {
            return Some(slot);
        }
        // Hash collision: two distinct paths share a bucket.
        scan(start, end, path, read)
    }

    /// Forgets everything; call after slots in the range were reordered.
    pub(crate) fn reset(&self) {
        let mut inner = self.inner.lock();
        inner.indexed_upto = inner.start;
        inner.map.clear();
    }
}

impl Clone for PathIndex {
    fn clone(&self) -> Self {
        let inner = self.inner.lock();
        Self {
            inner: Mutex::new(Inner {
                start: inner.start,
                indexed_upto: inner.indexed_upto,
                map: inner.map.clone(),
                hasher: inner.hasher.clone(),
            }),
        }
    }
}

impl std::fmt::Debug for PathIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.inner.lock();
        f.debug_struct("PathIndex")
            .field("start", &inner.start)
            .field("indexed", &(inner.indexed_upto - inner.start))
            .finish()
    }
}

fn scan(start: usize, end: usize, path: &str, read: &mut SlotReader<'_>) -> Option<usize> {
    let mut buf = [0u8; PATH_BUF_SIZE];
    (start..end).find(|&slot| {
        let len = read(slot, &mut buf);
        &buf[..len] == path.as_bytes()
    })
}
