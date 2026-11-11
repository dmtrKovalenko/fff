use smallvec::SmallVec;
use std::sync::Arc;

use crate::constants::MAX_OVERFLOW_FILES;
use crate::index::path_index::PathIndex;
use crate::simd_path::{ArenaPtr, ChunkedPathStore, ChunkedPathStoreBuilder};

/// Index of an arena layer. `0` is the base scan, `1..` are overlays.
pub type LayerId = u8;

const LAYER_ID_MASK: u8 = 0b11;

/// Base scan plus three overlays.
pub const MAX_INDEX_LAYERS: usize = LAYER_ID_MASK as usize + 1;

/// How many overlays can exist on top of the base scan, the session layer included.
pub const MAX_OVERLAY_LAYERS: usize = MAX_INDEX_LAYERS - 1;

/// Overlay that collects every file the watcher added since the last scan.
pub const SESSION_LAYER: LayerId = 1;

/// Label given to [`SESSION_LAYER`] when the stack is created.
pub const SESSION_LAYER_LABEL: &str = "session";

pub(crate) const LAYER_SHIFT: u8 = 2;
pub(crate) const LAYER_MASK: u8 = LAYER_ID_MASK << LAYER_SHIFT;

/// Arena pointer per layer, resolved once per search and copied into workers.
#[derive(Clone, Copy, Debug)]
pub struct LayerArenas {
    ptrs: [ArenaPtr; MAX_INDEX_LAYERS],
}

impl LayerArenas {
    /// Every layer resolves to the same arena. Used when a slice is known to
    /// hold items of a single layer.
    #[inline]
    pub fn uniform(arena: ArenaPtr) -> Self {
        Self {
            ptrs: [arena; MAX_INDEX_LAYERS],
        }
    }

    #[inline]
    pub fn base(&self) -> ArenaPtr {
        self.ptrs[0]
    }

    #[inline]
    pub fn get(&self, layer: LayerId) -> ArenaPtr {
        self.ptrs[(layer & LAYER_ID_MASK) as usize]
    }
}

/// `(files_start, files_end, layer_id, arena)` for every overlay in storage order.
pub type LayerRanges = SmallVec<[(usize, usize, LayerId, ArenaPtr); MAX_OVERLAY_LAYERS]>;

/// Public snapshot of one layer, returned by `FilePicker::layers`.
#[derive(Debug, Clone)]
pub struct LayerInfo {
    pub id: LayerId,
    pub label: Option<String>,
    /// Index slots owned by the layer, tombstones included.
    pub file_count: usize,
    pub live_file_count: usize,
    pub dir_count: usize,
    /// Bytes held by the layer's path arena (excludes the dedup table).
    pub arena_bytes: usize,
    /// `true` for the session layer and the layer `create_layer` opened last.
    pub is_open: bool,
}

#[derive(Clone)]
enum LayerStore {
    // Writable layer. `None` until the first path lands in it, so an
    // untouched layer costs nothing.
    Open(Option<ChunkedPathStoreBuilder>),
    // Frozen: the chunk dedup table is dropped, only the arena survives.
    Sealed(Arc<ChunkedPathStore>),
}

impl LayerStore {
    #[inline]
    fn arena_ptr(&self) -> ArenaPtr {
        match self {
            LayerStore::Open(Some(builder)) => builder.as_arena_ptr(),
            LayerStore::Open(None) => ArenaPtr::null(),
            LayerStore::Sealed(store) => store.as_arena_ptr(),
        }
    }

    fn heap_bytes(&self) -> usize {
        match self {
            LayerStore::Open(Some(builder)) => builder.heap_bytes(),
            LayerStore::Open(None) => 0,
            LayerStore::Sealed(store) => store.heap_bytes(),
        }
    }

    #[inline]
    fn is_open(&self) -> bool {
        matches!(self, LayerStore::Open(_))
    }
}

#[derive(Clone)]
struct IndexLayer {
    id: LayerId,
    label: Option<Box<str>>,
    // First `files` / `dirs` index owned by this layer.
    files_start: usize,
    dirs_start: usize,
    store: LayerStore,
    // Files sorted by `(dir, name)`, so lookups can binary search. Unsorted
    // layers are looked up through `index`.
    sorted: bool,
    index: PathIndex,
}

// Overlays above the base scan in storage order. Each owns a contiguous range
// of `files`/`dirs` plus its own path arena. Ids never move between layers, and
// the session layer stays last so watcher additions append in place.
#[derive(Clone)]
pub(crate) struct LayerStack {
    layers: Vec<IndexLayer>,
}

impl LayerStack {
    pub fn new(files_start: usize, dirs_start: usize) -> Self {
        let mut layers = Vec::with_capacity(MAX_OVERLAY_LAYERS);
        layers.push(IndexLayer {
            id: SESSION_LAYER,
            label: Some(SESSION_LAYER_LABEL.into()),
            files_start,
            dirs_start,
            store: LayerStore::Open(None),
            sorted: false,
            index: PathIndex::default(),
        });

        Self { layers }
    }

    #[inline]
    pub fn arenas(&self, base: ArenaPtr) -> LayerArenas {
        let mut arenas = LayerArenas::uniform(ArenaPtr::null());
        arenas.ptrs[0] = base;
        for layer in &self.layers {
            arenas.ptrs[layer.id as usize] = layer.store.arena_ptr();
        }

        arenas
    }

    // Session arena, backing the legacy two-arena view exposed through
    // `FFFStringStorage::overflow_arena`.
    #[inline]
    pub fn top_arena(&self) -> ArenaPtr {
        self.layers
            .last()
            .map(|l| l.store.arena_ptr())
            .unwrap_or(ArenaPtr::null())
    }

    pub fn session_builder(&mut self) -> &mut ChunkedPathStoreBuilder {
        self.builder(SESSION_LAYER)
            .expect("the session layer is always open")
    }

    // Path builder of an open layer, allocated on first use.
    pub fn builder(&mut self, id: LayerId) -> Option<&mut ChunkedPathStoreBuilder> {
        let layer = self.layer_mut(id)?;
        let LayerStore::Open(slot) = &mut layer.store else {
            return None;
        };
        layer.sorted = false;

        Some(slot.get_or_insert_with(|| ChunkedPathStoreBuilder::new(MAX_OVERFLOW_FILES)))
    }

    // Newest layer opened by `create_layer` that still accepts paths.
    pub fn open_layer(&self) -> Option<LayerId> {
        self.layers
            .iter()
            .rev()
            .find(|l| l.id != SESSION_LAYER && l.store.is_open())
            .map(|l| l.id)
    }

    pub fn free_id(&self) -> Option<LayerId> {
        (SESSION_LAYER + 1..=MAX_OVERLAY_LAYERS as LayerId).find(|&id| self.index_of(id).is_none())
    }

    pub fn ids(&self) -> impl Iterator<Item = LayerId> + '_ {
        self.layers.iter().map(|l| l.id)
    }

    pub fn insert_open(&mut self, id: LayerId, label: Option<&str>) {
        self.insert(id, label.map(Box::from), LayerStore::Open(None), false);
    }

    // The caller sorts the inserted range before any lookup reaches it when
    // `sorted` is true.
    pub fn insert_sealed(
        &mut self,
        id: LayerId,
        label: Option<Box<str>>,
        store: Arc<ChunkedPathStore>,
        sorted: bool,
    ) {
        self.insert(id, label, LayerStore::Sealed(store), sorted);
    }

    // New layers start empty right below the session layer.
    fn insert(&mut self, id: LayerId, label: Option<Box<str>>, store: LayerStore, sorted: bool) {
        debug_assert!(id != SESSION_LAYER && self.index_of(id).is_none());
        let session = self.layers.last().expect("layer stack is never empty");
        let layer = IndexLayer {
            id,
            label,
            files_start: session.files_start,
            dirs_start: session.dirs_start,
            store,
            sorted,
            index: PathIndex::default(),
        };
        self.layers.insert(self.layers.len() - 1, layer);
    }

    // Moves every layer above `id` after slots were inserted at the end of it.
    pub fn shift_above(&mut self, id: LayerId, files: usize, dirs: usize) {
        let Some(at) = self.index_of(id) else {
            return;
        };
        for layer in &mut self.layers[at + 1..] {
            layer.files_start += files;
            layer.dirs_start += dirs;
        }
    }

    // `(start, end, arena, sorted, index)` per overlay for point lookups.
    pub fn lookup_ranges(
        &self,
        files_len: usize,
    ) -> impl Iterator<Item = (usize, usize, ArenaPtr, bool, &PathIndex)> + '_ {
        self.file_ranges(files_len)
            .into_iter()
            .zip(self.layers.iter())
            .map(|((start, end, _, arena), layer)| (start, end, arena, layer.sorted, &layer.index))
    }

    // `files_len` closes the range of the session layer.
    pub fn file_ranges(&self, files_len: usize) -> LayerRanges {
        let mut ranges = LayerRanges::new();
        for (i, layer) in self.layers.iter().enumerate() {
            let (start, end) = self.range_at(i, files_len, |l| l.files_start);
            ranges.push((start, end, layer.id, layer.store.arena_ptr()));
        }

        ranges
    }

    pub fn file_range(&self, id: LayerId, files_len: usize) -> Option<(usize, usize)> {
        let idx = self.index_of(id)?;
        Some(self.range_at(idx, files_len, |l| l.files_start))
    }

    pub fn dir_range(&self, id: LayerId, dirs_len: usize) -> Option<(usize, usize)> {
        let idx = self.index_of(id)?;
        Some(self.range_at(idx, dirs_len, |l| l.dirs_start))
    }

    fn range_at(
        &self,
        idx: usize,
        len: usize,
        start_of: fn(&IndexLayer) -> usize,
    ) -> (usize, usize) {
        let end = self.layers.get(idx + 1).map_or(len, start_of).min(len);
        (start_of(&self.layers[idx]).min(end), end)
    }

    pub fn seal(&mut self, id: LayerId) {
        let Some(layer) = self.layer_mut(id) else {
            return;
        };

        if let LayerStore::Open(slot) = &mut layer.store {
            let sealed = slot
                .take()
                .map(|builder| builder.finish())
                .unwrap_or_else(ChunkedPathStore::empty);

            layer.store = LayerStore::Sealed(Arc::new(sealed));
        }
    }

    // Makes a sealed layer writable again; a shared arena is copied first.
    pub fn reopen(&mut self, id: LayerId) {
        let Some(layer) = self.layer_mut(id) else {
            return;
        };

        let store = std::mem::replace(&mut layer.store, LayerStore::Open(None));
        layer.store = match store {
            LayerStore::Sealed(store) => {
                let owned = Arc::try_unwrap(store).unwrap_or_else(|arc| (*arc).clone());
                LayerStore::Open(Some(ChunkedPathStoreBuilder::from_store(owned, 0)))
            }
            open => open,
        };
    }

    pub fn set_sorted(&mut self, id: LayerId, sorted: bool) {
        if let Some(layer) = self.layer_mut(id) {
            layer.sorted = sorted;
            layer.index.reset();
        }
    }

    pub fn is_sorted(&self, id: LayerId) -> bool {
        self.index_of(id).is_some_and(|idx| self.layers[idx].sorted)
    }

    pub fn label(&self, id: LayerId) -> Option<Box<str>> {
        self.index_of(id)
            .and_then(|idx| self.layers[idx].label.clone())
    }

    // Arena of `id` as a shareable store; an open layer's arena is copied.
    pub fn store_snapshot(&self, id: LayerId) -> Option<Arc<ChunkedPathStore>> {
        let idx = self.index_of(id)?;
        Some(match &self.layers[idx].store {
            LayerStore::Sealed(store) => Arc::clone(store),
            LayerStore::Open(Some(builder)) => Arc::new(builder.clone().finish()),
            LayerStore::Open(None) => Arc::new(ChunkedPathStore::empty()),
        })
    }

    // Consumes the stack as `(id, arena, open)` in storage order. Each arena comes
    // out uniquely owned unless a snapshot clone of it is still alive.
    pub fn into_stores(self) -> Vec<(LayerId, Arc<ChunkedPathStore>, bool)> {
        self.layers
            .into_iter()
            .map(|layer| {
                let open = layer.store.is_open();
                let store = match layer.store {
                    LayerStore::Sealed(store) => store,
                    LayerStore::Open(Some(builder)) => Arc::new(builder.finish()),
                    LayerStore::Open(None) => Arc::new(ChunkedPathStore::empty()),
                };
                (layer.id, store, open)
            })
            .collect()
    }

    pub fn overlay_arena_bytes(&self) -> usize {
        self.layers.iter().map(|l| l.store.heap_bytes()).sum()
    }

    // Ordered by id, so the session layer comes first.
    pub fn info(&self, files: &[crate::types::FileItem], dirs_len: usize) -> Vec<LayerInfo> {
        let files_len = files.len();
        let mut info: Vec<LayerInfo> = self
            .layers
            .iter()
            .map(|layer| {
                let (start, end) = self.file_range(layer.id, files_len).unwrap_or((0, 0));
                let (dir_start, dir_end) = self.dir_range(layer.id, dirs_len).unwrap_or((0, 0));

                LayerInfo {
                    id: layer.id,
                    label: layer.label.as_ref().map(|l| l.to_string()),
                    file_count: end - start,
                    live_file_count: files[start..end].iter().filter(|f| !f.is_deleted()).count(),
                    dir_count: dir_end - dir_start,
                    arena_bytes: layer.store.heap_bytes(),
                    is_open: layer.store.is_open(),
                }
            })
            .collect();
        info.sort_unstable_by_key(|layer| layer.id);

        info
    }

    #[inline]
    fn index_of(&self, id: LayerId) -> Option<usize> {
        self.layers.iter().position(|l| l.id == id)
    }

    #[inline]
    fn layer_mut(&mut self, id: LayerId) -> Option<&mut IndexLayer> {
        self.layers.iter_mut().find(|l| l.id == id)
    }
}

impl std::fmt::Debug for LayerStack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LayerStack")
            .field("count", &self.layers.len())
            .field("overlay_arena_bytes", &self.overlay_arena_bytes())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn add(stack: &mut LayerStack, id: LayerId, path: &str) {
        stack.builder(id).unwrap().add_dir_immediate(path);
    }

    #[test]
    fn starts_with_the_session_layer_open() {
        let stack = LayerStack::new(10, 3);

        assert_eq!(stack.ids().collect::<Vec<_>>(), [SESSION_LAYER]);
        assert_eq!(stack.open_layer(), None);
        assert_eq!(stack.free_id(), Some(SESSION_LAYER + 1));
        assert_eq!(stack.overlay_arena_bytes(), 0);
    }

    #[test]
    fn untouched_layer_allocates_no_arena() {
        let mut stack = LayerStack::new(0, 0);
        stack.insert_open(2, None);

        assert_eq!(stack.overlay_arena_bytes(), 0);
        assert!(stack.arenas(ArenaPtr::null()).get(2).as_ptr().is_null());
    }

    #[test]
    fn sealing_keeps_the_arena_addressable() {
        let mut stack = LayerStack::new(0, 0);
        stack.insert_open(2, None);
        add(&mut stack, 2, "src/");
        let bytes_open = stack.overlay_arena_bytes();

        stack.seal(2);

        assert!(bytes_open > 0);
        assert_eq!(stack.overlay_arena_bytes(), bytes_open);
        assert!(!stack.arenas(ArenaPtr::null()).get(2).as_ptr().is_null());
        assert!(stack.builder(2).is_none());
        assert_eq!(stack.open_layer(), None);
    }

    #[test]
    fn reopening_keeps_existing_paths() {
        let mut stack = LayerStack::new(0, 0);
        stack.insert_open(2, None);
        let path = stack.builder(2).unwrap().add_dir_immediate("src/");
        stack.seal(2);

        stack.reopen(2);
        stack.builder(2).unwrap().add_dir_immediate("docs/");

        let mut buf = [0u8; crate::simd_path::PATH_BUF_SIZE];
        let arena = stack.arenas(ArenaPtr::null()).get(2);
        assert_eq!(path.read_to_buf(arena, &mut buf), "src/");
        assert_eq!(stack.open_layer(), Some(2));
    }

    #[test]
    fn arenas_are_distinct_per_layer() {
        let mut stack = LayerStack::new(0, 0);
        add(&mut stack, SESSION_LAYER, "src/");
        stack.insert_open(2, Some("second"));
        add(&mut stack, 2, "docs/");

        let base = [0u8; 16];
        let arenas = stack.arenas(ArenaPtr::new(base.as_ptr(), std::ptr::null()));
        assert_eq!(arenas.base().as_ptr(), base.as_ptr());
        assert_ne!(arenas.get(1).as_ptr(), arenas.get(2).as_ptr());
    }

    #[test]
    fn new_layers_go_below_the_session_layer() {
        let mut stack = LayerStack::new(100, 10);
        stack.insert_open(2, None);
        stack.shift_above(2, 4, 1);

        let ranges = stack.file_ranges(110);
        assert_eq!(ranges.len(), 2);
        assert_eq!((ranges[0].0, ranges[0].1, ranges[0].2), (100, 104, 2));
        assert_eq!(
            (ranges[1].0, ranges[1].1, ranges[1].2),
            (104, 110, SESSION_LAYER)
        );
        assert_eq!(stack.dir_range(SESSION_LAYER, 12), Some((11, 12)));
    }

    #[test]
    fn ranges_stay_in_bounds_when_the_index_is_shorter_than_a_layer_start() {
        let mut stack = LayerStack::new(100, 10);
        stack.insert_open(2, None);
        stack.shift_above(2, 4, 1);

        // A rescan can shrink the index below a recorded layer start.
        let ranges = stack.file_ranges(100);
        assert!(
            ranges
                .iter()
                .all(|&(start, end, ..)| start <= end && end <= 100)
        );
        assert_eq!(stack.file_range(SESSION_LAYER, 100), Some((100, 100)));
    }

    #[test]
    fn ids_are_never_reused_while_their_layer_exists() {
        let mut stack = LayerStack::new(0, 0);
        let mut ids = Vec::new();
        while let Some(id) = stack.free_id() {
            stack.insert_open(id, None);
            ids.push(id);
        }

        assert_eq!(ids.len(), MAX_OVERLAY_LAYERS - 1);
        assert_eq!(ids, (2..=MAX_OVERLAY_LAYERS as LayerId).collect::<Vec<_>>());
        assert_eq!(stack.ids().last(), Some(SESSION_LAYER));
        // The newest open layer receives explicit paths.
        assert_eq!(stack.open_layer(), ids.last().copied());
    }

    #[test]
    fn info_lists_layers_by_id() {
        let mut stack = LayerStack::new(0, 0);
        stack.insert_open(2, Some("second"));
        stack.insert_sealed(
            3,
            Some("third".into()),
            Arc::new(ChunkedPathStore::empty()),
            true,
        );

        let info = stack.info(&[], 0);
        let ids: Vec<_> = info.iter().map(|l| l.id).collect();
        assert_eq!(ids, [1, 2, 3]);
        let open: Vec<_> = info.iter().map(|l| l.is_open).collect();
        assert_eq!(open, [true, true, false]);
    }

    #[test]
    fn unknown_layer_ids_have_no_range() {
        let stack = LayerStack::new(0, 0);

        assert_eq!(stack.file_range(0, 10), None);
        assert_eq!(stack.dir_range(0, 10), None);
        assert_eq!(stack.file_range(7, 10), None);
    }

    #[test]
    fn arena_lookup_is_bounded_for_out_of_range_ids() {
        let base = [0u8; 16];
        let arenas = LayerArenas::uniform(ArenaPtr::new(base.as_ptr(), std::ptr::null()));

        for layer in 0..=u8::MAX {
            assert_eq!(arenas.get(layer).as_ptr(), base.as_ptr());
        }
    }
}
