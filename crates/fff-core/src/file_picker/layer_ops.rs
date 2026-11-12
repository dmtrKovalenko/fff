use ahash::{AHashMap, AHashSet};
use rayon::prelude::*;
use std::cell::RefCell;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tracing::{debug, info};

use super::{FilePicker, FileSync, is_indexable};
use crate::constants::{MAX_OVERFLOW_FILES, PATH_BUF_SIZE};
use crate::error::Error;
use crate::frecency::FrecencyTracker;
use crate::index::layers::{LayerId, LayerInfo, LayerStack, SESSION_LAYER};
use crate::index::snapshot::{
    IndexEntry, LayerSnapshot, SnapshotLayout, dir_name_key, filename_offset, last_segment_offset,
    normalize_relative_path,
};
use crate::parallelism::BACKGROUND_THREAD_POOL;
use crate::simd_path::{ChunkedPathStore, ChunkedPathStoreBuilder};
use crate::stable_vec::StableVec;
use crate::types::{ContentCacheBudget, DirItem, FileItem};

impl FileSync {
    // Seals the open layer and opens an empty one under the session layer.
    // Fails instead of merging when every id is taken, so ids never move.
    fn create_layer(&mut self, label: Option<&str>) -> Result<LayerId, Error> {
        let id = self.layers.free_id().ok_or(Error::LayerLimit)?;
        self.seal_open_layer();
        self.layers.insert_open(id, label);

        Ok(id)
    }

    // Sealing drops the layer's chunk dedup table, keeping only the packed arena.
    fn seal_open_layer(&mut self) {
        if let Some(id) = self.layers.open_layer() {
            self.sort_layer(id);
            self.layers.seal(id);
        }
    }

    // Sorting by `(dir, name)` turns every later lookup into a binary search.
    fn sort_layer(&mut self, id: LayerId) {
        if self.layers.is_sorted(id) {
            return;
        }
        let Some((start, end)) = self.layers.file_range(id, self.files.len()) else {
            return;
        };

        if end - start > 1 {
            let arenas = self.layer_arenas();
            let (mut buf_a, mut buf_b) = ([0u8; PATH_BUF_SIZE], [0u8; PATH_BUF_SIZE]);
            self.files.range_mut(start..end).sort_unstable_by(|a, b| {
                let pa = a.path.read_to_buf(arenas.get(a.layer_id()), &mut buf_a);
                let pb = b.path.read_to_buf(arenas.get(b.layer_id()), &mut buf_b);
                dir_name_key(pa, a.path.filename_offset)
                    .cmp(&dir_name_key(pb, b.path.filename_offset))
            });
        }
        self.layers.set_sorted(id, true);
    }

    // Moves the files and dirs pushed past `files_len` / `dirs_len` to the end
    // of layer `id`, shifting every layer above it. Only those upper layers and
    // the new slots move, so growing the newest layer moves the session only.
    fn settle_appended(&mut self, id: LayerId, files_len: usize, dirs_len: usize) {
        let (Some((_, files_end)), Some((_, dirs_end))) = (
            self.layers.file_range(id, files_len),
            self.layers.dir_range(id, dirs_len),
        ) else {
            return;
        };
        let added_files = self.files.len() - files_len;
        let added_dirs = self.dirs.len() - dirs_len;

        if added_dirs > 0 && dirs_end < dirs_len {
            let dirs_total = self.dirs.len();
            self.dirs
                .range_mut(dirs_end..dirs_total)
                .rotate_right(added_dirs);
            let moved = dirs_len - dirs_end;
            let remap = |dir: u32| match dir as usize {
                d if d < dirs_end || d >= dirs_total => dir,
                d if d < dirs_len => (d + added_dirs) as u32,
                d => (d - moved) as u32,
            };
            let (base_count, files_total) = (self.base_count, self.files.len());
            for file in self.files.range_mut(base_count..files_total) {
                file.parent_dir_index = remap(file.parent_dir_index);
            }
        }
        if added_files > 0 && files_end < files_len {
            let files_total = self.files.len();
            self.files
                .range_mut(files_end..files_total)
                .rotate_right(added_files);
        }

        self.layers.shift_above(id, added_files, added_dirs);
    }

    // Brings a tombstoned slot back; `false` when it was already live.
    pub(super) fn revive_file(&mut self, index: usize) -> bool {
        let file = &self.files[index];
        if !file.is_deleted() {
            return false;
        }

        file.set_deleted(false);
        let parent_dir = file.parent_dir_index;
        self.live_count += 1;
        self.revive_dir(parent_dir);
        true
    }

    fn revive_entry(&mut self, index: usize, size: u64, modified: u64, binary: bool) -> bool {
        if !self.revive_file(index) {
            return false;
        }
        // Tombstoning already evicted the old cached contents.
        let file = &mut self.files[index];
        file.size = size;
        file.modified = modified;
        file.set_binary(binary);
        if index < self.indexable_count
            && let Some(overlay) = &self.bigram_overlay
        {
            overlay.write().invalidate_file(index);
        }
        true
    }

    // `true` when the file table was reallocated, which drops its mmap caches.
    // Dirs hold no mappings, so growing them alone keeps the cache budget.
    pub(super) fn ensure_capacity(&mut self, files_extra: usize, dirs_extra: usize) -> bool {
        if self.dirs.len() + dirs_extra > self.dirs.capacity() {
            self.dirs = self
                .dirs
                .reallocate_with_reserve(dirs_extra + MAX_OVERFLOW_FILES);
        }
        if self.files.len() + files_extra <= self.files.capacity() {
            return false;
        }
        self.files = self
            .files
            .reallocate_with_reserve(files_extra + MAX_OVERFLOW_FILES);

        true
    }

    // Explicit paths never go to the session layer, which a rescan rebuilds
    // from disk. A layer is opened for the first new path, so a batch of
    // duplicates does not take an id.
    fn add_entries(&mut self, entries: &[IndexEntry<'_>]) -> Result<usize, Error> {
        let mut layer = self.layers.open_layer();
        if layer.is_none() && self.layers.free_id().is_none() {
            return Err(Error::LayerLimit);
        }
        let (files_len, dirs_len) = (self.files.len(), self.dirs.len());
        let mut batch: AHashSet<String> = AHashSet::new();
        let mut added = 0;

        for entry in entries {
            let Some(path) = normalize_relative_path(entry.relative_path) else {
                continue;
            };
            if let Some(idx) = self.lookup_relative_path(&path, files_len) {
                added +=
                    self.revive_entry(idx, entry.size, entry.modified, entry.is_binary()) as usize;
                continue;
            }
            if batch.contains(path.as_str()) {
                continue;
            }
            let layer = match layer {
                Some(id) => id,
                None => *layer.insert(self.create_layer(None)?),
            };

            let offset = filename_offset(&path) as u16;
            let Some(dir_idx) = self.find_or_add_dir_in(layer, &path[..offset as usize]) else {
                break;
            };
            let Some(builder) = self.layers.builder(layer) else {
                break;
            };

            let mut file =
                FileItem::new_raw(offset, entry.size, entry.modified, None, entry.is_binary());
            file.mark_definition_language(&path);
            file.set_path(builder.add_file_immediate(&path, offset));
            file.set_layer(layer);
            file.parent_dir_index = dir_idx;

            if !self.files.push(file) {
                break;
            }
            self.live_count += 1;
            self.revive_dir(dir_idx);
            batch.insert(path);
            added += 1;
        }

        if let Some(layer) = layer {
            self.settle_appended(layer, files_len, dirs_len);
        }
        Ok(added)
    }

    // Attaches `snapshot` as layer `id`. An import skips files the index already
    // has. A rescan keeps them in the layer, which owned them first, and
    // tombstones the base copy instead. Returns the files added.
    fn attach_snapshot(&mut self, id: LayerId, snapshot: LayerSnapshot, attach: Attach) -> usize {
        let LayerSnapshot {
            label,
            store,
            files,
            dirs,
            layout,
        } = snapshot;
        let arena = store.as_arena_ptr();
        let (files_len, dirs_len) = (self.files.len(), self.dirs.len());
        // Registered up front so lookups below can resolve the new dirs' arena.
        self.layers
            .insert_sealed(id, label, store, layout == SnapshotLayout::Sorted);

        let mut dir_map: Vec<u32> = Vec::with_capacity(dirs.len());
        let mut dir_buf = [0u8; PATH_BUF_SIZE];
        for mut dir in dirs {
            let path = dir.read_relative_path(arena, &mut dir_buf);
            let idx = match self.find_dir_index(path) {
                Some(idx) => idx as u32,
                None => {
                    let idx = self.dirs.len() as u32;
                    dir.set_layer(id);
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
            let path = file.path.read_to_buf(arena, &mut path_buf);
            // A rescan leaves out the parents the index already has.
            let dir_idx = match dir_map.get(file.parent_dir_index as usize) {
                Some(&idx) => idx,
                None => match self.find_dir_index(&path[..file.path.filename_offset as usize]) {
                    Some(idx) => idx as u32,
                    None => continue,
                },
            };
            if let Some(idx) = self.lookup_relative_path(path, files_len) {
                let existing = &self.files[idx];
                if attach == Attach::Import || idx >= self.base_count || existing.is_deleted() {
                    added +=
                        self.revive_entry(idx, file.size, file.modified, file.is_binary()) as usize;
                    continue;
                }
                file.size = existing.size;
                file.modified = existing.modified;
                file.set_binary(existing.is_binary());
                existing.set_deleted(true);
                self.live_count -= 1;
            }

            file.set_layer(id);
            file.set_deleted(false);
            file.parent_dir_index = dir_idx;
            if !self.files.push(file) {
                break;
            }
            self.live_count += 1;
            self.revive_dir(dir_idx);
            added += 1;
        }

        self.settle_appended(id, files_len, dirs_len);
        // Every sealed layer must be binary searchable.
        self.sort_layer(id);
        if attach == (Attach::Adopt { open: true }) {
            self.layers.reopen(id);
        }

        added
    }

    // Live files of `layer` (0 = base) as a self-contained snapshot; the arena
    // is shared unless the layer's dirs live in another layer's arena.
    fn export_layer(&self, layer: LayerId) -> Option<LayerSnapshot> {
        if layer == 0 {
            return self.export_base();
        }

        let export = self.export_layer_parts(layer)?;
        let store = self.layers.store_snapshot(layer)?;
        Some(export.into_snapshot(store, &self.dirs, |_| false))
    }

    fn export_layer_parts(&self, layer: LayerId) -> Option<LayerExport> {
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

        let dirs = order
            .iter()
            .map(|&i| std::mem::take(&mut local_dirs[i as usize]))
            .collect();

        Some(LayerExport {
            layer,
            label: self.layers.label(layer),
            files,
            dirs,
            sorted: self.layers.is_sorted(layer),
        })
    }

    fn export_base(&self) -> Option<LayerSnapshot> {
        let store = Arc::clone(self.chunked_paths.as_ref()?);
        let mut layout = SnapshotLayout::BasePartitioned;
        let files = self.files[..self.base_count]
            .iter()
            .enumerate()
            .filter(|(_, f)| !f.is_deleted())
            .map(|(index, file)| {
                if is_indexable(file) != (index < self.indexable_count) {
                    layout = SnapshotLayout::Unsorted;
                }
                file.clone()
            })
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
            layout,
        })
    }

    // Moves every explicit layer of `old` onto this sync with its id, label and
    // open state. The session layer stays behind: a rescan re-discovers its files.
    pub(super) fn adopt_overlays(&mut self, old: &mut FileSync) {
        let exports: Vec<(LayerId, Option<LayerExport>)> = old
            .layers
            .ids()
            .filter(|&id| id != SESSION_LAYER)
            .map(|id| (id, old.export_layer_parts(id)))
            .collect();
        // Taking the stack leaves each arena uniquely owned, so a layer with
        // foreign dirs reopens it in place instead of copying it.
        let base_only = LayerStack::new(old.base_count, old.base_dirs_count);
        let mut stores = std::mem::replace(&mut old.layers, base_only).into_stores();
        for (id, export) in exports {
            let (Some(export), Some(pos)) = (export, stores.iter().position(|s| s.0 == id)) else {
                continue;
            };
            let (_, store, open) = stores.swap_remove(pos);
            let snapshot =
                export.into_snapshot(store, &old.dirs, |dir| self.find_dir_index(dir).is_some());
            self.ensure_capacity(snapshot.files.len(), snapshot.dirs.len());
            let added = self.attach_snapshot(id, snapshot, Attach::Adopt { open });
            debug!(layer = id, added, "Adopted index layer across rescan");
        }
    }

    pub(crate) fn from_snapshot(
        snapshot: LayerSnapshot,
        base_path: &Path,
        git: Option<crate::git::GitDiscovery>,
        frecency: Option<&FrecencyTracker>,
        mode: super::FFFMode,
    ) -> Self {
        let shared_base = git.as_ref().and_then(|git| git.shared_base(base_path));
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
                    (!is_indexable(a)).cmp(&!is_indexable(b)).then_with(|| {
                        COMPARE_BUFS.with_borrow_mut(|bufs| {
                            let [buf_a, buf_b] = &mut **bufs;
                            let pa = a.path.read_to_buf(arena, buf_a);
                            let pb = b.path.read_to_buf(arena, buf_b);
                            dir_name_key(pa, a.path.filename_offset)
                                .cmp(&dir_name_key(pb, b.path.filename_offset))
                        })
                    })
                });
            }),
        }
        let indexable_count = files.partition_point(is_indexable);

        // An in-memory snapshot carries the source worktree's status and ranking.
        for file in &mut files {
            file.git_status = None;
            file.git_recency_score = 0;
            file.access_frecency_score = 0;
            file.modification_frecency_score = 0;
        }
        for dir in &dirs {
            dir.reset_frecency();
        }
        if let Some(frecency) = frecency {
            let dirs_ref = &dirs;
            BACKGROUND_THREAD_POOL.install(|| {
                files.par_iter_mut().for_each_init(
                    || frecency.reader(),
                    |reader, file| {
                        file.update_frecency_scores(
                            reader,
                            arena,
                            base_path,
                            shared_base.as_deref(),
                            mode,
                        );
                        let score = file.access_frecency_score as i32;
                        if score > 0
                            && let Some(dir) = dirs_ref.get(file.parent_dir_index as usize)
                        {
                            dir.update_frecency_if_larger(score);
                        }
                    },
                );
            });
        }

        let base_count = files.len();
        let base_dirs_count = dirs.len();
        Self {
            files: StableVec::from_vec_with_reserve(files, MAX_OVERFLOW_FILES),
            indexable_count,
            base_count,
            live_count: base_count,
            watcher_additions: 0,
            dirs: StableVec::from_vec_with_reserve(dirs, MAX_OVERFLOW_FILES),
            base_dirs_count,
            live_dirs_count: base_dirs_count,
            layers: LayerStack::new(base_count, base_dirs_count),
            git,
            shared_base,
            bigram_index: None,
            bigram_overlay: None,
            chunked_paths: Some(store),
            ignore_rules: None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Attach {
    Import,
    // Re-attaching a layer of the previous index after a rescan.
    Adopt { open: bool },
}

// An exported layer minus its arena, so the arena can be handed over once
// the source stack has been consumed.
struct LayerExport {
    layer: LayerId,
    label: Option<Box<str>>,
    files: Vec<FileItem>,
    // `(path, global dir index)` sorted by path.
    dirs: Vec<(String, u32)>,
    sorted: bool,
}

impl LayerExport {
    // Own dirs keep their paths in `store`. A foreign parent is copied into it
    // unless `known` says the target index has it: attaching then finds it by
    // path, so repeated rescans do not grow the arena.
    fn into_snapshot(
        self,
        store: Arc<ChunkedPathStore>,
        dirs: &StableVec<DirItem>,
        known: impl Fn(&str) -> bool,
    ) -> LayerSnapshot {
        let mut store = Some(store);
        let mut builder: Option<ChunkedPathStoreBuilder> = None;
        let mut slots = Vec::with_capacity(self.dirs.len());
        let mut kept = Vec::with_capacity(self.dirs.len());
        for (path, global) in &self.dirs {
            let dir = &dirs[*global as usize];
            let item = if dir.layer_id() == self.layer {
                let mut dir = dir.clone();
                dir.set_layer(0);
                dir.set_deleted(false);
                dir
            } else if known(path) {
                slots.push(u32::MAX);
                continue;
            } else {
                let builder = builder.get_or_insert_with(|| {
                    let store = store.take().expect("the arena is taken once");
                    let owned = Arc::try_unwrap(store).unwrap_or_else(|arc| (*arc).clone());
                    ChunkedPathStoreBuilder::from_store(owned, 0)
                });
                DirItem::new(builder.add_dir_immediate(path), last_segment_offset(path))
            };
            slots.push(kept.len() as u32);
            kept.push(item);
        }

        let mut files = self.files;
        for file in &mut files {
            file.parent_dir_index = slots[file.parent_dir_index as usize];
        }
        let store = match builder {
            Some(builder) => Arc::new(builder.finish()),
            None => store.expect("the arena is kept when no dir was copied"),
        };

        LayerSnapshot {
            label: self.label,
            store,
            files,
            dirs: kept,
            layout: if self.sorted {
                SnapshotLayout::Sorted
            } else {
                SnapshotLayout::Unsorted
            },
        }
    }
}

impl FilePicker {
    /// Seals the open layer and opens a new one for [`add_entries`](Self::add_entries),
    /// returning its id.
    ///
    /// A layer keeps its id and label until the picker is dropped, rescans
    /// included. The watcher only writes to [`SESSION_LAYER`](crate::SESSION_LAYER),
    /// so a layer holds exactly the paths added or imported into it.
    ///
    /// Returns [`Error::LayerLimit`] when all [`MAX_OVERLAY_LAYERS`](crate::MAX_OVERLAY_LAYERS)
    /// overlays exist and [`Error::IndexBusy`] during a scan or post-scan indexing.
    pub fn create_layer(&mut self, label: Option<&str>) -> Result<LayerId, Error> {
        self.ensure_index_idle()?;
        let id = self.sync_data.create_layer(label)?;
        debug!(layer = id, ?label, "Opened new index layer");

        Ok(id)
    }

    /// Layer [`add_entries`](Self::add_entries) appends to. Until a layer is
    /// open this is [`SESSION_LAYER`](crate::SESSION_LAYER), where the watcher
    /// writes, and `add_entries` opens an unlabeled layer itself.
    #[inline]
    pub fn current_layer(&self) -> LayerId {
        self.sync_data.layers.open_layer().unwrap_or(SESSION_LAYER)
    }

    /// Overlay layers by id, so the session layer comes first. The base scan
    /// (layer 0) is not included — it is the whole of [`get_files`](Self::get_files)
    /// below `base_count`.
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

    /// Appends entries to the open layer without touching the filesystem,
    /// opening an unlabeled layer when none is open. Entries survive rescans,
    /// also when the scan finds them on disk. Paths already in the index are
    /// skipped and tombstoned ones are revived.
    /// Returns the number added.
    ///
    /// Grows the file table when needed. Returns [`Error::LayerLimit`] when a
    /// layer must be opened but all exist, and [`Error::IndexBusy`] during a
    /// scan or post-scan indexing.
    pub fn add_entries<'a>(
        &mut self,
        entries: impl IntoIterator<Item = IndexEntry<'a>>,
    ) -> Result<usize, Error> {
        let entries: Vec<IndexEntry<'a>> = entries.into_iter().collect();
        self.reserve_index(entries.len(), entries.len())?;
        let added = self.sync_data.add_entries(&entries)?;
        debug!(
            added,
            layer = self.current_layer(),
            "Added entries to index layer"
        );

        Ok(added)
    }

    /// Attaches `snapshot` as a new sealed layer and returns its id. Files
    /// already present in the index are skipped. The open layer stays open.
    ///
    /// Returns [`Error::LayerLimit`] when all [`MAX_OVERLAY_LAYERS`](crate::MAX_OVERLAY_LAYERS)
    /// overlays exist.
    pub fn import_layer(&mut self, snapshot: LayerSnapshot) -> Result<LayerId, Error> {
        self.ensure_index_idle()?;
        let id = self.sync_data.layers.free_id().ok_or(Error::LayerLimit)?;
        self.reserve_index(snapshot.files.len(), snapshot.dirs.len())?;
        let added = self.sync_data.attach_snapshot(id, snapshot, Attach::Import);
        info!(layer = id, added, "Imported index layer");

        Ok(id)
    }

    /// Replaces the base scan with `snapshot`, keeping explicit overlay layers.
    /// Frecency is applied when a tracker is given; git status stays unset
    /// until the next status refresh. A snapshot contains root-relative names
    /// and metadata. When reusing it in another worktree, the caller checks
    /// that its file set and ignore policy match or applies the worktree's changes.
    pub fn import_base(
        &mut self,
        snapshot: LayerSnapshot,
        frecency: Option<&FrecencyTracker>,
    ) -> Result<(), Error> {
        self.ensure_index_idle()?;

        let started = std::time::Instant::now();
        let mut sync = FileSync::from_snapshot(
            snapshot,
            &self.base_path,
            FileSync::discover_git(&self.base_path, self.ignore_options.disable_git),
            frecency,
            self.mode,
        );
        sync.ignore_rules = self.ignore_rules();
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
        self.ensure_index_idle()?;
        if self.sync_data.ensure_capacity(files, dirs) {
            self.cache_budget.reset();
        }

        Ok(())
    }

    // Layer mutations are refused while a scan or post-scan indexing owns the tables.
    fn ensure_index_idle(&self) -> Result<(), Error> {
        if self.is_scan_active()
            || self
                .signals
                .post_scan_indexing_active
                .load(Ordering::Acquire)
        {
            return Err(Error::IndexBusy);
        }

        Ok(())
    }
}

thread_local! {
    // Heap-backed scratch for parallel path comparisons: zeroing two 4 KiB
    // stack buffers per compare dominated the sort.
    static COMPARE_BUFS: RefCell<Box<[[u8; PATH_BUF_SIZE]; 2]>> =
        RefCell::new(Box::new([[0; PATH_BUF_SIZE]; 2]));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_picker::FilePickerOptions;

    fn scanned(root: &Path) -> FilePicker {
        std::fs::write(root.join("scanned.rs"), "").unwrap();
        let mut picker = FilePicker::new(FilePickerOptions {
            base_path: root.to_string_lossy().into_owned(),
            watch: false,
            enable_home_dir_scanning: true,
            ..Default::default()
        })
        .unwrap();
        picker.collect_files().unwrap();
        picker
    }

    #[test]
    fn a_watcher_file_gets_a_parent_after_explicit_paths_fill_the_dir_table() {
        let tmp = tempfile::tempdir().unwrap();
        let mut picker = scanned(tmp.path());
        let free = picker.sync_data.dirs.capacity() - picker.sync_data.dirs.len();
        // Room for the files, so only the dir table runs out.
        picker
            .sync_data
            .ensure_capacity(free + MAX_OVERFLOW_FILES, 0);
        let paths: Vec<String> = (0..free).map(|i| format!("v{i}/f.rs")).collect();
        picker
            .add_entries(paths.iter().map(|p| IndexEntry::new(p)))
            .unwrap();
        assert_eq!(
            picker.sync_data.dirs.len(),
            picker.sync_data.dirs.capacity()
        );

        let created = tmp.path().join("new/real.rs");
        std::fs::create_dir_all(created.parent().unwrap()).unwrap();
        std::fs::write(&created, "").unwrap();
        assert!(picker.add_new_file(&created).is_some());
        let session = picker.export_layer(SESSION_LAYER).unwrap();
        assert_eq!(session.files.len(), 1);
    }

    #[test]
    fn growing_only_the_dir_table_keeps_the_cache_budget() {
        let tmp = tempfile::tempdir().unwrap();
        let mut picker = scanned(tmp.path());
        picker.cache_budget.cached_count.store(1, Ordering::Relaxed);

        let sync = &picker.sync_data;
        let free_dirs = sync.dirs.capacity() - sync.dirs.len();
        picker.reserve_index(0, free_dirs + 1).unwrap();
        assert!(picker.sync_data.dirs.capacity() > free_dirs);
        assert_eq!(picker.cache_budget.cached_count.load(Ordering::Relaxed), 1);

        let sync = &picker.sync_data;
        let free_files = sync.files.capacity() - sync.files.len();
        picker.reserve_index(free_files + 1, 0).unwrap();
        assert_eq!(picker.cache_budget.cached_count.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn appending_to_the_open_layer_leaves_an_imported_layer_in_place() {
        let tmp = tempfile::tempdir().unwrap();
        let mut picker = scanned(tmp.path());
        let open = picker.create_layer(Some("open")).unwrap();
        picker.add_entries([IndexEntry::new("open/a.rs")]).unwrap();
        let mut builder = crate::LayerBuilder::new(Some("import"));
        for path in ["imp/b.rs", "imp/deep/c.rs"] {
            assert!(builder.push(&IndexEntry::new(path)));
        }
        let imported = picker.import_layer(builder.finish()).unwrap();
        let session_file = tmp.path().join("sess/x.rs");
        std::fs::create_dir_all(session_file.parent().unwrap()).unwrap();
        std::fs::write(&session_file, "").unwrap();
        assert!(picker.add_new_file(&session_file).is_some());

        let range = |picker: &FilePicker| {
            let sync = &picker.sync_data;
            (
                sync.layers.file_range(imported, sync.files.len()),
                sync.layers.dir_range(imported, sync.dirs.len()),
            )
        };
        let before = range(&picker);
        picker
            .add_entries([IndexEntry::new("open/new/d.rs")])
            .unwrap();

        assert_eq!(range(&picker), before);
        assert_eq!(picker.current_layer(), open);
        for path in [
            "open/a.rs",
            "open/new/d.rs",
            "imp/b.rs",
            "imp/deep/c.rs",
            "sess/x.rs",
        ] {
            let idx = picker.sync_data.find_relative_path_index(path).unwrap();
            assert_eq!(picker.sync_data.files[idx].relative_path(&picker), path);
        }
        for dir in ["open/", "open/new/", "imp/", "imp/deep/", "sess/"] {
            assert!(picker.sync_data.find_dir_index(dir).is_some(), "{dir}");
        }
    }

    #[test]
    fn a_direct_base_import_drops_the_source_status_and_ranking() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let mut snapshot = scanned(source.path()).export_layer(0).unwrap();
        for file in &mut snapshot.files {
            file.git_status = Some(git2::Status::WT_MODIFIED);
            file.git_recency_score = 5;
            file.access_frecency_score = 7;
            file.modification_frecency_score = 3;
        }

        let mut picker = scanned(target.path());
        picker.import_base(snapshot, None).unwrap();
        let file = &picker.get_files()[0];
        assert_eq!(file.git_status, None);
        assert_eq!(
            (
                file.git_recency_score,
                file.access_frecency_score,
                file.modification_frecency_score
            ),
            (0, 0, 0)
        );
    }

    #[test]
    fn adopting_a_layer_across_a_rescan_keeps_its_arena() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("scanned.rs"), "").unwrap();
        let mut picker = FilePicker::new(FilePickerOptions {
            base_path: tmp.path().to_string_lossy().into_owned(),
            watch: false,
            enable_home_dir_scanning: true,
            ..Default::default()
        })
        .unwrap();
        picker.collect_files().unwrap();

        // A root-level entry makes the layer's only dir foreign (it lives in the base arena).
        let kept = picker.create_layer(Some("kept")).unwrap();
        picker.add_entries([IndexEntry::new("virtual.rs")]).unwrap();
        picker.create_layer(None).unwrap();
        let before = picker.sync_data.layer_arenas().get(kept).as_ptr();

        picker.collect_files().unwrap();

        let kept = picker
            .layers()
            .iter()
            .find(|layer| layer.label.as_deref() == Some("kept"))
            .map(|layer| layer.id)
            .expect("explicit layer survives the rescan");
        assert_eq!(picker.sync_data.layer_arenas().get(kept).as_ptr(), before);
        assert_eq!(picker.files_in_layer(kept).len(), 1);
    }

    #[test]
    fn growing_an_older_layer_keeps_parents_and_ranges_consistent() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("scanned.rs"), "").unwrap();
        let mut picker = FilePicker::new(FilePickerOptions {
            base_path: tmp.path().to_string_lossy().into_owned(),
            watch: false,
            enable_home_dir_scanning: true,
            ..Default::default()
        })
        .unwrap();
        picker.collect_files().unwrap();

        picker.create_layer(Some("old")).unwrap();
        picker.add_entries([IndexEntry::new("old/a.rs")]).unwrap();
        let mut builder = crate::LayerBuilder::new(Some("import"));
        assert!(builder.push(&IndexEntry::new("imp/b.rs")));
        picker.import_layer(builder.finish()).unwrap();
        for path in ["sess/c.rs", "sess/deep/d.rs"] {
            let full = tmp.path().join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(&full, "").unwrap();
            assert!(picker.add_new_file(&full).is_some());
        }
        picker
            .add_entries([
                IndexEntry::new("old/new/e.rs"),
                IndexEntry::new("sess/f.rs"),
                IndexEntry::new("imp/g.rs"),
            ])
            .unwrap();

        let sync = &picker.sync_data;
        let arenas = sync.layer_arenas();
        let (mut file_buf, mut dir_buf) = ([0u8; PATH_BUF_SIZE], [0u8; PATH_BUF_SIZE]);
        for file in &sync.files[sync.base_count..] {
            let path = file
                .path
                .read_to_buf(arenas.get(file.layer_id()), &mut file_buf);
            let dir = &sync.dirs[file.parent_dir_index as usize];
            let dir_path = dir.read_relative_path(arenas.get(dir.layer_id()), &mut dir_buf);
            assert_eq!(dir_path, &path[..file.path.filename_offset as usize]);
        }
        for id in [SESSION_LAYER, 2, 3] {
            let (start, end) = sync.layers.file_range(id, sync.files.len()).unwrap();
            assert!(sync.files[start..end].iter().all(|f| f.layer_id() == id));
            let (start, end) = sync.layers.dir_range(id, sync.dirs.len()).unwrap();
            assert!(sync.dirs[start..end].iter().all(|d| d.layer_id() == id));
        }
        assert_eq!(picker.files_in_layer(2).len(), 4);
        assert_eq!(picker.files_in_layer(SESSION_LAYER).len(), 2);
    }

    #[test]
    fn creating_a_layer_is_refused_during_post_scan_indexing() {
        let tmp = tempfile::tempdir().unwrap();
        let mut picker = FilePicker::new(FilePickerOptions {
            base_path: tmp.path().to_string_lossy().into_owned(),
            watch: false,
            enable_home_dir_scanning: true,
            ..Default::default()
        })
        .unwrap();
        picker.collect_files().unwrap();

        picker
            .signals
            .post_scan_indexing_active
            .store(true, Ordering::Release);
        assert!(matches!(picker.create_layer(None), Err(Error::IndexBusy)));
        picker
            .signals
            .post_scan_indexing_active
            .store(false, Ordering::Release);
        assert_eq!(picker.create_layer(None).unwrap(), SESSION_LAYER + 1);
    }
}
