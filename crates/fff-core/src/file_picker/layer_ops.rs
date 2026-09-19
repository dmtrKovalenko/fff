use ahash::AHashMap;
use rayon::prelude::*;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tracing::{debug, info};

use super::{FilePicker, FileSync, is_indexable};
use crate::constants::{MAX_OVERFLOW_FILES, PATH_BUF_SIZE};
use crate::error::Error;
use crate::frecency::FrecencyTracker;
use crate::index::layers::{LayerId, LayerInfo, LayerStack};
use crate::index::path_index::PathIndex;
use crate::index::snapshot::{
    IndexEntry, LayerSnapshot, SnapshotLayout, dir_name_key, filename_offset,
    normalize_relative_path,
};
use crate::parallelism::BACKGROUND_THREAD_POOL;
use crate::simd_path::ChunkedPathStoreBuilder;
use crate::stable_vec::StableVec;
use crate::types::{ContentCacheBudget, DirItem, FileItem};

impl FileSync {
    // Sealing drops the layer's chunk dedup table, keeping only the packed
    // arena. At the cap the two newest layers compact into one first.
    fn create_layer(&mut self, label: Option<&str>) -> LayerId {
        self.seal_top_sorted();
        self.make_room();
        self.layers
            .open_new(label, self.files.len(), self.dirs.len());

        self.layers.top_id()
    }

    // Sealed layers are immutable: a sealed top (imported or adopted) gets a
    // fresh open layer above it before the first write.
    pub(super) fn ensure_open_top(&mut self) -> LayerId {
        if !self.layers.top_is_open() {
            self.make_room();
            self.layers
                .open_new(None, self.files.len(), self.dirs.len());
        }

        self.layers.top_id()
    }

    fn make_room(&mut self) {
        if self.layers.is_full() {
            self.compact_layers_at(self.layers.merge_candidate());
        }
    }

    // Sorting by `(dir, name)` turns every later lookup into a binary search.
    fn seal_top_sorted(&mut self) {
        let top = self.layers.top_id();
        if !self.layers.is_sorted(top)
            && let Some((start, end)) = self.layers.file_range(top, self.files.len())
        {
            self.sort_file_range(start, end);
        }

        self.layers.seal_top();
        self.layers.set_sorted(top, true);
    }

    fn sort_file_range(&mut self, start: usize, end: usize) {
        self.open_file_index.reset();
        if end - start < 2 {
            return;
        }

        let arenas = self.layer_arenas();
        self.files[start..end].sort_unstable_by(|a, b| {
            let mut buf_a = [0u8; PATH_BUF_SIZE];
            let mut buf_b = [0u8; PATH_BUF_SIZE];
            let pa = a.path.read_to_buf(arenas.get(a.layer_id()), &mut buf_a);
            let pb = b.path.read_to_buf(arenas.get(b.layer_id()), &mut buf_b);
            dir_name_key(pa, a.path.filename_offset).cmp(&dir_name_key(pb, b.path.filename_offset))
        });
    }

    // Brings a tombstoned slot back; `false` when it was already live.
    pub(super) fn revive_file(&mut self, index: usize) -> bool {
        let file = &mut self.files[index];
        if !file.is_deleted() {
            return false;
        }

        file.set_deleted(false);
        let parent_dir = file.parent_dir_index;
        self.live_count += 1;
        self.revive_dir(parent_dir);
        true
    }

    // Merges layers `at` and `at + 1` into one re-deduped arena and shifts
    // higher ids down so an id always equals its stack position.
    fn compact_layers_at(&mut self, at: usize) {
        let first = (at + 1) as LayerId;
        let second = first + 1;
        let arenas = self.layer_arenas();
        let mut buf = [0u8; PATH_BUF_SIZE];

        let mut file_paths: Vec<(usize, String, u16)> = Vec::new();
        for (idx, file) in self.files.iter().enumerate().skip(self.base_count) {
            let layer = file.layer_id();
            if layer == first || layer == second {
                let path = file
                    .path
                    .read_to_buf(arenas.get(layer), &mut buf)
                    .to_owned();
                file_paths.push((idx, path, file.path.filename_offset));
            }
        }

        let mut dir_paths: Vec<(usize, String)> = Vec::new();
        for (idx, dir) in self.dirs.iter().enumerate().skip(self.base_dirs_count) {
            let layer = dir.layer_id();
            if layer == first || layer == second {
                let path = dir.path.read_to_buf(arenas.get(layer), &mut buf).to_owned();
                dir_paths.push((idx, path));
            }
        }

        let mut builder = ChunkedPathStoreBuilder::new(file_paths.len() + dir_paths.len());

        for (idx, path, filename_offset) in &file_paths {
            let chunked = builder.add_file_immediate(path, *filename_offset);
            if let Some(file) = self.files.get_mut(*idx) {
                file.set_path(chunked);
                file.set_layer(first);
            }
        }

        for (idx, path) in &dir_paths {
            let chunked = builder.add_dir_immediate(path);
            if let Some(dir) = self.dirs.get_mut(*idx) {
                dir.path = chunked;
                dir.set_layer(first);
            }
        }

        for file in self.files.iter_mut().skip(self.base_count) {
            let layer = file.layer_id();
            if layer > second {
                file.set_layer(layer - 1);
            }
        }

        for dir in self.dirs.iter_mut().skip(self.base_dirs_count) {
            let layer = dir.layer_id();
            if layer > second {
                dir.set_layer(layer - 1);
            }
        }

        self.layers.merge_at(at, builder.finish());
        if let Some((start, end)) = self.layers.file_range(first, self.files.len()) {
            self.sort_file_range(start, end);
            self.layers.set_sorted(first, true);
        }
    }

    // `true` when the tables were reallocated (mmap caches are dropped).
    fn ensure_capacity(&mut self, files_extra: usize, dirs_extra: usize) -> bool {
        let mut grew = false;
        if self.files.len() + files_extra > self.files.capacity() {
            self.files = self
                .files
                .reallocate_with_reserve(files_extra + MAX_OVERFLOW_FILES);
            grew = true;
        }
        if self.dirs.len() + dirs_extra > self.dirs.capacity() {
            self.dirs = self
                .dirs
                .reallocate_with_reserve(dirs_extra + MAX_OVERFLOW_FILES);
            grew = true;
        }

        grew
    }

    fn add_entries(&mut self, entries: &[IndexEntry<'_>]) -> usize {
        let layer = self.ensure_open_top();
        let mut added = 0;

        for entry in entries {
            let Some(path) = normalize_relative_path(entry.relative_path) else {
                continue;
            };
            if let Some(idx) = self.find_relative_path_index(&path) {
                added += self.revive_file(idx) as usize;
                continue;
            }

            let offset = filename_offset(&path) as u16;
            let Some(dir_idx) = self.find_or_add_dir(&path[..offset as usize]) else {
                break;
            };

            let mut file =
                FileItem::new_raw(offset, entry.size, entry.modified, None, entry.is_binary());
            file.set_path(self.layers.top_builder().add_file_immediate(&path, offset));
            file.set_layer(layer);
            file.parent_dir_index = dir_idx;

            if !self.files.push(file) {
                break;
            }
            self.live_count += 1;
            self.revive_dir(dir_idx);
            added += 1;
        }

        added
    }

    // Appends the snapshot as a new sealed layer; files already present in
    // the index are skipped. Returns the new layer id and files added.
    fn append_snapshot(&mut self, snapshot: LayerSnapshot) -> (LayerId, usize) {
        self.seal_top_sorted();
        self.make_room();

        let LayerSnapshot {
            label,
            store,
            files,
            dirs,
            layout,
        } = snapshot;
        let layer = self.layers.top_id() + 1;
        let arena = store.as_arena_ptr();
        let files_start = self.files.len();
        // Registered up front so lookups below can resolve the new dirs' arena.
        self.layers
            .push_sealed(label, store, files_start, self.dirs.len());

        let mut dir_map: Vec<u32> = Vec::with_capacity(dirs.len());
        let mut dir_buf = [0u8; PATH_BUF_SIZE];
        for mut dir in dirs {
            let path = dir.read_relative_path(arena, &mut dir_buf);
            let idx = match self.find_dir_index(path) {
                Some(idx) => idx as u32,
                None => {
                    let idx = self.dirs.len() as u32;
                    dir.set_layer(layer);
                    dir.set_deleted(false);
                    if !self.dirs.push(dir) {
                        break;
                    }
                    self.live_dirs_count += 1;
                    idx
                }
            };
            dir_map.push(idx);
        }

        let mut added = 0;
        let mut path_buf = [0u8; PATH_BUF_SIZE];
        for mut file in files {
            let Some(&dir_idx) = dir_map.get(file.parent_dir_index as usize) else {
                continue;
            };
            let path = file.path.read_to_buf(arena, &mut path_buf);
            if let Some(idx) = self.lookup_relative_path(path, files_start) {
                added += self.revive_file(idx) as usize;
                continue;
            }

            file.set_layer(layer);
            file.set_deleted(false);
            file.parent_dir_index = dir_idx;
            if !self.files.push(file) {
                break;
            }
            self.live_count += 1;
            self.revive_dir(dir_idx);
            added += 1;
        }

        // Every sealed layer must be binary searchable.
        if layout != SnapshotLayout::Sorted {
            self.sort_file_range(files_start, self.files.len());
        }

        (layer, added)
    }

    // Live files of `layer` (0 = base) as a self-contained snapshot; the arena
    // is shared unless the layer's dirs live in another layer's arena.
    fn export_layer(&self, layer: LayerId) -> Option<LayerSnapshot> {
        if layer == 0 {
            return self.export_base();
        }

        let (start, end) = self.layers.file_range(layer, self.files.len())?;
        let (dir_start, dir_end) = self.layers.dir_range(layer, self.dirs.len())?;
        let arenas = self.layer_arenas();

        // Local dir table: the layer's own dirs plus every foreign parent.
        let mut local_of: AHashMap<u32, u32> = AHashMap::new();
        let mut local_dirs: Vec<(String, u32)> = Vec::new();
        let mut register = |global: u32, dirs: &StableVec<DirItem>| -> Option<u32> {
            if let Some(&local) = local_of.get(&global) {
                return Some(local);
            }
            let dir = dirs.get(global as usize)?;
            let local = local_dirs.len() as u32;
            local_dirs.push((dir.relative_path(arenas.get(dir.layer_id())), global));
            local_of.insert(global, local);
            Some(local)
        };

        for global in dir_start..dir_end {
            register(global as u32, &self.dirs);
        }
        let mut files: Vec<FileItem> = Vec::with_capacity(end - start);
        for file in self.files[start..end].iter().filter(|f| !f.is_deleted()) {
            let Some(local) = register(file.parent_dir_index, &self.dirs) else {
                continue;
            };
            let mut copy = file.clone();
            copy.parent_dir_index = local;
            copy.set_layer(0);
            files.push(copy);
        }

        // Sorted local table, remapping the parents assigned above.
        let mut order: Vec<u32> = (0..local_dirs.len() as u32).collect();
        order.sort_unstable_by(|&a, &b| local_dirs[a as usize].0.cmp(&local_dirs[b as usize].0));
        let mut sorted_pos = vec![0u32; local_dirs.len()];
        for (pos, &old) in order.iter().enumerate() {
            sorted_pos[old as usize] = pos as u32;
        }
        for file in &mut files {
            file.parent_dir_index = sorted_pos[file.parent_dir_index as usize];
        }

        let foreign = order
            .iter()
            .any(|&i| self.dirs[local_dirs[i as usize].1 as usize].layer_id() != layer);
        let store = self.layers.store_snapshot(layer)?;
        let (store, dirs) = if foreign {
            let owned = Arc::try_unwrap(store).unwrap_or_else(|arc| (*arc).clone());
            let mut builder = ChunkedPathStoreBuilder::from_store(owned, 0);
            let dirs = order
                .iter()
                .map(|&i| {
                    let (path, _) = &local_dirs[i as usize];
                    DirItem::new(
                        builder.add_dir_immediate(path),
                        crate::index::snapshot::last_segment_offset(path),
                    )
                })
                .collect();
            (Arc::new(builder.finish()), dirs)
        } else {
            let dirs = order
                .iter()
                .map(|&i| {
                    let mut dir = self.dirs[local_dirs[i as usize].1 as usize].clone();
                    dir.set_layer(0);
                    dir.set_deleted(false);
                    dir
                })
                .collect();
            (store, dirs)
        };

        Some(LayerSnapshot {
            label: self.layers.label(layer),
            store,
            files,
            dirs,
            layout: if self.layers.is_sorted(layer) {
                SnapshotLayout::Sorted
            } else {
                SnapshotLayout::Unsorted
            },
        })
    }

    fn export_base(&self) -> Option<LayerSnapshot> {
        let store = Arc::clone(self.chunked_paths.as_ref()?);
        let files = self.files[..self.base_count]
            .iter()
            .filter(|f| !f.is_deleted())
            .cloned()
            .collect();
        let dirs = self.dirs[..self.base_dirs_count]
            .iter()
            .map(|d| {
                let mut dir = d.clone();
                dir.set_deleted(false);
                dir
            })
            .collect();

        Some(LayerSnapshot {
            label: None,
            store,
            files,
            dirs,
            layout: SnapshotLayout::BasePartitioned,
        })
    }

    // Moves every explicit overlay of `old` onto this sync. The session layer
    // is left behind: a rescan re-discovers its files.
    pub(super) fn adopt_overlays(&mut self, old: &mut FileSync) {
        let top = old.layers.top_id();
        for id in (crate::index::layers::SESSION_LAYER + 1)..=top {
            let Some(snapshot) = old.export_layer(id) else {
                continue;
            };
            if snapshot.files.is_empty() && snapshot.label.is_none() {
                continue;
            }
            self.ensure_capacity(snapshot.files.len(), snapshot.dirs.len());
            let (layer, added) = self.append_snapshot(snapshot);
            debug!(layer, added, "Adopted index layer across rescan");
        }
    }

    pub(crate) fn from_snapshot(
        snapshot: LayerSnapshot,
        base_path: &Path,
        frecency: Option<&FrecencyTracker>,
        mode: super::FFFMode,
    ) -> Self {
        let LayerSnapshot {
            store,
            mut files,
            dirs,
            layout,
            ..
        } = snapshot;
        let arena = store.as_arena_ptr();

        match layout {
            SnapshotLayout::BasePartitioned => {}
            // Stable: keeps `(dir, name)` order inside each partition.
            SnapshotLayout::Sorted => files.sort_by_key(|f| !is_indexable(f)),
            SnapshotLayout::Unsorted => BACKGROUND_THREAD_POOL.install(|| {
                files.par_sort_unstable_by(|a, b| {
                    let mut buf_a = [0u8; PATH_BUF_SIZE];
                    let mut buf_b = [0u8; PATH_BUF_SIZE];
                    (!is_indexable(a)).cmp(&!is_indexable(b)).then_with(|| {
                        let pa = a.path.read_to_buf(arena, &mut buf_a);
                        let pb = b.path.read_to_buf(arena, &mut buf_b);
                        dir_name_key(pa, a.path.filename_offset)
                            .cmp(&dir_name_key(pb, b.path.filename_offset))
                    })
                });
            }),
        }
        let indexable_count = files.partition_point(is_indexable);

        for dir in &dirs {
            dir.reset_frecency();
        }
        if let Some(frecency) = frecency {
            let dirs_ref = &dirs;
            BACKGROUND_THREAD_POOL.install(|| {
                files.par_iter_mut().for_each(|file| {
                    let _ = file.update_frecency_scores(frecency, arena, base_path, mode);
                    let score = file.access_frecency_score as i32;
                    if score > 0
                        && let Some(dir) = dirs_ref.get(file.parent_dir_index as usize)
                    {
                        dir.update_frecency_if_larger(score);
                    }
                });
            });
        }

        let base_count = files.len();
        let base_dirs_count = dirs.len();
        Self {
            files: StableVec::from_vec_with_reserve(files, MAX_OVERFLOW_FILES),
            indexable_count,
            base_count,
            live_count: base_count,
            dirs: StableVec::from_vec_with_reserve(dirs, MAX_OVERFLOW_FILES),
            base_dirs_count,
            live_dirs_count: base_dirs_count,
            layers: LayerStack::new(base_count, base_dirs_count),
            open_file_index: PathIndex::default(),
            overlay_dir_index: PathIndex::default(),
            git_workdir: FileSync::discover_git_workdir(base_path),
            bigram_index: None,
            bigram_overlay: None,
            chunked_paths: Some(store),
            ignore_rules: None,
        }
    }
}

impl FilePicker {
    /// Seals the writable overlay and opens a new one, returning its id.
    ///
    /// Everything the watcher indexes from now on lands in the new layer. The
    /// stack holds at most [`MAX_OVERLAY_LAYERS`](crate::MAX_OVERLAY_LAYERS)
    /// overlays; past that the two newest are compacted into one, so
    /// [`SESSION_LAYER`](crate::SESSION_LAYER) — every file added since the
    /// picker started — always survives.
    pub fn create_layer(&mut self, label: Option<&str>) -> LayerId {
        let id = self.sync_data.create_layer(label);
        debug!(layer = id, ?label, "Opened new index layer");

        id
    }

    /// Layer new files are currently written into.
    #[inline]
    pub fn current_layer(&self) -> LayerId {
        self.sync_data.layers.top_id()
    }

    /// Overlay layers oldest first. The base scan (layer 0) is not included —
    /// it is the whole of [`get_files`](Self::get_files) below `base_count`.
    pub fn layers(&self) -> Vec<LayerInfo> {
        self.sync_data
            .layers
            .info(self.sync_data.files(), self.sync_data.dirs.len())
    }

    /// Files owned by `layer` (0 = base scan), tombstones included. Empty
    /// for unknown ids.
    pub fn files_in_layer(&self, layer: LayerId) -> &[FileItem] {
        let files = self.sync_data.files();
        if layer == 0 {
            return &files[..self.sync_data.base_count];
        }
        match self.sync_data.layers.file_range(layer, files.len()) {
            Some((start, end)) => &files[start..end],
            None => &[],
        }
    }

    /// Dirs owned by `layer` (0 = base scan), tombstones included. Empty for
    /// unknown ids.
    pub fn dirs_in_layer(&self, layer: LayerId) -> &[DirItem] {
        let dirs = &self.sync_data.dirs;
        if layer == 0 {
            return &dirs[..self.sync_data.base_dirs_count];
        }
        match self.sync_data.layers.dir_range(layer, dirs.len()) {
            Some((start, end)) => &dirs[start..end],
            None => &[],
        }
    }

    /// Appends entries to the writable layer without touching the filesystem.
    /// Paths already in the index are skipped. Returns the number added.
    ///
    /// Grows the file table when needed; fails with [`Error::IndexBusy`] while
    /// a post-scan pass holds a snapshot of it.
    pub fn add_entries<'a>(
        &mut self,
        entries: impl IntoIterator<Item = IndexEntry<'a>>,
    ) -> Result<usize, Error> {
        let entries: Vec<IndexEntry<'a>> = entries.into_iter().collect();
        self.reserve_index(entries.len(), entries.len())?;
        let added = self.sync_data.add_entries(&entries);
        debug!(
            added,
            layer = self.sync_data.layers.top_id(),
            "Added entries to index layer"
        );

        Ok(added)
    }

    /// Attaches `snapshot` as a new sealed layer on top of the stack and
    /// returns its id. Files already present in the index are skipped.
    pub fn import_layer(&mut self, snapshot: LayerSnapshot) -> Result<LayerId, Error> {
        self.reserve_index(snapshot.files.len(), snapshot.dirs.len())?;
        let (layer, added) = self.sync_data.append_snapshot(snapshot);
        info!(layer, added, "Imported index layer");

        Ok(layer)
    }

    /// Replaces the base scan with `snapshot`, keeping explicit overlay layers.
    /// Frecency is applied when a tracker is given; git status stays unset
    /// until the next status refresh.
    pub fn import_base(
        &mut self,
        snapshot: LayerSnapshot,
        frecency: Option<&FrecencyTracker>,
    ) -> Result<(), Error> {
        if self
            .signals
            .post_scan_indexing_active
            .load(Ordering::Acquire)
        {
            return Err(Error::IndexBusy);
        }

        let started = std::time::Instant::now();
        let sync = FileSync::from_snapshot(snapshot, &self.base_path, frecency, self.mode);
        let file_count = sync.files.len();
        self.commit_new_sync(sync);

        if !self.has_explicit_cache_budget {
            self.cache_budget = Arc::new(ContentCacheBudget::new_for_repo(file_count));
        }
        info!(file_count, elapsed = ?started.elapsed(), "Imported base index");

        Ok(())
    }

    /// Copies the live files of `layer` (0 = base scan) into a snapshot that
    /// can be saved with [`LayerSnapshot::save`]. `None` for unknown ids or
    /// when the base has not been scanned yet.
    pub fn export_layer(&self, layer: LayerId) -> Option<LayerSnapshot> {
        self.sync_data.export_layer(layer)
    }

    /// `export_layer` + [`LayerSnapshot::save`]; returns bytes written.
    pub fn save_layer(&self, layer: LayerId, path: impl AsRef<Path>) -> Result<u64, Error> {
        let snapshot = self
            .export_layer(layer)
            .ok_or(Error::LayerFormat("no such layer"))?;
        snapshot.save(path)
    }

    fn reserve_index(&mut self, files: usize, dirs: usize) -> Result<(), Error> {
        let needs_growth = self.sync_data.files.len() + files > self.sync_data.files.capacity()
            || self.sync_data.dirs.len() + dirs > self.sync_data.dirs.capacity();
        if !needs_growth {
            return Ok(());
        }
        if self
            .signals
            .post_scan_indexing_active
            .load(Ordering::Acquire)
        {
            return Err(Error::IndexBusy);
        }

        if self.sync_data.ensure_capacity(files, dirs) {
            self.cache_budget.reset();
        }

        Ok(())
    }
}
