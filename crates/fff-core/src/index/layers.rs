use smallvec::SmallVec;
use std::sync::Arc;

use crate::constants::MAX_OVERFLOW_FILES;
use crate::simd_path::{ArenaPtr, ChunkedPathStore, ChunkedPathStoreBuilder};

/// Index of an arena layer. `0` is the base scan, `1..` are overlays.
pub type LayerId = u8;

/// Base scan plus four append-only overlays.
pub const MAX_INDEX_LAYERS: usize = 5;

/// How many overlays can exist on top of the base scan.
pub const MAX_OVERLAY_LAYERS: usize = MAX_INDEX_LAYERS - 1;

/// Overlay that collects every file the watcher added since the picker started.
pub const SESSION_LAYER: LayerId = 1;

/// Label given to [`SESSION_LAYER`] when the stack is created.
pub const SESSION_LAYER_LABEL: &str = "session";

// Layer id occupies three flag bits, so the arena table is padded to 8 slots
// and indexed with a mask — no bounds branch on the scoring hot path.
const LAYER_SLOTS: usize = 8;

pub(crate) const LAYER_SHIFT: u8 = 3;
pub(crate) const LAYER_MASK: u8 = 0b111 << LAYER_SHIFT;

/// Arena pointer per layer, resolved once per search and copied into workers.
#[derive(Clone, Copy, Debug)]
pub struct LayerArenas {
    ptrs: [ArenaPtr; LAYER_SLOTS],
}

impl LayerArenas {
    /// Every layer resolves to the same arena. Used when a slice is known to
    /// hold items of a single layer.
    #[inline]
    pub fn uniform(arena: ArenaPtr) -> Self {
        Self {
            ptrs: [arena; LAYER_SLOTS],
        }
    }

    #[inline]
    pub fn base(&self) -> ArenaPtr {
        self.ptrs[0]
    }

    #[inline]
    pub fn get(&self, layer: LayerId) -> ArenaPtr {
        self.ptrs[(layer & 0b111) as usize]
    }
}

/// `(files_start, files_end, layer_id, arena)` for every overlay, oldest first.
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
    /// `true` for the single writable layer at the top of the stack.
    pub is_open: bool,
}

#[derive(Clone)]
enum LayerStore {
    // Writable top layer. `None` until the first path lands in it, so an
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
}

#[derive(Clone)]
struct IndexLayer {
    label: Option<Box<str>>,
    // First `files` / `dirs` index owned by this layer.
    files_start: usize,
    dirs_start: usize,
    store: LayerStore,
    // Files sorted by relative path bytes, so lookups can binary search.
    sorted: bool,
}

// Overlays above the immutable base scan; each owns a contiguous range of
// `files`/`dirs` plus its own path arena, costing nothing per file.
#[derive(Clone)]
pub(crate) struct LayerStack {
    layers: Vec<IndexLayer>,
}

impl LayerStack {
    pub fn new(files_start: usize, dirs_start: usize) -> Self {
        let mut layers = Vec::with_capacity(MAX_OVERLAY_LAYERS);
        layers.push(IndexLayer {
            label: Some(SESSION_LAYER_LABEL.into()),
            files_start,
            dirs_start,
            store: LayerStore::Open(None),
            sorted: false,
        });

        Self { layers }
    }

    #[inline]
    pub fn top_id(&self) -> LayerId {
        self.layers.len() as LayerId
    }

    #[inline]
    pub fn arenas(&self, base: ArenaPtr) -> LayerArenas {
        let mut arenas = LayerArenas::uniform(ArenaPtr::null());
        arenas.ptrs[0] = base;
        for (i, layer) in self.layers.iter().enumerate() {
            arenas.ptrs[i + 1] = layer.store.arena_ptr();
        }

        arenas
    }

    // Arena of the topmost layer, backing the legacy two-arena view exposed
    // through `FFFStringStorage::overflow_arena`.
    #[inline]
    pub fn top_arena(&self) -> ArenaPtr {
        self.layers
            .last()
            .map(|l| l.store.arena_ptr())
            .unwrap_or(ArenaPtr::null())
    }

    #[inline]
    pub fn top_is_open(&self) -> bool {
        matches!(self.layers.last(), Some(l) if matches!(l.store, LayerStore::Open(_)))
    }

    // Path builder for the writable top layer, allocated on first use.
    // Callers open a new layer first when the top is sealed.
    pub fn top_builder(&mut self) -> &mut ChunkedPathStoreBuilder {
        let top = self.layers.last_mut().expect("layer stack is never empty");
        top.sorted = false;

        match &mut top.store {
            LayerStore::Open(slot) => {
                slot.get_or_insert_with(|| ChunkedPathStoreBuilder::new(MAX_OVERFLOW_FILES))
            }
            LayerStore::Sealed(_) => unreachable!("sealed layers are never written"),
        }
    }

    // `(start, end, arena, sorted)` per overlay for point lookups.
    pub fn lookup_ranges(
        &self,
        files_len: usize,
    ) -> impl Iterator<Item = (usize, usize, ArenaPtr, bool)> + '_ {
        self.file_ranges(files_len)
            .into_iter()
            .zip(self.layers.iter())
            .map(|((start, end, _, arena), layer)| (start, end, arena, layer.sorted))
    }

    // `files_len` closes the range of the open top layer.
    pub fn file_ranges(&self, files_len: usize) -> LayerRanges {
        let mut ranges = LayerRanges::new();
        for (i, layer) in self.layers.iter().enumerate() {
            let (start, end) = self.range_at(i, files_len, |l| l.files_start);
            ranges.push((start, end, (i + 1) as LayerId, layer.store.arena_ptr()));
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

    pub fn seal_top(&mut self) {
        let Some(top) = self.layers.last_mut() else {
            return;
        };

        if let LayerStore::Open(slot) = &mut top.store {
            let sealed = slot
                .take()
                .map(|builder| builder.finish())
                .unwrap_or_else(ChunkedPathStore::empty);

            top.store = LayerStore::Sealed(Arc::new(sealed));
        }
    }

    pub fn open_new(&mut self, label: Option<&str>, files_start: usize, dirs_start: usize) {
        self.layers.push(IndexLayer {
            label: label.map(Box::from),
            files_start,
            dirs_start,
            store: LayerStore::Open(None),
            sorted: false,
        });
    }

    // The caller sorts the pushed range before any lookup reaches it.
    pub fn push_sealed(
        &mut self,
        label: Option<Box<str>>,
        store: Arc<ChunkedPathStore>,
        files_start: usize,
        dirs_start: usize,
    ) {
        self.layers.push(IndexLayer {
            label,
            files_start,
            dirs_start,
            store: LayerStore::Sealed(store),
            sorted: true,
        });
    }

    pub fn set_sorted(&mut self, id: LayerId, sorted: bool) {
        if let Some(idx) = self.index_of(id) {
            self.layers[idx].sorted = sorted;
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

    #[inline]
    pub fn is_full(&self) -> bool {
        self.layers.len() >= MAX_OVERLAY_LAYERS
    }

    // Position of the older of the two newest layers, which a compaction
    // merges. Only meaningful when the stack is full.
    #[inline]
    pub fn merge_candidate(&self) -> usize {
        self.layers.len() - 2
    }

    // Replaces layers `[at, at + 1]` with one layer owning `store`. Callers must
    // have rewritten the affected paths into `store` already.
    pub fn merge_at(&mut self, at: usize, store: ChunkedPathStore) {
        let label = self.layers[at].label.take();
        let files_start = self.layers[at].files_start;
        let dirs_start = self.layers[at].dirs_start;

        self.layers.remove(at + 1);
        self.layers[at] = IndexLayer {
            label,
            files_start,
            dirs_start,
            store: LayerStore::Sealed(Arc::new(store)),
            sorted: false,
        };
    }

    pub fn overlay_arena_bytes(&self) -> usize {
        self.layers.iter().map(|l| l.store.heap_bytes()).sum()
    }

    pub fn info(&self, files: &[crate::types::FileItem], dirs_len: usize) -> Vec<LayerInfo> {
        let files_len = files.len();

        self.layers
            .iter()
            .enumerate()
            .map(|(i, layer)| {
                let id = (i + 1) as LayerId;
                let (start, end) = self.file_range(id, files_len).unwrap_or((0, 0));
                let (dir_start, dir_end) = self.dir_range(id, dirs_len).unwrap_or((0, 0));

                LayerInfo {
                    id,
                    label: layer.label.as_ref().map(|l| l.to_string()),
                    file_count: end - start,
                    live_file_count: files[start..end].iter().filter(|f| !f.is_deleted()).count(),
                    dir_count: dir_end - dir_start,
                    arena_bytes: layer.store.heap_bytes(),
                    is_open: matches!(layer.store, LayerStore::Open(_)),
                }
            })
            .collect()
    }

    #[inline]
    fn index_of(&self, id: LayerId) -> Option<usize> {
        let idx = (id as usize).checked_sub(1)?;
        (idx < self.layers.len()).then_some(idx)
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

    fn add(stack: &mut LayerStack, path: &str) {
        stack.top_builder().add_dir_immediate(path);
    }

    #[test]
    fn starts_with_the_session_layer_open() {
        let stack = LayerStack::new(10, 3);

        assert_eq!(stack.top_id(), SESSION_LAYER);
        assert!(!stack.is_full());
        assert_eq!(stack.overlay_arena_bytes(), 0);
    }

    #[test]
    fn untouched_layer_allocates_no_arena() {
        let mut stack = LayerStack::new(0, 0);
        stack.seal_top();
        stack.open_new(None, 0, 0);

        assert_eq!(stack.overlay_arena_bytes(), 0);
        // The open top layer has no arena until a path lands in it.
        assert!(stack.top_arena().as_ptr().is_null());
    }

    #[test]
    fn sealing_keeps_the_arena_addressable() {
        let mut stack = LayerStack::new(0, 0);
        add(&mut stack, "src/");
        let bytes_open = stack.overlay_arena_bytes();

        stack.seal_top();

        assert!(bytes_open > 0);
        assert_eq!(stack.overlay_arena_bytes(), bytes_open);
        assert!(!stack.arenas(ArenaPtr::null()).get(1).as_ptr().is_null());
    }

    #[test]
    fn arenas_are_distinct_per_layer() {
        let mut stack = LayerStack::new(0, 0);
        add(&mut stack, "src/");
        stack.seal_top();
        stack.open_new(Some("second"), 4, 2);
        add(&mut stack, "docs/");

        let base = [0u8; 16];
        let arenas = stack.arenas(ArenaPtr::new(base.as_ptr(), std::ptr::null()));
        assert_eq!(arenas.base().as_ptr(), base.as_ptr());
        assert_ne!(arenas.get(1).as_ptr(), arenas.get(2).as_ptr());
        assert_eq!(stack.top_id(), 2);
    }

    #[test]
    fn file_ranges_close_the_open_layer_at_the_current_length() {
        let mut stack = LayerStack::new(100, 10);
        stack.seal_top();
        stack.open_new(None, 104, 11);

        let ranges = stack.file_ranges(110);
        assert_eq!(ranges.len(), 2);
        assert_eq!((ranges[0].0, ranges[0].1, ranges[0].2), (100, 104, 1));
        assert_eq!((ranges[1].0, ranges[1].1, ranges[1].2), (104, 110, 2));
    }

    #[test]
    fn ranges_stay_in_bounds_when_the_index_is_shorter_than_a_layer_start() {
        let mut stack = LayerStack::new(100, 10);
        stack.seal_top();
        stack.open_new(None, 104, 11);

        // A rescan can shrink the index below a recorded layer start.
        let ranges = stack.file_ranges(100);
        assert!(
            ranges
                .iter()
                .all(|&(start, end, ..)| start <= end && end <= 100)
        );
        assert_eq!(stack.file_range(2, 100), Some((100, 100)));
    }

    #[test]
    fn fills_up_after_max_overlay_layers() {
        let mut stack = LayerStack::new(0, 0);
        for _ in 1..MAX_OVERLAY_LAYERS {
            stack.seal_top();
            stack.open_new(None, 0, 0);
        }

        assert!(stack.is_full());
        assert_eq!(stack.top_id() as usize, MAX_OVERLAY_LAYERS);
    }

    #[test]
    fn merge_candidate_never_picks_the_session_layer() {
        let mut stack = LayerStack::new(0, 0);
        while !stack.is_full() {
            stack.seal_top();
            stack.open_new(None, 0, 0);
        }

        assert_eq!(stack.merge_candidate(), MAX_OVERLAY_LAYERS - 2);
        assert!(stack.merge_candidate() >= 1);
    }

    #[test]
    fn merge_at_collapses_two_layers_into_one() {
        let mut stack = LayerStack::new(0, 0);
        stack.seal_top();
        stack.open_new(Some("second"), 5, 1);
        stack.seal_top();
        stack.open_new(Some("third"), 9, 2);
        stack.seal_top();

        stack.merge_at(1, ChunkedPathStore::empty());

        assert_eq!(stack.top_id(), 2);
        let ranges = stack.file_ranges(12);
        assert_eq!(ranges.len(), 2);
        // The merged layer inherits the older layer's start and label.
        assert_eq!((ranges[1].0, ranges[1].1), (5, 12));
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

        // Layer id comes from three flag bits, so ids up to 7 must not panic.
        for layer in 0..=7u8 {
            assert_eq!(arenas.get(layer).as_ptr(), base.as_ptr());
        }
    }
}
