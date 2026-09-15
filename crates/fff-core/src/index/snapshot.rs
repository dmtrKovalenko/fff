use std::io::{self, Write};
use std::path::Path;
use std::sync::Arc;

use rayon::prelude::*;

use crate::constants::PATH_BUF_SIZE;
use crate::error::Error;
use crate::parallelism::BACKGROUND_THREAD_POOL;
use crate::simd_path::{
    ChunkedPathStore, ChunkedPathStoreBuilder, ChunkedString, SIMD_CHUNK_BYTES, SimdChunk,
};
use crate::types::{DirItem, FileItem};

const MAGIC: &[u8; 8] = b"FFFLAYER";
const FORMAT_VERSION: u32 = 1;
const HEADER_BYTES: usize = 32;
const MIN_DIR_RECORD: usize = 8;
const MIN_FILE_RECORD: usize = 29;

/// How the files of a snapshot are ordered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum SnapshotLayout {
    /// Base scan order: indexable files first, each partition sorted by
    /// `(dir, name)`.
    BasePartitioned = 0,
    /// Every file sorted by `(dir, name)`; overlay lookups binary search it.
    Sorted = 1,
    Unsorted = 2,
}

/// One file handed to [`LayerBuilder`] or `FilePicker::add_entries`. Paths are
/// relative to the picker base and use `/` separators.
#[derive(Clone, Debug)]
pub struct IndexEntry<'a> {
    pub relative_path: &'a str,
    pub size: u64,
    /// Unix seconds. `0` when unknown.
    pub modified: u64,
    /// `None` classifies by extension.
    pub binary: Option<bool>,
}

impl<'a> IndexEntry<'a> {
    pub fn new(relative_path: &'a str) -> Self {
        Self {
            relative_path,
            size: 0,
            modified: 0,
            binary: None,
        }
    }

    pub fn with_metadata(mut self, size: u64, modified: u64) -> Self {
        self.size = size;
        self.modified = modified;
        self
    }

    pub fn with_binary(mut self, binary: bool) -> Self {
        self.binary = Some(binary);
        self
    }

    pub(crate) fn is_binary(&self) -> bool {
        self.binary.unwrap_or_else(|| {
            let name = &self.relative_path[filename_offset(self.relative_path)..];
            crate::file_picker::is_known_binary_extension_basename(name)
        })
    }
}

struct PendingEntry {
    path: String,
    filename_offset: u16,
    size: u64,
    modified: u64,
    binary: bool,
}

/// Accumulates files off-lock, then freezes them into a [`LayerSnapshot`] whose
/// paths live in one deduplicated arena. Nothing touches the filesystem.
pub struct LayerBuilder {
    label: Option<Box<str>>,
    entries: Vec<PendingEntry>,
}

impl LayerBuilder {
    pub fn new(label: Option<&str>) -> Self {
        Self {
            label: label.map(Box::from),
            entries: Vec::new(),
        }
    }

    pub fn with_capacity(label: Option<&str>, files: usize) -> Self {
        Self {
            label: label.map(Box::from),
            entries: Vec::with_capacity(files),
        }
    }

    /// `false` when the path is absolute, empty, or too long for the index.
    pub fn push(&mut self, entry: &IndexEntry<'_>) -> bool {
        let Some(path) = normalize_relative_path(entry.relative_path) else {
            return false;
        };

        self.entries.push(PendingEntry {
            filename_offset: filename_offset(&path) as u16,
            binary: entry.is_binary(),
            size: entry.size,
            modified: entry.modified,
            path,
        });
        true
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Sorts by `(dir, name)`, drops duplicate paths, derives every ancestor
    /// dir and packs paths into a fresh arena.
    pub fn finish(self) -> LayerSnapshot {
        let mut entries = self.entries;
        BACKGROUND_THREAD_POOL.install(|| {
            entries.par_sort_unstable_by(|a, b| {
                dir_name_key(&a.path, a.filename_offset)
                    .cmp(&dir_name_key(&b.path, b.filename_offset))
            });
        });
        entries.dedup_by(|a, b| a.path == b.path);

        let mut dir_paths: Vec<&str> = Vec::with_capacity(entries.len() / 8 + 1);
        let mut prev_dir = "\0";
        for entry in &entries {
            let dir = &entry.path[..entry.filename_offset as usize];
            if dir == prev_dir {
                continue;
            }
            prev_dir = dir;
            // Entries are dir-sorted so the ancestors of `dir` either were
            // pushed already or are pushed now, ahead of the sort below.
            let mut end = dir.len();
            while end > 0 {
                dir_paths.push(&dir[..end]);
                end = dir[..end - 1].rfind('/').map(|i| i + 1).unwrap_or(0);
            }
            dir_paths.push("");
        }
        dir_paths.sort_unstable();
        dir_paths.dedup();

        let mut store = ChunkedPathStoreBuilder::new(entries.len() + dir_paths.len());
        let dirs: Vec<DirItem> = dir_paths
            .iter()
            .map(|dir| DirItem::new(store.add_dir_immediate(dir), last_segment_offset(dir)))
            .collect();

        let mut files = Vec::with_capacity(entries.len());
        let mut prev_dir = "\0";
        let mut dir_idx = 0u32;
        for entry in &entries {
            let dir = &entry.path[..entry.filename_offset as usize];
            if dir != prev_dir {
                dir_idx = dir_paths
                    .binary_search(&dir)
                    .expect("ancestor dirs cover every file") as u32;
                prev_dir = dir;
            }

            let mut file = FileItem::new_raw(
                entry.filename_offset,
                entry.size,
                entry.modified,
                None,
                entry.binary,
            );
            file.set_path(store.add_file_immediate(&entry.path, entry.filename_offset));
            file.parent_dir_index = dir_idx;
            files.push(file);
        }

        LayerSnapshot {
            label: self.label,
            store: Arc::new(store.finish()),
            files,
            dirs,
            layout: SnapshotLayout::Sorted,
        }
    }
}

/// Self-contained, immutable copy of one index layer: files, dirs and the arena
/// their paths point into. Round-trips through [`LayerSnapshot::save`].
pub struct LayerSnapshot {
    pub(crate) label: Option<Box<str>>,
    pub(crate) store: Arc<ChunkedPathStore>,
    /// `parent_dir_index` is local to `dirs`; layer flag bits are clear.
    pub(crate) files: Vec<FileItem>,
    /// Sorted by relative path.
    pub(crate) dirs: Vec<DirItem>,
    pub(crate) layout: SnapshotLayout,
}

impl std::fmt::Debug for LayerSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LayerSnapshot")
            .field("label", &self.label)
            .field("files", &self.files.len())
            .field("dirs", &self.dirs.len())
            .field("arena_bytes", &self.store.heap_bytes())
            .field("layout", &self.layout)
            .finish()
    }
}

impl LayerSnapshot {
    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    pub fn dir_count(&self) -> usize {
        self.dirs.len()
    }

    pub fn arena_bytes(&self) -> usize {
        self.store.heap_bytes()
    }

    pub fn layout(&self) -> SnapshotLayout {
        self.layout
    }

    pub fn relative_paths(&self) -> impl Iterator<Item = String> + '_ {
        let arena = self.store.as_arena_ptr();
        self.files.iter().map(move |f| f.relative_path(arena))
    }

    pub fn dir_paths(&self) -> impl Iterator<Item = String> + '_ {
        let arena = self.store.as_arena_ptr();
        self.dirs.iter().map(move |d| d.relative_path(arena))
    }

    /// Writes the snapshot to `path`, returning the bytes written.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<u64, Error> {
        let path = path.as_ref();
        let io_err = |source| Error::LayerFile {
            path: path.to_path_buf(),
            source,
        };

        let file = std::fs::File::create(path).map_err(io_err)?;
        let mut writer = io::BufWriter::with_capacity(1 << 20, file);
        let written = self.write_to(&mut writer).map_err(io_err)?;
        writer.flush().map_err(io_err)?;
        Ok(written)
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = path.as_ref();
        let bytes = std::fs::read(path).map_err(|source| Error::LayerFile {
            path: path.to_path_buf(),
            source,
        })?;

        Self::from_bytes(&bytes)
    }

    // Little-endian: 32 B header, label padded to 16 B, raw chunk arena, raw
    // chunk index table, then dir and file records (see `write_chunked`).
    pub fn write_to<W: Write>(&self, mut w: W) -> io::Result<u64> {
        let label = self.label.as_deref().unwrap_or("").as_bytes();
        let chunks = self.store.chunks();
        let indices = self.store.indices();

        let mut header = [0u8; HEADER_BYTES];
        header[..8].copy_from_slice(MAGIC);
        header[8..12].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
        header[12] = self.layout as u8;
        header[14..16].copy_from_slice(&(label.len() as u16).to_le_bytes());
        header[16..20].copy_from_slice(&(self.files.len() as u32).to_le_bytes());
        header[20..24].copy_from_slice(&(self.dirs.len() as u32).to_le_bytes());
        header[24..28].copy_from_slice(&(chunks.len() as u32).to_le_bytes());
        header[28..32].copy_from_slice(&(indices.len() as u32).to_le_bytes());
        w.write_all(&header)?;
        w.write_all(label)?;

        let pad = label.len().next_multiple_of(SIMD_CHUNK_BYTES) - label.len();
        w.write_all(&[0u8; SIMD_CHUNK_BYTES][..pad])?;
        let mut written = (HEADER_BYTES + label.len() + pad) as u64;

        // SAFETY: SimdChunk is repr(C, align(16)) over [u8; 16] with no padding.
        let arena_bytes = unsafe {
            std::slice::from_raw_parts(
                chunks.as_ptr() as *const u8,
                chunks.len() * SIMD_CHUNK_BYTES,
            )
        };
        w.write_all(arena_bytes)?;
        written += arena_bytes.len() as u64;

        // SAFETY: plain u32s; the reader parses them back as little-endian.
        let index_bytes =
            unsafe { std::slice::from_raw_parts(indices.as_ptr() as *const u8, indices.len() * 4) };
        w.write_all(index_bytes)?;
        written += index_bytes.len() as u64;

        let mut buf = Vec::with_capacity(64);
        for dir in &self.dirs {
            buf.clear();
            buf.extend_from_slice(&dir.last_segment_offset().to_le_bytes());
            write_chunked(&mut buf, &dir.path);
            w.write_all(&buf)?;
            written += buf.len() as u64;
        }

        for file in &self.files {
            buf.clear();
            buf.extend_from_slice(&file.size.to_le_bytes());
            buf.extend_from_slice(&file.modified.to_le_bytes());
            buf.push(file.is_binary() as u8);
            buf.extend_from_slice(&file.path.filename_offset.to_le_bytes());
            buf.extend_from_slice(&file.parent_dir_index.to_le_bytes());
            write_chunked(&mut buf, &file.path);
            w.write_all(&buf)?;
            written += buf.len() as u64;
        }

        Ok(written)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        let mut cur = Cursor { bytes, pos: 0 };
        if cur.take(8)? != MAGIC {
            return Err(Error::LayerFormat("bad magic"));
        }

        let version = cur.u32()?;
        if version != FORMAT_VERSION {
            return Err(Error::LayerFormat("unsupported version"));
        }

        let layout = match cur.u8()? {
            0 => SnapshotLayout::BasePartitioned,
            1 => SnapshotLayout::Sorted,
            2 => SnapshotLayout::Unsorted,
            _ => return Err(Error::LayerFormat("bad layout")),
        };
        cur.take(1)?;
        let label_len = cur.u16()? as usize;
        let file_count = cur.u32()? as usize;
        let dir_count = cur.u32()? as usize;
        let chunk_count = cur.u32()? as usize;
        let index_count = cur.u32()? as usize;

        let label = std::str::from_utf8(cur.take(label_len)?)
            .map_err(|_| Error::LayerFormat("label is not utf-8"))?;
        let label = (!label.is_empty()).then(|| Box::from(label));
        cur.take(label_len.next_multiple_of(SIMD_CHUNK_BYTES) - label_len)?;

        let arena_bytes = cur.take(chunk_count * SIMD_CHUNK_BYTES)?;
        let mut arena: Vec<SimdChunk> = Vec::with_capacity(chunk_count);
        // SAFETY: `arena_bytes` holds exactly `chunk_count * 16` bytes and any
        // byte pattern is a valid SimdChunk; the Vec allocation is aligned.
        unsafe {
            std::ptr::copy_nonoverlapping(
                arena_bytes.as_ptr(),
                arena.as_mut_ptr() as *mut u8,
                arena_bytes.len(),
            );
            arena.set_len(chunk_count);
        }

        let indices: Vec<u32> = cur
            .take(index_count * 4)?
            .chunks_exact(4)
            .map(|b| u32::from_le_bytes(b.try_into().expect("4 bytes")))
            .collect();
        if indices.iter().any(|&idx| idx as usize >= chunk_count) {
            return Err(Error::LayerFormat("chunk index out of range"));
        }
        let store = ChunkedPathStore::from_parts(arena, indices);

        // Capacities are bounded by the bytes left, not by the header.
        let mut dirs = Vec::with_capacity(dir_count.min(cur.remaining() / MIN_DIR_RECORD));
        for _ in 0..dir_count {
            let last_segment_offset = cur.u16()?;
            let path = read_chunked(&mut cur, &store, last_segment_offset)?;
            dirs.push(DirItem::new(path, last_segment_offset));
        }

        let mut files = Vec::with_capacity(file_count.min(cur.remaining() / MIN_FILE_RECORD));
        for _ in 0..file_count {
            let size = cur.u64()?;
            let modified = cur.u64()?;
            let binary = cur.u8()? != 0;
            let filename_offset = cur.u16()?;
            let parent_dir_index = cur.u32()?;
            if parent_dir_index as usize >= dir_count {
                return Err(Error::LayerFormat("parent dir out of range"));
            }

            let path = read_chunked(&mut cur, &store, filename_offset)?;
            let mut file = FileItem::new_raw(filename_offset, size, modified, None, binary);
            file.set_path(path);
            file.parent_dir_index = parent_dir_index;
            files.push(file);
        }

        Ok(Self {
            label,
            store: Arc::new(store),
            files,
            dirs,
            layout,
        })
    }
}

struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.pos)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|&end| end <= self.bytes.len())
            .ok_or(Error::LayerFormat("truncated"))?;
        let out = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, Error> {
        Ok(u16::from_le_bytes(
            self.take(2)?.try_into().expect("2 bytes"),
        ))
    }

    fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().expect("4 bytes"),
        ))
    }

    fn u64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().expect("8 bytes"),
        ))
    }
}

// Path record: index_offset u32 into the store's chunk index table, byte_len u16.
fn write_chunked(buf: &mut Vec<u8>, path: &ChunkedString) {
    buf.extend_from_slice(&path.index_offset().to_le_bytes());
    buf.extend_from_slice(&path.byte_len.to_le_bytes());
}

// Every reader slices paths as `str` unchecked, so the bytes are validated
// here once: UTF-8 and `filename_offset` on a char boundary.
fn read_chunked(
    cur: &mut Cursor<'_>,
    store: &ChunkedPathStore,
    filename_offset: u16,
) -> Result<ChunkedString, Error> {
    let index_offset = cur.u32()?;
    let byte_len = cur.u16()?;
    let path = ChunkedString::new(index_offset, byte_len, filename_offset);
    let index_end = index_offset as usize + path.chunk_count();
    if filename_offset > byte_len
        || byte_len as usize >= PATH_BUF_SIZE
        || index_end > store.indices().len()
    {
        return Err(Error::LayerFormat("inconsistent path record"));
    }

    let mut bytes = [0u8; PATH_BUF_SIZE];
    let text = path.read_to_buf(store.as_arena_ptr(), &mut bytes);
    if std::str::from_utf8(text.as_bytes()).is_err() {
        return Err(Error::LayerFormat("path is not utf-8"));
    }
    if !text.is_char_boundary(filename_offset as usize) {
        return Err(Error::LayerFormat("filename offset splits a char"));
    }

    Ok(path)
}

pub(crate) fn normalize_relative_path(raw: &str) -> Option<String> {
    let path = crate::path_utils::to_canonical_slashes(raw);
    let path = path.trim_start_matches("./");
    if path.is_empty()
        || path.starts_with('/')
        || path.ends_with('/')
        || path.len() >= PATH_BUF_SIZE
        || Path::new(path).is_absolute()
    {
        return None;
    }

    Some(path.to_owned())
}

#[inline]
pub(crate) fn filename_offset(path: &str) -> usize {
    path.rfind('/').map(|i| i + 1).unwrap_or(0)
}

pub(crate) fn last_segment_offset(dir: &str) -> u16 {
    let trimmed = dir.trim_end_matches('/');
    trimmed.rfind('/').map(|i| i + 1).unwrap_or(0) as u16
}

// `(dir, name)` ordering shared by the base scan, sealed layers and lookups.
#[inline]
pub(crate) fn dir_name_key(path: &str, filename_offset: u16) -> (&[u8], &[u8]) {
    let (dir, name) = path.as_bytes().split_at(filename_offset as usize);
    (dir, name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(paths: &[&str]) -> LayerSnapshot {
        let mut builder = LayerBuilder::new(Some("test"));
        for (i, p) in paths.iter().enumerate() {
            assert!(
                builder.push(
                    &IndexEntry::new(p).with_metadata(i as u64 * 10, 1_700_000_000 + i as u64)
                )
            );
        }
        builder.finish()
    }

    #[test]
    fn builder_sorts_dedups_and_derives_ancestor_dirs() {
        let snap = snapshot(&[
            "src/b/y.rs",
            "src/a.rs",
            "root.txt",
            "src/b/y.rs",
            "src/b/c/z.rs",
        ]);
        let paths: Vec<String> = snap.relative_paths().collect();
        assert_eq!(
            paths,
            ["root.txt", "src/a.rs", "src/b/y.rs", "src/b/c/z.rs"]
        );

        let dirs: Vec<String> = snap.dir_paths().collect();
        assert_eq!(dirs, ["", "src/", "src/b/", "src/b/c/"]);

        let arena = snap.store.as_arena_ptr();
        for file in &snap.files {
            let dir = snap.dirs[file.parent_dir_index as usize].relative_path(arena);
            assert_eq!(file.dir_str(arena), dir);
        }
    }

    #[test]
    fn builder_rejects_absolute_and_empty_paths() {
        let mut builder = LayerBuilder::new(None);
        assert!(!builder.push(&IndexEntry::new("/etc/passwd")));
        assert!(!builder.push(&IndexEntry::new("")));
        assert!(!builder.push(&IndexEntry::new("dir/")));
        assert!(builder.push(&IndexEntry::new("./ok.rs")));
        assert_eq!(builder.len(), 1);
    }

    #[test]
    fn snapshot_round_trips_through_bytes() {
        let snap = snapshot(&["src/main.rs", "src/lib.rs", "assets/logo.png", "README.md"]);
        let mut bytes = Vec::new();
        let written = snap.write_to(&mut bytes).unwrap();
        assert_eq!(written as usize, bytes.len());

        let back = LayerSnapshot::from_bytes(&bytes).unwrap();
        assert_eq!(back.label(), Some("test"));
        assert_eq!(back.layout(), SnapshotLayout::Sorted);
        assert_eq!(
            back.relative_paths().collect::<Vec<_>>(),
            snap.relative_paths().collect::<Vec<_>>()
        );
        assert_eq!(
            back.dir_paths().collect::<Vec<_>>(),
            snap.dir_paths().collect::<Vec<_>>()
        );
        assert_eq!(back.arena_bytes(), snap.arena_bytes());

        for (a, b) in back.files.iter().zip(&snap.files) {
            assert_eq!(a.size, b.size);
            assert_eq!(a.modified, b.modified);
            assert_eq!(a.is_binary(), b.is_binary());
            assert_eq!(a.parent_dir_index, b.parent_dir_index);
        }
        assert!(
            back.files.iter().any(|f| f.is_binary()),
            "png is binary by extension"
        );
    }

    #[test]
    fn corrupt_bytes_are_rejected_not_panicked() {
        let snap = snapshot(&["a/b.rs", "c.rs"]);
        let mut bytes = Vec::new();
        snap.write_to(&mut bytes).unwrap();

        assert!(LayerSnapshot::from_bytes(&bytes[..bytes.len() - 3]).is_err());
        assert!(LayerSnapshot::from_bytes(b"nope").is_err());

        let mut bad = bytes.clone();
        bad[8] = 99;
        assert!(LayerSnapshot::from_bytes(&bad).is_err());

        // Point the first chunk index past the arena.
        let arena_start = HEADER_BYTES + "test".len().next_multiple_of(SIMD_CHUNK_BYTES);
        let indices_start = arena_start + snap.store.chunks().len() * SIMD_CHUNK_BYTES;
        let mut bad = bytes.clone();
        bad[indices_start..indices_start + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(LayerSnapshot::from_bytes(&bad).is_err());

        // Point a path's index run past the index table.
        let mut bad = bytes.clone();
        let offset_pos = bytes.len() - 6;
        bad[offset_pos..offset_pos + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(LayerSnapshot::from_bytes(&bad).is_err());

        // Huge counts in the header must not allocate before failing.
        let mut bad = bytes.clone();
        bad[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(LayerSnapshot::from_bytes(&bad).is_err());
        let mut bad = bytes.clone();
        bad[24..28].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(LayerSnapshot::from_bytes(&bad).is_err());

        // Arena bytes that are not UTF-8.
        let mut bad = bytes.clone();
        bad[arena_start] = 0xff;
        assert!(LayerSnapshot::from_bytes(&bad).is_err());
    }

    #[test]
    fn filename_offset_inside_a_char_is_rejected() {
        let snap = snapshot(&["é/x.rs"]);
        let mut bytes = Vec::new();
        snap.write_to(&mut bytes).unwrap();

        // The file record ends the buffer; its filename_offset sits right
        // after size + modified + binary.
        let record_len = 8 + 8 + 1 + 2 + 4 + 4 + 2;
        let offset_pos = bytes.len() - record_len + 17;
        assert_eq!(
            u16::from_le_bytes([bytes[offset_pos], bytes[offset_pos + 1]]),
            3
        );
        bytes[offset_pos] = 1;
        assert!(LayerSnapshot::from_bytes(&bytes).is_err());
    }

    #[test]
    fn save_and_load_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("layer.fff");
        let snap = snapshot(&["x/y/z.rs"]);
        let written = snap.save(&path).unwrap();
        assert_eq!(written, std::fs::metadata(&path).unwrap().len());

        let back = LayerSnapshot::load(&path).unwrap();
        assert_eq!(back.relative_paths().collect::<Vec<_>>(), ["x/y/z.rs"]);
        assert!(LayerSnapshot::load(dir.path().join("missing")).is_err());
    }
}
