use std::io::Read;
use std::path::{Path, PathBuf};
#[cfg(not(target_os = "windows"))]
use std::sync::OnceLock;
use std::sync::atomic::{AtomicI32, AtomicU8, AtomicU64, AtomicUsize, Ordering};

#[cfg(not(target_os = "windows"))]
use crate::constants::{FRESH_MMAP_THRESHOLD, MMAP_THRESHOLD};
use crate::constants::{MAX_CACHED_CONTENT_BYTES, MAX_FFFILE_SIZE, PATH_BUF_SIZE};
use crate::index::constraints::Constrainable;
use crate::query_tracker::QueryMatchEntry;
use crate::simd_path::ArenaPtr;
use fff_query_parser::{FFFQuery, FuzzyQuery, Location};

/// Different sources of the string storage used by FFF
/// implements as a deduplicated 16-bytes alined heap
/// can be stored in RAM or on disk
pub trait FFFStringStorage {
    /// Resolve the arena for a [`FileItem`] (handles base vs overflow split).
    fn arena_for(&self, file: &FileItem) -> ArenaPtr;

    /// The base arena (scan-time paths).
    fn base_arena(&self) -> ArenaPtr;
    /// The overflow arena (paths added after the last full scan).
    fn overflow_arena(&self) -> ArenaPtr;
}

impl FFFStringStorage for ArenaPtr {
    #[inline]
    fn arena_for(&self, _file: &FileItem) -> ArenaPtr {
        *self
    }

    #[inline]
    fn base_arena(&self) -> ArenaPtr {
        *self
    }

    #[inline]
    fn overflow_arena(&self) -> ArenaPtr {
        *self
    }
}

pub trait FileSliceExt {
    fn live_count(&self) -> usize;
}

impl FileSliceExt for [FileItem] {
    #[inline]
    fn live_count(&self) -> usize {
        self.iter().filter(|f| !f.is_deleted()).count()
    }
}

pub struct FileItemFlags;

impl FileItemFlags {
    pub const BINARY: u8 = 1 << 0;
    /// Tombstone — file was deleted but index slot is preserved so
    /// bigram indices for other files stay valid.
    pub const DELETED: u8 = 1 << 1;
    /// File was added after the last full reindex; its indices point
    /// into the overflow builder arena, not the base arena.
    pub const OVERFLOW: u8 = 1 << 2;
}

pub struct DirFlags;

impl DirFlags {
    pub const OVERFLOW: u8 = 1 << 0;
    pub const DELETED: u8 = 1 << 1;
}

/// A directory in the file index. Shares chunk arena with file paths.
#[derive(Debug)]
pub struct DirItem {
    flags: u8,
    pub(crate) path: crate::simd_path::ChunkedString,
    /// Byte offset where the last path segment begins (e.g. for `src/components/`
    /// this is 4, pointing to `components/`). Used for dirname-bonus scoring.
    last_segment_offset: u16,
    /// Maximum `access_frecency_score` among direct child files.
    /// Atomic so parallel frecency updates can write directly without juggling.
    max_access_frecency: AtomicI32,
}

impl Clone for DirItem {
    fn clone(&self) -> Self {
        Self {
            flags: self.flags,
            path: self.path.clone(),
            last_segment_offset: self.last_segment_offset,
            max_access_frecency: AtomicI32::new(self.max_access_frecency()),
        }
    }
}

impl DirItem {
    #[inline(always)]
    pub fn is_overflow(&self) -> bool {
        self.flags & DirFlags::OVERFLOW != 0
    }

    #[inline(always)]
    pub fn is_deleted(&self) -> bool {
        self.flags & DirFlags::DELETED != 0
    }

    /// Marks the dir deleted/restored. Returns `true` when the state changed.
    pub(crate) fn set_deleted(&mut self, deleted: bool) -> bool {
        if self.is_deleted() == deleted {
            return false;
        }
        if deleted {
            self.flags |= DirFlags::DELETED;
        } else {
            self.flags &= !DirFlags::DELETED;
        }
        true
    }

    pub(crate) fn new(path: crate::simd_path::ChunkedString, last_segment_offset: u16) -> Self {
        Self {
            path,
            flags: 0,
            last_segment_offset,
            max_access_frecency: AtomicI32::new(0),
        }
    }

    /// A dir appended after the initial scan; its path lives in the overflow arena.
    pub(crate) fn new_overflow(
        path: crate::simd_path::ChunkedString,
        last_segment_offset: u16,
    ) -> Self {
        Self {
            path,
            flags: DirFlags::OVERFLOW,
            last_segment_offset,
            max_access_frecency: AtomicI32::new(0),
        }
    }

    /// Byte offset of the last path segment within the directory path.
    #[inline]
    pub fn last_segment_offset(&self) -> u16 {
        self.last_segment_offset
    }

    /// Current max access frecency score.
    #[inline]
    pub fn max_access_frecency(&self) -> i32 {
        self.max_access_frecency.load(Ordering::Relaxed)
    }

    /// Atomically update the directory's frecency score if the given score is larger.
    /// Safe to call from parallel threads.
    #[inline]
    pub fn update_frecency_if_larger(&self, score: i32) {
        self.max_access_frecency.fetch_max(score, Ordering::Relaxed);
    }

    /// Reset frecency to zero (used before full recomputation).
    #[inline]
    pub fn reset_frecency(&self) {
        self.max_access_frecency.store(0, Ordering::Relaxed);
    }

    pub(crate) fn read_relative_path<'a>(&self, arena: ArenaPtr, buf: &'a mut [u8]) -> &'a str {
        self.path.read_to_buf(arena, buf)
    }

    /// Relative dir path as owned String (cold path).
    pub fn relative_path(&self, arena: impl FFFStringStorage) -> String {
        let mut out = String::new();
        let ptr = if self.is_overflow() {
            arena.overflow_arena()
        } else {
            arena.base_arena()
        };

        self.path.write_to_string(ptr, &mut out);
        out
    }

    /// Write the last segment (dirname) of this directory path to `out`.
    pub fn write_dir_name(&self, arena: ArenaPtr, out: &mut String) {
        out.clear();
        let total = self.path.byte_len as usize;
        let offset = self.last_segment_offset as usize;
        if offset >= total {
            return;
        }
        // Read the full path, then slice from last_segment_offset
        let mut buf = [0u8; PATH_BUF_SIZE];
        let full = self.path.read_to_buf(arena, &mut buf);
        out.push_str(&full[offset..]);
    }

    /// The dirname (last segment) as an owned String. Cold path.
    pub fn dir_name(&self, arena: impl FFFStringStorage) -> String {
        let mut out = String::new();
        let ptr = if self.is_overflow() {
            arena.overflow_arena()
        } else {
            arena.base_arena()
        };
        self.write_dir_name(ptr, &mut out);
        out
    }

    /// A path = base_path + "/" + relative. Cold path, allocates.
    pub fn absolute_path(&self, arena: impl FFFStringStorage, base_path: &Path) -> PathBuf {
        let rel = self.relative_path(arena);
        if rel.is_empty() {
            base_path.to_path_buf()
        } else {
            base_path.join(&rel)
        }
    }
}

impl Constrainable for DirItem {
    #[inline]
    fn write_file_name(&self, arena: ArenaPtr, out: &mut String) {
        // For dirs, the "file name" equivalent is the last path segment
        self.write_dir_name(arena, out);
    }

    #[inline]
    fn write_relative_path(&self, arena: ArenaPtr, out: &mut String) {
        self.path.write_to_string(arena, out);
    }

    #[inline]
    fn git_status(&self) -> Option<git2::Status> {
        None
    }

    #[inline]
    fn is_overflow(&self) -> bool {
        DirItem::is_overflow(self)
    }
}

#[derive(Debug)]
pub struct FileItem {
    pub size: u64,
    pub modified: u64,
    pub access_frecency_score: i16,
    pub modification_frecency_score: i16,
    pub git_recency_score: i16,
    pub git_status: Option<git2::Status>,
    pub(crate) path: crate::simd_path::ChunkedString,
    pub(crate) parent_dir_index: u32,
    flags: AtomicU8,
    /// Lazy mmap cache. Only populated by the actual file read, controlled by the budget.
    #[cfg(not(target_os = "windows"))]
    content: OnceLock<memmap2::Mmap>,
}

impl Clone for FileItem {
    fn clone(&self) -> Self {
        Self {
            path: self.path.clone(),
            parent_dir_index: self.parent_dir_index,
            size: self.size,
            modified: self.modified,
            access_frecency_score: self.access_frecency_score,
            modification_frecency_score: self.modification_frecency_score,
            git_recency_score: self.git_recency_score,
            git_status: self.git_status,
            flags: AtomicU8::new(self.flags.load(Ordering::Relaxed)),
            // on clone we have to reset the content lock
            #[cfg(not(target_os = "windows"))]
            content: OnceLock::new(),
        }
    }
}

/// Single-block read used by the binary classifier. Most binaries reveal a
/// NUL byte within the first filesystem block, so 16 KB lets one read settle
/// the classification for typical files while keeping the scratch buffer
/// small enough to live on the stack.
pub const BINARY_CLASSIFICATION_CHUNK_SIZE: usize = 16 * 1024;

/// A file is treated as binary if any NUL byte appears in the scanned prefix.
#[inline]
pub(crate) fn detect_binary_content(content: &[u8]) -> bool {
    memchr::memchr(0, content).is_some()
}

impl FileItem {
    pub fn new_raw(
        filename_start: u16,
        size: u64,
        modified: u64,
        git_status: Option<git2::Status>,
        is_binary: bool,
    ) -> Self {
        let mut flags = 0u8;
        if is_binary {
            flags |= FileItemFlags::BINARY;
        }

        let mut path = crate::simd_path::ChunkedString::empty();
        path.filename_offset = filename_start;

        Self {
            path,
            parent_dir_index: u32::MAX,
            size,
            modified,
            access_frecency_score: 0,
            modification_frecency_score: 0,
            git_recency_score: 0,
            git_status,
            flags: AtomicU8::new(flags),
            #[cfg(not(target_os = "windows"))]
            content: OnceLock::new(),
        }
    }

    /// Returns an absolute path of the file
    pub fn absolute_path(&self, arena: impl FFFStringStorage, base_path: &Path) -> PathBuf {
        let mut buf = [0u8; PATH_BUF_SIZE];
        let rel = self.path.read_to_buf(arena.arena_for(self), &mut buf);
        base_path.join(rel)
    }

    pub(crate) fn set_path(&mut self, path: crate::simd_path::ChunkedString) {
        self.path = path;
    }

    pub fn dir_str(&self, arena: impl FFFStringStorage) -> String {
        let mut s = String::with_capacity(64);
        self.path.write_dir_to(arena.arena_for(self), &mut s);
        s
    }

    pub(crate) fn write_dir_str(&self, arena: ArenaPtr, out: &mut String) {
        self.path.write_dir_to(arena, out);
    }

    pub fn file_name(&self, arena: impl FFFStringStorage) -> String {
        let mut s = String::with_capacity(32);
        self.path.write_filename_to(arena.arena_for(self), &mut s);
        s
    }

    pub(crate) fn write_file_name_from_arena(&self, arena: ArenaPtr, out: &mut String) {
        self.path.write_filename_to(arena, out);
    }

    pub fn relative_path(&self, arena: impl FFFStringStorage) -> String {
        let mut s = String::with_capacity(64);
        self.path.write_to_string(arena.arena_for(self), &mut s);
        s
    }

    pub(crate) fn write_relative_path_from_arena(&self, arena: ArenaPtr, out: &mut String) {
        self.path.write_to_string(arena, out);
    }

    pub fn relative_path_len(&self) -> usize {
        self.path.byte_len as usize
    }

    pub fn filename_offset_in_relative_path(&self) -> usize {
        self.path.filename_offset as usize
    }

    pub(crate) fn relative_path_eq(&self, arena: ArenaPtr, other: &str) -> bool {
        if other.len() != self.path.byte_len as usize {
            return false;
        }
        let mut buf = [0u8; 512];
        let mine = self.path.read_to_buf(arena, &mut buf);
        mine == other
    }

    pub(crate) fn relative_path_starts_with(&self, arena: ArenaPtr, prefix: &str) -> bool {
        let mut buf = [0u8; PATH_BUF_SIZE];
        let path = self.path.read_to_buf(arena, &mut buf);
        path.starts_with(prefix)
    }

    /// Write `base_path + '/' + relative_path` into `buf` and return it
    /// as `&Path`. Takes a fixed-size array so the buffer can live on
    /// the stack (no heap allocation, no bounds checks in the hot loop).
    pub(crate) fn write_absolute_path<'a>(
        &self,
        arena: ArenaPtr,
        base_path: &Path,
        buf: &'a mut [u8; PATH_BUF_SIZE],
    ) -> &'a Path {
        let base = base_path.as_os_str().as_encoded_bytes();
        let base_len = base.len();
        buf[..base_len].copy_from_slice(base);
        let sep_len = if base_len > 0 && base[base_len - 1] != std::path::MAIN_SEPARATOR as u8 {
            buf[base_len] = std::path::MAIN_SEPARATOR as u8;
            1
        } else {
            0
        };

        let base_end_idx = base_len + sep_len;
        let relative_portion_str = self.path.read_to_buf(arena, &mut buf[base_end_idx..]);
        let rel_len = relative_portion_str.len();
        let total = base_end_idx + rel_len;
        // Stored relative paths are '/'-canonical; rewrite to the OS-native
        // separator so the result matches git-cache keys, the frecency DB, and
        // Win32 file APIs. No-op off Windows.
        crate::path_utils::nativize_slashes_in_place(&mut buf[base_end_idx..total]);
        Path::new(unsafe { std::str::from_utf8_unchecked(&buf[..total]) })
    }

    /// Write the relative path into `buf` and NUL-terminate, returning
    /// a `&CStr`. Fixed-size array so the buffer is stack-allocatable.
    ///
    /// Paired with a parent-directory fd this eliminates the per-file
    /// absolute-path memcpy: `openat(dir_fd, cstr.as_ptr(), O_RDONLY)`
    /// resolves the name relative to `dir_fd`. Unix-only.
    #[cfg(unix)]
    pub(crate) fn write_relative_cstr<'a>(
        &self,
        arena: ArenaPtr,
        buf: &'a mut [u8; PATH_BUF_SIZE],
    ) -> &'a std::ffi::CStr {
        // Reserve the last byte for the NUL terminator.
        let rel = self.path.read_to_buf(arena, &mut buf[..PATH_BUF_SIZE - 1]);
        let n = rel.len();
        buf[n] = 0;
        // SAFETY: `buf[..=n]` ends with the NUL we just wrote and
        // filesystem paths never contain interior NULs.
        unsafe { std::ffi::CStr::from_bytes_with_nul_unchecked(&buf[..=n]) }
    }

    #[inline]
    pub fn total_frecency_score(&self) -> i32 {
        self.access_frecency_score as i32 + self.modification_frecency_score as i32
    }

    #[allow(dead_code)]
    #[inline]
    pub(crate) fn is_likely_hot(&self) -> bool {
        self.access_frecency_score > 0 || self.git_status.is_some()
    }

    /// Reads a fixed bytes count from the file optimized for quick speed of opening
    #[inline]
    pub(crate) fn read_trimmed_into_buf(
        &self,
        base_fd: i32,
        base_path: &Path,
        arena: ArenaPtr,
        path_buf: &mut [u8; PATH_BUF_SIZE],
        buf: &mut [u8],
    ) -> usize {
        #[cfg(unix)]
        {
            self.read_into_buf_unix(base_fd, base_path, arena, path_buf, buf)
        }
        #[cfg(not(unix))]
        {
            let _ = base_fd;
            self.read_into_buf_std(base_path, arena, path_buf, buf)
        }
    }

    #[cfg(unix)]
    fn read_into_buf_unix(
        &self,
        base_fd: libc::c_int,
        base_path: &Path,
        arena: ArenaPtr,
        path_buf: &mut [u8; PATH_BUF_SIZE],
        buf: &mut [u8],
    ) -> usize {
        let fd = if base_fd >= 0 {
            let relative_path = self.write_relative_cstr(arena, path_buf);
            // SAFETY: `relative_path` is NUL-terminated, `base_fd` is a
            // valid directory descriptor owned by the caller.
            unsafe { libc::openat(base_fd, relative_path.as_ptr(), libc::O_RDONLY) }
        } else {
            use std::os::unix::io::IntoRawFd;
            let abs = self.write_absolute_path(arena, base_path, path_buf);
            match std::fs::File::open(abs) {
                Ok(f) => f.into_raw_fd(),
                Err(e) => {
                    tracing::error!(?e, "Failed to fopen file");
                    return 0;
                }
            }
        };
        if fd < 0 {
            return 0;
        }

        let mut filled = 0usize;
        while filled < buf.len() {
            // SAFETY: `fd` is an owned descriptor, `buf[filled..]` is a
            // valid writable slice for `buf.len() - filled` bytes.
            let n = unsafe {
                libc::read(
                    fd,
                    buf[filled..].as_mut_ptr() as *mut libc::c_void,
                    (buf.len() - filled) as libc::size_t,
                )
            };
            if n <= 0 {
                break;
            }
            filled += n as usize;
        }

        // SAFETY: matching close for the owned descriptor.
        unsafe { libc::close(fd) };
        filled
    }

    #[cfg(not(unix))]
    fn read_into_buf_std(
        &self,
        base_path: &Path,
        arena: ArenaPtr,
        path_buf: &mut [u8; PATH_BUF_SIZE],
        buf: &mut [u8],
    ) -> usize {
        let abs = self.write_absolute_path(arena, base_path, path_buf);
        let Ok(mut f) = std::fs::File::open(abs) else {
            return 0;
        };
        let mut filled = 0usize;
        while filled < buf.len() {
            match f.read(&mut buf[filled..]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(_) => return 0,
            }
        }
        filled
    }

    #[inline]
    pub fn is_binary(&self) -> bool {
        self.flags.load(Ordering::Relaxed) & FileItemFlags::BINARY != 0
    }

    #[inline]
    pub fn set_binary(&self, val: bool) {
        if val {
            self.flags
                .fetch_or(FileItemFlags::BINARY, Ordering::Relaxed);
        } else {
            self.flags
                .fetch_and(!FileItemFlags::BINARY, Ordering::Relaxed);
        }
    }

    /// Chunked classifier of the binary content of the file chunk by chunk
    /// accepts path which to reuse the allocated buffer for absolute path read
    pub(crate) fn detect_binary_per_byte(&self, path: &Path, chunk: &mut [u8]) {
        if self.size == 0 {
            return;
        }

        let Ok(mut file) = std::fs::OpenOptions::new()
            .write(false)
            .read(true)
            .open(path)
        else {
            tracing::error!(path = ?path.display(), "Failed to open indexed file");
            return;
        };

        loop {
            match file.read(chunk) {
                Ok(0) => break,
                Err(e) => {
                    tracing::error!(?e, "Failed to read file chunk");
                    break;
                }
                Ok(n) => {
                    if detect_binary_content(&chunk[..n]) {
                        self.set_binary(true);
                    }
                }
            }
        }
    }

    #[inline]
    pub fn is_deleted(&self) -> bool {
        self.flags.load(Ordering::Relaxed) & FileItemFlags::DELETED != 0
    }

    #[inline]
    #[doc(hidden)]
    /// Don't use it, use FilePicker::delete_file
    pub fn set_deleted(&self, val: bool) {
        if val {
            self.flags
                .fetch_or(FileItemFlags::DELETED, Ordering::Relaxed);
        } else {
            self.flags
                .fetch_and(!FileItemFlags::DELETED, Ordering::Relaxed);
        }
    }

    #[inline]
    pub fn is_overflow(&self) -> bool {
        self.flags.load(Ordering::Relaxed) & FileItemFlags::OVERFLOW != 0
    }

    #[inline]
    pub fn set_overflow(&self, val: bool) {
        if val {
            self.flags
                .fetch_or(FileItemFlags::OVERFLOW, Ordering::Relaxed);
        } else {
            self.flags
                .fetch_and(!FileItemFlags::OVERFLOW, Ordering::Relaxed);
        }
    }
}

impl FileItem {
    /// Invalidate the cached mmap content, has to be called every time the file is updated.
    ///
    /// Call this when the background watcher detects that the file has been modified.
    /// On Unix, a file that is truncated while mapped can cause SIGBUS. On Windows,
    /// the stale buffer simply won't reflect the new contents. In both cases,
    /// invalidating ensures a fresh read on the next access.
    #[cfg(not(target_os = "windows"))]
    pub fn invalidate_mmap(&mut self, budget: &ContentCacheBudget) {
        if self.content.get().is_some() {
            budget.cached_count.fetch_sub(1, Ordering::Relaxed);
            budget.cached_bytes.fetch_sub(self.size, Ordering::Relaxed);
        }

        self.content = OnceLock::new();
    }

    #[cfg(target_os = "windows")]
    pub fn invalidate_mmap(&mut self, _: &ContentCacheBudget) {}

    pub fn update_metadata(
        &mut self,
        budget: &ContentCacheBudget,
        modified_secs: Option<u64>,
        new_size: Option<u64>,
    ) {
        if let Some(modified) = modified_secs
            && self.modified < modified
        {
            self.modified = modified;
        }

        self.invalidate_mmap(budget);

        if let Some(size) = new_size {
            self.size = size;
        }
    }

    /// Get the cached file contents or lazily load and cache them.
    ///
    /// Returns `None` if the file is too large, empty, can't be opened, **or
    /// the cache budget is exhausted**. Callers that need content regardless
    /// of the budget should use [`get_content_for_search`].
    ///
    /// After the first call, this is lock-free (just an atomic load + pointer deref).
    ///
    /// On Windows we never back this cache — `memmap2` would require a full
    /// `std::fs::read` heap copy and the OS page cache already absorbs repeat
    /// reopens. Returning `None` keeps callers on the scratch-read slow path
    /// and avoids duplicating every indexed file on the heap.
    #[cfg(target_os = "windows")]
    pub(crate) fn get_cached_content(
        &self,
        _arena: ArenaPtr,
        _base_path: &Path,
        _budget: &ContentCacheBudget,
    ) -> Option<&[u8]> {
        None
    }

    /// Returns a reference to a cached mmap of the file's contents.
    ///
    /// SAFETY-CRITICAL: callers must hold the picker read lock for as long as the returned slice is in use.
    #[cfg(not(target_os = "windows"))]
    pub(crate) fn get_cached_content(
        &self,
        arena: ArenaPtr,
        base_path: &Path,
        budget: &ContentCacheBudget,
    ) -> Option<&[u8]> {
        if let Some(content) = self.content.get() {
            return Some(content);
        }

        if self.size < MMAP_THRESHOLD || self.size > budget.max_file_size {
            return None;
        }

        // Check cache budget before creating a new persistent cache entry.
        let count = budget.cached_count.load(Ordering::Relaxed);
        let bytes = budget.cached_bytes.load(Ordering::Relaxed);
        let max_files = budget.max_files;
        let max_bytes = budget.max_bytes;
        if count >= max_files || bytes + self.size > max_bytes {
            return None;
        }

        let path = self.absolute_path(arena, base_path);
        let file = std::fs::File::open(&path).ok()?;
        // SAFETY: the mmap is backed by the kernel page cache and reflects
        // file updates; the only risk is SIGBUS on a concurrent truncate,
        // which the watcher mitigates by invalidating on modification.
        let mmap = unsafe { memmap2::Mmap::map(&file) }.ok()?;
        let result = self.content.get_or_init(|| mmap);

        budget.cached_count.fetch_add(1, Ordering::Relaxed);
        budget.cached_bytes.fetch_add(self.size, Ordering::Relaxed);

        Some(result)
    }

    /// Get file content for searching — **always returns content** for eligible
    /// files, even when the persistent cache budget is exhausted.
    ///
    /// The caller provides a reusable `path_buf` (pre-filled with `base_path/`)
    /// and its `base_len` to avoid allocations when constructing the absolute path.
    #[inline]
    pub(crate) fn get_content_for_search<'a>(
        &'a self,
        buf: &'a mut Vec<u8>,
        #[cfg_attr(target_os = "windows", allow(unused_variables))] mmap_slot: &'a mut MmapSlot,
        arena: ArenaPtr,
        base_path: &Path,
        budget: &ContentCacheBudget,
    ) -> Option<&'a [u8]> {
        #[cfg(not(target_os = "windows"))]
        {
            // Fast path: persistent cache hit (zero-copy). Safe here because
            // grep callers hold the picker read lock for the lifetime of the
            // returned slice — see [`Self::get_cached_content`] safety note.
            if let Some(cached) = self.get_cached_content(arena, base_path, budget) {
                return Some(cached);
            }
        }

        let max_file_size = budget.max_file_size;
        if self.is_binary() || self.size == 0 || self.size > max_file_size {
            return None;
        }

        let abs = self.absolute_path(arena, base_path);

        #[cfg(not(target_os = "windows"))]
        if self.size >= FRESH_MMAP_THRESHOLD {
            let file = std::fs::File::open(&abs).ok()?;
            let mmap = unsafe { memmap2::Mmap::map(&file) }.ok()?;
            let stored = mmap_slot.insert(mmap);
            return Some(&stored[..]);
        } else {
            let _ = (mmap_slot, arena);
        }

        let len = self.size as usize;
        buf.resize(len, 0);

        let mut file = std::fs::File::open(&abs).ok()?;
        file.read_exact(buf).ok()?;
        Some(buf.as_slice())
    }
}

/// Per-thread scratch slot owning a transient mmap returned from
/// [`FileItem::get_content_for_search`]. `Option<Mmap>` on Unix,
/// unit on Windows where mmap is unused.
#[cfg(not(target_os = "windows"))]
pub type MmapSlot = Option<memmap2::Mmap>;
#[cfg(target_os = "windows")]
pub type MmapSlot = ();

impl Constrainable for FileItem {
    #[inline]
    fn write_file_name(&self, arena: ArenaPtr, out: &mut String) {
        self.path.write_filename_to(arena, out);
    }

    #[inline]
    fn write_relative_path(&self, arena: ArenaPtr, out: &mut String) {
        self.path.write_to_string(arena, out);
    }

    #[inline]
    fn git_status(&self) -> Option<git2::Status> {
        self.git_status
    }

    #[inline]
    fn is_overflow(&self) -> bool {
        FileItem::is_overflow(self)
    }
}

#[derive(Debug, Clone, Default)]
pub struct Score {
    pub total: i32,
    pub base_score: i32,
    pub filename_bonus: i32,
    pub special_filename_bonus: i32,
    pub frecency_boost: i32,
    pub git_status_boost: i32,
    pub git_recency_boost: i32,
    pub distance_penalty: i32,
    pub current_file_penalty: i32,
    pub combo_match_boost: i32,
    pub path_alignment_bonus: i32,
    pub exact_match: bool,
    pub match_type: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct PaginationArgs {
    pub offset: usize,
    pub limit: usize,
}

impl Default for PaginationArgs {
    fn default() -> Self {
        Self {
            offset: 0,
            limit: 100,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ScoringContext<'a> {
    pub query: &'a FFFQuery<'a>,
    pub project_path: Option<&'a Path>,
    pub current_file: Option<&'a str>,
    pub max_typos: u16,
    pub max_threads: usize,
    pub last_same_query_match: Option<QueryMatchEntry>,
    pub combo_boost_score_multiplier: i32,
    pub min_combo_count: u32,
    pub pagination: PaginationArgs,
}

impl ScoringContext<'_> {
    pub fn effective_query(&self) -> &str {
        match &self.query.fuzzy_query {
            FuzzyQuery::Text(t) => t,
            FuzzyQuery::Parts(parts) if !parts.is_empty() => parts[0],
            _ => self.query.raw_query.trim(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SearchResult<'a> {
    pub items: Vec<&'a FileItem>,
    pub scores: Vec<Score>,
    pub match_byte_offsets: Vec<smallvec::SmallVec<[(u32, u32); 4]>>,
    pub total_matched: usize,
    pub total_files: usize,
    pub location: Option<Location>,
}

/// Search result for directory-only fuzzy search.
#[derive(Debug, Clone, Default)]
pub struct DirSearchResult<'a> {
    pub items: Vec<&'a DirItem>,
    pub scores: Vec<Score>,
    pub total_matched: usize,
    pub total_dirs: usize,
}

/// A single item in a mixed (files + directories) search result.
#[derive(Debug, Clone)]
pub enum MixedItemRef<'a> {
    File(&'a FileItem),
    Dir(&'a DirItem),
}

/// Search result for mixed (files + directories) fuzzy search.
/// Items are interleaved by total score in descending order.
#[derive(Debug, Clone, Default)]
pub struct MixedSearchResult<'a> {
    pub items: Vec<MixedItemRef<'a>>,
    pub scores: Vec<Score>,
    pub total_matched: usize,
    pub total_files: usize,
    pub total_dirs: usize,
    pub location: Option<Location>,
}

impl Default for MixedItemRef<'_> {
    fn default() -> Self {
        // Should never be used, exists only for Default derive on MixedSearchResult
        unreachable!("MixedItemRef::default should not be called")
    }
}

#[derive(Debug)]
pub struct ContentCacheBudget {
    pub max_files: usize,
    pub max_bytes: u64,
    pub max_file_size: u64,
    pub cached_count: AtomicUsize,
    pub cached_bytes: AtomicU64,
}

impl ContentCacheBudget {
    pub fn unlimited() -> Self {
        Self {
            max_files: usize::MAX,
            max_bytes: u64::MAX,
            max_file_size: MAX_FFFILE_SIZE,
            cached_count: AtomicUsize::new(0),
            cached_bytes: AtomicU64::new(0),
        }
    }

    pub fn zero() -> Self {
        Self {
            max_files: 0,
            max_bytes: 0,
            max_file_size: 0,
            cached_count: AtomicUsize::new(0),
            cached_bytes: AtomicU64::new(0),
        }
    }

    // Byte budget
    pub fn is_exhausted(&self) -> bool {
        self.cached_count.load(Ordering::Relaxed) >= self.max_files
            || self.cached_bytes.load(Ordering::Relaxed) >= self.max_bytes
    }

    pub fn new_for_repo(file_count: usize) -> Self {
        let max_files = if file_count > 50_000 {
            5_000
        } else if file_count > 10_000 {
            10_000
        } else {
            30_000 // effectively unlimited for small repos
        };

        let max_bytes = if file_count > 50_000 {
            128 * 1024 * 1024 // 128 MB
        } else if file_count > 10_000 {
            256 * 1024 * 1024 // 256 MB
        } else {
            MAX_CACHED_CONTENT_BYTES // 512 MB
        };

        Self {
            max_files,
            max_bytes,
            max_file_size: MAX_FFFILE_SIZE,
            cached_count: AtomicUsize::new(0),
            cached_bytes: AtomicU64::new(0),
        }
    }

    /// Build a budget from caller-supplied overrides.
    ///
    /// Each argument is a cap; `0` means "use the library default for that
    /// cap" (inherits from [`Self::default`], which is `new_for_repo(30_000)`).
    /// Returns `None` when every cap is `0`, signalling to the picker that it
    /// should auto-size the budget from the final scanned file count rather
    /// than applying an explicit override.
    pub fn from_overrides(max_files: usize, max_bytes: u64, max_file_size: u64) -> Option<Self> {
        if max_files == 0 && max_bytes == 0 && max_file_size == 0 {
            return None;
        }

        let mut budget = Self::default();
        if max_files > 0 {
            budget.max_files = max_files;
        }
        if max_bytes > 0 {
            budget.max_bytes = max_bytes;
        }
        if max_file_size > 0 {
            budget.max_file_size = max_file_size;
        }
        Some(budget)
    }

    pub fn reset(&self) {
        self.cached_count.store(0, Ordering::Relaxed);
        self.cached_bytes.store(0, Ordering::Relaxed);
    }
}

impl Default for ContentCacheBudget {
    fn default() -> Self {
        Self::new_for_repo(30_000)
    }
}
