use crate::types::FileItem;
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

    pub(super) fn skips(&self, file: &FileItem) -> bool {
        if self.previous.is_empty() {
            return false;
        }
        let (word, mask) = self.bit(file);
        if self.previous[word].load(Ordering::Relaxed) & mask == 0 {
            return false;
        }
        self.next[word].fetch_or(mask, Ordering::Relaxed);
        self.skipped.fetch_add(1, Ordering::Relaxed);
        true
    }

    pub(super) fn record_miss(&self, file: &FileItem) {
        let (word, mask) = self.bit(file);
        self.next[word].fetch_or(mask, Ordering::Relaxed);
    }

    fn bit(&self, file: &FileItem) -> (usize, u64) {
        let index = self
            .files
            .element_offset(file)
            .expect("ranked file belongs to index");
        (index / 64, 1 << (index % 64))
    }
}
