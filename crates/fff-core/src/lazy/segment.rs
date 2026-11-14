use crate::file_picker::FFFMode;
use crate::frecency::FrecencyTracker;
use crate::parallelism::SEARCH_THREAD_POOL;
use crate::search_scope::SearchScope;
use crate::simd_path::{ArenaPtr, ChunkedPathStore, ChunkedPathStoreBuilder};
use crate::types::{ContentCacheBudget, FFFStringStorage, FileItem};
use crate::walk::WalkBatch;
use rayon::prelude::*;
use std::path::Path;

// A slice of the running walk in the picker's own layout: files plus the
// arena holding their paths, so every search function works on it unchanged.
pub(super) struct Segment {
    files: Vec<FileItem>,
    arena: ChunkedPathStore,
}

impl Segment {
    pub(super) fn new(batch: WalkBatch) -> Self {
        let mut builder = ChunkedPathStoreBuilder::new(batch.len());
        let files = batch.into_files(|file, path| {
            let filename_offset = file.path.filename_offset;
            file.set_path(builder.add_file_immediate(path, filename_offset));
        });

        Self {
            files,
            arena: builder.finish(),
        }
    }

    pub(super) fn len(&self) -> usize {
        self.files.len()
    }

    pub(super) fn scope<'a>(
        &'a self,
        base_path: &'a Path,
        budget: &'a ContentCacheBudget,
    ) -> SearchScope<'a> {
        let arena = self.arena.as_arena_ptr();
        SearchScope {
            files: &self.files,
            base_count: self.files.len(),
            live_count: self.files.len(),
            arena,
            overflow_arena: arena,
            base_path,
            budget,
            bigram_index: None,
        }
    }

    // Positions of `files` in this segment, in the given order.
    pub(super) fn indices_of(&self, files: &[&FileItem]) -> Vec<u32> {
        files
            .iter()
            .filter_map(|file| self.index_of(file))
            .map(|index| index as u32)
            .collect()
    }

    fn index_of(&self, file: &FileItem) -> Option<usize> {
        let start = self.files.as_ptr() as usize;
        let offset = (std::ptr::from_ref(file) as usize).checked_sub(start)?;
        let index = offset / std::mem::size_of::<FileItem>();
        (index < self.files.len()).then_some(index)
    }

    pub(super) fn apply_frecency(
        &mut self,
        tracker: &FrecencyTracker,
        base_path: &Path,
        shared_base: Option<&Path>,
        mode: FFFMode,
    ) {
        let arena = self.arena.as_arena_ptr();
        SEARCH_THREAD_POOL.install(|| {
            self.files.par_iter_mut().for_each_init(
                || tracker.reader(),
                |reader, file| {
                    file.update_frecency_scores(reader, arena, base_path, shared_base, mode)
                },
            );
        });
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct FileRef {
    segment: u32,
    index: u32,
}

/// Files of the running walk that a streamed lazy page points into.
#[derive(Default)]
pub struct StreamedFiles {
    segments: Vec<Option<Segment>>,
}

impl StreamedFiles {
    // Keeps `segment` alive and returns stable references to its `indices`.
    pub(super) fn push(&mut self, segment: Segment, indices: Vec<u32>) -> Vec<FileRef> {
        let id = self.segments.len() as u32;
        self.segments.push(Some(segment));
        indices
            .into_iter()
            .map(|index| FileRef { segment: id, index })
            .collect()
    }

    pub(super) fn file(&self, file: FileRef) -> Option<&FileItem> {
        let segment = self.segments.get(file.segment as usize)?.as_ref()?;
        segment.files.get(file.index as usize)
    }

    pub(super) fn arena(&self, file: FileRef) -> ArenaPtr {
        self.segments
            .get(file.segment as usize)
            .and_then(Option::as_ref)
            .map_or(ArenaPtr::null(), |segment| segment.arena.as_arena_ptr())
    }

    // Frees every segment none of `used` points into.
    pub(super) fn retain(&mut self, used: impl Iterator<Item = FileRef>) {
        let mut keep = vec![false; self.segments.len()];
        for file in used {
            keep[file.segment as usize] = true;
        }
        for (segment, keep) in self.segments.iter_mut().zip(keep) {
            if !keep {
                *segment = None;
            }
        }
    }

    pub(super) fn update_frecency(
        &mut self,
        files: &[FileRef],
        tracker: &FrecencyTracker,
        base_path: &Path,
        shared_base: Option<&Path>,
        mode: FFFMode,
    ) {
        let reader = tracker.reader();
        for file in files {
            let Some(segment) = self.segments[file.segment as usize].as_mut() else {
                continue;
            };
            let arena = segment.arena.as_arena_ptr();
            segment.files[file.index as usize].update_frecency_scores(
                &reader,
                arena,
                base_path,
                shared_base,
                mode,
            );
        }
    }
}

impl FFFStringStorage for &StreamedFiles {
    fn arena_for(&self, file: &FileItem) -> ArenaPtr {
        self.segments
            .iter()
            .flatten()
            .find(|segment| segment.index_of(file).is_some())
            .map_or(ArenaPtr::null(), |segment| segment.arena.as_arena_ptr())
    }

    fn base_arena(&self) -> ArenaPtr {
        ArenaPtr::null()
    }

    fn overflow_arena(&self) -> ArenaPtr {
        ArenaPtr::null()
    }
}
