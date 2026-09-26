use crate::constants::MAX_INDEXABLE_FILE_SIZE;
use ahash::AHashMap;
use rayon::iter::{IndexedParallelIterator, ParallelIterator};
use rayon::slice::ParallelSlice;
use std::cell::UnsafeCell;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering};

use crate::{FileItem, constants};

/// Maximum number of distinct bigrams tracked in the inverted index.
/// 95 printable ASCII chars (32..=126) after lowercasing → ~70 distinct → 4900 possible.
/// We cap at 5000 to cover all printable bigrams with margin.
/// 5000 columns × 62.5KB (500k files) = 305MB. For 50k files: 30MB.
const MAX_BIGRAM_COLUMNS: usize = 5000;

/// Sentinel value: bigram has no allocated column.
const NO_COLUMN: u16 = u16::MAX;

/// 1024 × u64 = 8 KB covers all 65536 possible bigram keys.
const SEEN_WORDS: usize = 1024;

/// Content size where the branchless two-pass `add_long_content` overtakes
/// the single-pass `add_short_content`: ~-35% on 4 KB files, but its fixed
/// flush scan dominates files under ~1 KB. See bigram_bench `bigram_build`.
const LONG_CONTENT_MIN_LEN: usize = 1024;

thread_local! {
    static NORM_BUF: std::cell::RefCell<Vec<u8>> =
        std::cell::RefCell::new(Vec::with_capacity(4096));
}

/// Temporary sync dense builder for the bigram index.
/// Builds from the many threads reading file contents in parallel
pub struct BigramIndexBuilder {
    // we use lookup as atomics only in the builder because it is filled by the rayon threads
    // the actual index uses pure u16 for the allocations
    lookup: Vec<AtomicU16>,
    /// Flat bitset data, materialised on first use.
    col_data: OnceLock<UnsafeCell<Box<[u64]>>>,
    next_column: AtomicU16,
    words: usize,
    file_count: usize,
    populated: AtomicUsize,
}

// SAFETY: `col_data`'s interior mutability is coordinated via disjoint
// `word_idx` ranges (word-aligned file partitioning in the driver), so
// concurrent access is safe despite the `UnsafeCell`. See builder doc.
unsafe impl Sync for BigramIndexBuilder {}

impl BigramIndexBuilder {
    pub fn new(file_count: usize) -> Self {
        let words = file_count.div_ceil(64);
        let mut lookup = Vec::with_capacity(65536);
        lookup.resize_with(65536, || AtomicU16::new(NO_COLUMN));
        Self {
            lookup,
            col_data: OnceLock::new(),
            next_column: AtomicU16::new(0),
            words,
            file_count,
            populated: AtomicUsize::new(0),
        }
    }

    /// Lazily materialise the full `MAX_BIGRAM_COLUMNS * words` bitset
    /// on first access.
    #[inline(always)]
    fn col_data_cell(&self) -> &UnsafeCell<Box<[u64]>> {
        self.col_data.get_or_init(|| {
            let total = MAX_BIGRAM_COLUMNS * self.words;
            UnsafeCell::new(vec![0u64; total].into_boxed_slice())
        })
    }

    /// Raw pointer to the start of the bitset slab. Used for in-place
    /// `|=` writes under the partitioning invariant.
    #[inline(always)]
    fn col_data_ptr(&self) -> *mut u64 {
        unsafe { (*self.col_data_cell().get()).as_mut_ptr() }
    }

    #[inline]
    fn get_or_alloc_column(&self, key: u16) -> u16 {
        let current = self.lookup[key as usize].load(Ordering::Relaxed);
        if current != NO_COLUMN {
            return current;
        }
        let new_col = self.next_column.fetch_add(1, Ordering::Relaxed);
        if new_col >= MAX_BIGRAM_COLUMNS as u16 {
            return NO_COLUMN;
        }

        match self.lookup[key as usize].compare_exchange(
            NO_COLUMN,
            new_col,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => new_col,
            Err(existing) => existing,
        }
    }

    /// Test/bench accessor for a column's raw bitset words. Assumes the
    /// caller has joined all writers (no concurrent mutation).
    #[cfg(test)]
    fn column_bitset(&self, col: u16) -> &[u64] {
        let start = col as usize * self.words;
        let slab = unsafe { &*self.col_data_cell().get() };
        &slab[start..start + self.words]
    }

    #[doc(hidden)] // `pub` (via `#[doc(hidden)]`) only for benchmarking
    pub fn add_file_content(&self, skip_builder: &Self, file_idx: usize, content: &[u8]) {
        if content.len() < 2 {
            return;
        }

        debug_assert!(file_idx < self.file_count);
        let word_idx = file_idx / 64;
        let bit_mask = 1u64 << (file_idx % 64);

        NORM_BUF.with_borrow_mut(|buf| {
            let len = content.len();
            if buf.len() < len {
                buf.resize(len.next_power_of_two().max(4096), 0);
            }

            normalize_bytes(content, &mut buf[..len]);
            let n = &buf[..len];

            // Both paths record the identical bigram set; the split exists
            // purely for speed (see LONG_CONTENT_MIN_LEN).
            if len >= LONG_CONTENT_MIN_LEN {
                self.add_long_content(skip_builder, n, word_idx, bit_mask);
            } else {
                self.add_short_content(skip_builder, n, word_idx, bit_mask);
            }
        });

        self.populated.fetch_add(1, Ordering::Relaxed);
        skip_builder.populated.fetch_add(1, Ordering::Relaxed);
    }

    // Branchless two-pass: set every pair in stack-local bitmaps, including
    // pairs touching the 0 sentinel — flush_seen masks those out. ~-35% vs
    // the single pass on 4 KB files.
    #[inline(always)]
    fn add_long_content(&self, skip_builder: &Self, n: &[u8], word_idx: usize, bit_mask: u64) {
        // Stack-local dedup bitsets: 1024 × u64 = 8 KB each, covers all 65536
        // bigram keys. Has to fit in L1 cache.
        let mut seen_consec = [0u64; SEEN_WORDS];
        let mut seen_skip = [0u64; SEEN_WORDS];

        let mut n0 = n[0];
        let mut n1 = n[1];

        let key = (n0 as usize) << 8 | n1 as usize;
        // SAFETY: key < 65536, so key >> 6 < 1024 = SEEN_WORDS.
        unsafe { *seen_consec.get_unchecked_mut(key >> 6) |= 1u64 << (key & 63) };

        for &cur in &n[2..] {
            let ck = (n1 as usize) << 8 | cur as usize;
            let sk = (n0 as usize) << 8 | cur as usize;
            unsafe {
                *seen_consec.get_unchecked_mut(ck >> 6) |= 1u64 << (ck & 63);
                *seen_skip.get_unchecked_mut(sk >> 6) |= 1u64 << (sk & 63);
            }

            n0 = n1;
            n1 = cur;
        }

        self.flush_seen(&seen_consec, word_idx, bit_mask);
        skip_builder.flush_seen(&seen_skip, word_idx, bit_mask);
    }

    #[inline(always)]
    fn add_short_content(&self, skip_builder: &Self, n: &[u8], word_idx: usize, bit_mask: u64) {
        let mut seen_consec = [0u64; SEEN_WORDS];
        let mut seen_skip = [0u64; SEEN_WORDS];

        let consec_base = self.col_data_ptr();
        let consec_words = self.words;
        let skip_base = skip_builder.col_data_ptr();
        let skip_words = skip_builder.words;

        let mut n0 = n[0];
        let mut n1 = n[1];

        if n0 != 0 && n1 != 0 {
            let key = (n0 as u16) << 8 | n1 as u16;
            self.record_bigram(
                &mut seen_consec,
                key,
                word_idx,
                bit_mask,
                consec_base,
                consec_words,
            );
        }

        for &cur in &n[2..] {
            if cur != 0 {
                if n1 != 0 {
                    let key = (n1 as u16) << 8 | cur as u16;
                    self.record_bigram(
                        &mut seen_consec,
                        key,
                        word_idx,
                        bit_mask,
                        consec_base,
                        consec_words,
                    );
                }
                if n0 != 0 {
                    let key = (n0 as u16) << 8 | cur as u16;
                    skip_builder.record_bigram(
                        &mut seen_skip,
                        key,
                        word_idx,
                        bit_mask,
                        skip_base,
                        skip_words,
                    );
                }
            }
            n0 = n1;
            n1 = cur;
        }
    }

    #[inline(always)]
    fn record_bigram(
        &self,
        seen: &mut [u64; SEEN_WORDS],
        key: u16,
        word_idx: usize,
        bit_mask: u64,
        col_base: *mut u64,
        words: usize,
    ) {
        let k = key as usize;
        let w = k >> 6;
        let bit = 1u64 << (k & 63);
        // SAFETY: w = key/64 with key: u16, so w < 1024 = SEEN_WORDS.
        let prev = unsafe { *seen.get_unchecked(w) };
        if prev & bit == 0 {
            unsafe {
                *seen.get_unchecked_mut(w) = prev | bit;
            }
            let col = self.get_or_alloc_column(key);
            if col != NO_COLUMN {
                unsafe {
                    let p = col_base.add(col as usize * words + word_idx);
                    *p |= bit_mask;
                }
            }
        }
    }

    fn flush_seen(&self, seen: &[u64; SEEN_WORDS], word_idx: usize, bit_mask: u64) {
        let col_base = self.col_data_ptr();
        let words = self.words;
        // SEEN_WORDS is a multiple of 8, so the remainder is always empty.
        for (blk, block) in seen.as_chunks::<8>().0.iter().enumerate() {
            // OR-test whole blocks so the mostly-empty bitmap scans fast.
            if block.iter().fold(0u64, |a, &w| a | w) == 0 {
                continue;
            }
            for (j, &word_bits) in block.iter().enumerate() {
                let w = blk * 8 + j;
                let mut bits = match w & 3 {
                    _ if w < 4 => 0,
                    0 => word_bits & !1,
                    _ => word_bits,
                };
                while bits != 0 {
                    let key = (w << 6 | bits.trailing_zeros() as usize) as u16;
                    bits &= bits - 1;
                    let col = self.get_or_alloc_column(key);
                    if col != NO_COLUMN {
                        unsafe {
                            let p = col_base.add(col as usize * words + word_idx);
                            *p |= bit_mask;
                        }
                    }
                }
            }
        }
    }

    pub fn is_ready(&self) -> bool {
        self.populated.load(Ordering::Relaxed) > 0
    }

    pub fn columns_used(&self) -> u16 {
        self.next_column
            .load(Ordering::Relaxed)
            .min(MAX_BIGRAM_COLUMNS as u16)
    }

    /// Compress the dense builder into a compact `BigramFilter`.
    ///
    /// Retains columns where the bigram appears in ≥`min_density_pct`% (or
    /// the default ~3.1% heuristic when `None`) and <90% of indexed files.
    /// Sparse columns carry too little data to justify their memory;
    /// ubiquitous columns (≥90%) are nearly all-ones and barely filter.
    #[inline(always)]
    pub fn compress(self, min_density_pct: Option<u32>) -> BigramFilter {
        let cols = self.columns_used() as usize;
        let words = self.words;
        let file_count = self.file_count;
        let populated = self.populated.load(Ordering::Relaxed);
        let dense_bytes = words * 8; // cost of one dense column

        let old_lookup = self.lookup;
        // If no file ever populated content, col_data was never
        // materialised. Treat as empty — every column falls through.
        let col_data: Option<Box<[u64]>> = self.col_data.into_inner().map(UnsafeCell::into_inner);

        let mut lookup: Vec<u16> = vec![NO_COLUMN; 65536];
        let mut dense_data: Vec<u64> = Vec::with_capacity(cols * words);
        let mut dense_count: usize = 0;

        if let Some(col_data) = col_data.as_deref() {
            for key in 0..65536usize {
                let old_col = old_lookup[key].load(Ordering::Relaxed);
                if old_col == NO_COLUMN || old_col as usize >= cols {
                    continue;
                }

                let col_start = old_col as usize * words;
                let bitset = &col_data[col_start..col_start + words];

                // count set bits to decide if this column is worth keeping.
                let mut popcount = 0u32;
                for &word in bitset.iter().take(words) {
                    popcount += word.count_ones();
                }

                // drop bigrams appearing in too few files
                let not_to_rare = if let Some(min_pct) = min_density_pct {
                    // Percentage-based: require ≥ min_pct% of populated files.
                    populated > 0 && (popcount as usize) * 100 >= populated * min_pct as usize
                } else {
                    // Default: popcount ≥ words × 2 (~3.1% of files).
                    (popcount as usize * 4) >= dense_bytes
                };

                if !not_to_rare {
                    continue;
                }

                // Drop ubiquitous bigrams — columns ≥90% ones carry almost no
                // filtering power and just waste memory + AND cycles.
                if populated > 0 && (popcount as usize) * 10 >= populated * 9 {
                    continue;
                }

                let dense_idx = dense_count as u16;
                lookup[key] = dense_idx;
                dense_count += 1;

                dense_data.extend_from_slice(bitset);
            }
        }

        BigramFilter {
            lookup,
            dense_data,
            dense_count,
            words,
            file_count,
            populated,
            skip_index: None,
        }
    }
}

unsafe impl Send for BigramIndexBuilder {}

/// Inverted bigram index with optional "skip-1" extension
/// Copmressed into bitset for minimal usage, the layout of this struct actually matters
#[derive(Debug)]
pub struct BigramFilter {
    lookup: Vec<u16>,
    /// Flat buffer of all dense column data laid out at fixed stride `words`.
    /// Column `i` starts at `i * words`.
    dense_data: Vec<u64>, // do not try to change this to u8 it has to be wordsize
    dense_count: usize,
    words: usize,
    file_count: usize,
    populated: usize,
    /// Optional skip-1 bigram index (stride 2). Built from character pairs
    /// at distance 2, e.g. "ABCDE" → (A,C),(B,D),(C,E). ANDead with the
    /// consecutive bigram candidates during query to dramatically reduce
    /// false positives.
    skip_index: Option<Box<BigramFilter>>,
}

/// SIMD-friendly bitwise AND of two equal-length bitsets.
// Auto vectorized (don't touch)
#[inline]
fn bitset_and(result: &mut [u64], bitset: &[u64]) {
    result
        .iter_mut()
        .zip(bitset.iter())
        .for_each(|(r, b)| *r &= *b);
}

impl BigramFilter {
    /// AND the posting lists for all query bigrams (consecutive + skip).
    /// Returns None if no query bigrams are tracked.
    pub fn query(&self, pattern: &[u8]) -> Option<Vec<u64>> {
        if pattern.len() < 2 {
            return None;
        }

        let mut result = vec![u64::MAX; self.words];
        if !self.file_count.is_multiple_of(64) {
            let last = self.words - 1;
            result[last] = (1u64 << (self.file_count % 64)) - 1;
        }

        let words = self.words;
        let mut has_filter = false;

        let mut prev = pattern[0];
        for &b in &pattern[1..] {
            if (32..=126).contains(&prev) && (32..=126).contains(&b) {
                let key = (prev.to_ascii_lowercase() as u16) << 8 | b.to_ascii_lowercase() as u16;
                let col = self.lookup[key as usize];
                if col != NO_COLUMN {
                    let offset = col as usize * words;
                    // SAFETY: compress() guarantees offset + words <= dense_data.len()
                    let slice = unsafe { self.dense_data.get_unchecked(offset..offset + words) };
                    bitset_and(&mut result, slice);
                    has_filter = true;
                }
            }
            prev = b;
        }

        // strid-1 bigrams
        if let Some(skip) = &self.skip_index
            && pattern.len() >= 3
            && let Some(skip_candidates) = skip.query_skip(pattern)
        {
            bitset_and(&mut result, &skip_candidates);
            has_filter = true;
        }

        has_filter.then_some(result)
    }

    /// Query using stride-2 bigrams from the pattern.
    /// For "ABCDE" queries with keys (A,C), (B,D), (C,E).
    fn query_skip(&self, pattern: &[u8]) -> Option<Vec<u64>> {
        let mut result = vec![u64::MAX; self.words];
        if !self.file_count.is_multiple_of(64) {
            let last = self.words - 1;
            result[last] = (1u64 << (self.file_count % 64)) - 1;
        }

        let words = self.words;
        let mut has_filter = false;

        for i in 0..pattern.len().saturating_sub(2) {
            let a = pattern[i];
            let b = pattern[i + 2];
            if (32..=126).contains(&a) && (32..=126).contains(&b) {
                let key = (a.to_ascii_lowercase() as u16) << 8 | b.to_ascii_lowercase() as u16;
                let col = self.lookup[key as usize];
                if col != NO_COLUMN {
                    let offset = col as usize * words;
                    let slice = unsafe { self.dense_data.get_unchecked(offset..offset + words) };
                    bitset_and(&mut result, slice);
                    has_filter = true;
                }
            }
        }

        has_filter.then_some(result)
    }

    /// Attach a skip-1 bigram index for tighter candidate filtering.
    pub fn set_skip_index(&mut self, skip: BigramFilter) {
        self.skip_index = Some(Box::new(skip));
    }

    #[inline]
    pub fn is_candidate(candidates: &[u64], file_idx: usize) -> bool {
        let word = file_idx / 64;
        let bit = file_idx % 64;
        word < candidates.len() && candidates[word] & (1u64 << bit) != 0
    }

    pub fn count_candidates(candidates: &[u64]) -> usize {
        candidates.iter().map(|w| w.count_ones() as usize).sum()
    }

    pub fn is_ready(&self) -> bool {
        self.populated > 0
    }

    pub fn file_count(&self) -> usize {
        self.file_count
    }

    pub fn columns_used(&self) -> usize {
        self.dense_count
    }

    /// Total heap bytes used by this index (lookup + dense data + skip).
    pub fn heap_bytes(&self) -> usize {
        let lookup_bytes = self.lookup.len() * std::mem::size_of::<u16>();
        let dense_bytes = self.dense_data.len() * std::mem::size_of::<u64>();
        let skip_bytes = self.skip_index.as_ref().map_or(0, |s| s.heap_bytes());
        lookup_bytes + dense_bytes + skip_bytes
    }

    /// Check whether a bigram key is present in this index.
    pub fn has_key(&self, key: u16) -> bool {
        self.lookup[key as usize] != NO_COLUMN
    }

    /// Raw lookup table (65536 entries mapping bigram key → column index).
    pub fn lookup(&self) -> &[u16] {
        &self.lookup
    }

    /// Flat dense bitset data at fixed stride `words`.
    pub fn dense_data(&self) -> &[u64] {
        &self.dense_data
    }

    /// Number of u64 words per column (= ceil(file_count / 64)).
    pub fn words(&self) -> usize {
        self.words
    }

    /// Number of dense columns retained after compression.
    pub fn dense_count(&self) -> usize {
        self.dense_count
    }

    /// Number of files that contributed content to the index.
    pub fn populated(&self) -> usize {
        self.populated
    }

    /// Reference to the optional skip-1 bigram sub-index.
    pub fn skip_index(&self) -> Option<&BigramFilter> {
        self.skip_index.as_deref()
    }

    /// Create a new bigram filter from the internal data
    pub fn new(
        lookup: Vec<u16>,
        dense_data: Vec<u64>,
        dense_count: usize,
        words: usize,
        file_count: usize,
        populated: usize,
    ) -> Self {
        Self {
            lookup,
            dense_data,
            dense_count,
            words,
            file_count,
            populated,
            skip_index: None,
        }
    }
}

/// Single-byte normalize: 0 for non-printable, lowercased byte otherwise.
/// 0 is a safe sentinel: lowered printable bytes are 32..=126.
#[inline(always)]
fn normalize_byte_scalar(b: u8) -> u8 {
    let printable = b.wrapping_sub(32) <= 94;
    let lower = b | ((b.wrapping_sub(b'A') < 26) as u8 * 0x20);
    if printable { lower } else { 0 }
}

/// Bulk version: write `dst[i]` = `normalize_byte_scalar(src[i])` for `i`
/// in `0..src.len()`. Inlined-scalar so LLVM auto-vectorises with the
/// build's baseline SIMD; on x86_64 we runtime-dispatch to AVX2.
/// Caller guarantees `dst.len() >= src.len()`.
#[inline(always)]
fn normalize_bytes(src: &[u8], dst: &mut [u8]) {
    debug_assert!(dst.len() >= src.len());
    #[cfg(target_arch = "x86_64")]
    {
        if std::is_x86_feature_detected!("avx2") {
            unsafe { normalize_bytes_avx2(src, dst) };
            return;
        }
    }

    #[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
    {
        unsafe { normalize_bytes_neon(src, dst) };
        return;
    }

    #[allow(unused)]
    normalize_bytes_scalar(src, dst);
}

#[inline(always)]
fn normalize_bytes_scalar(src: &[u8], dst: &mut [u8]) {
    for (i, &b) in src.iter().enumerate() {
        dst[i] = normalize_byte_scalar(b);
    }
}

/// AVX2 normalize: 32 bytes/iter. AVX2 only has signed cmp, so unsigned
/// range checks use `min(max(v, lo), hi) == v`.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn normalize_bytes_avx2(src: &[u8], dst: &mut [u8]) {
    use std::arch::x86_64::*;
    let len = src.len();
    let mut i = 0;
    let p_lo = _mm256_set1_epi8(32);
    let p_hi = _mm256_set1_epi8(126u8 as i8);
    let u_lo = _mm256_set1_epi8(b'A' as i8);
    let u_hi = _mm256_set1_epi8(b'Z' as i8);
    let or20 = _mm256_set1_epi8(0x20);
    while i + 32 <= len {
        unsafe {
            let v = _mm256_loadu_si256(src.as_ptr().add(i) as *const __m256i);
            // printable_mask: v in [32, 126]
            let clamp_p = _mm256_min_epu8(_mm256_max_epu8(v, p_lo), p_hi);
            let printable = _mm256_cmpeq_epi8(v, clamp_p);
            // is_upper_mask: v in [65, 90]
            let clamp_u = _mm256_min_epu8(_mm256_max_epu8(v, u_lo), u_hi);
            let is_upper = _mm256_cmpeq_epi8(v, clamp_u);
            let or_bits = _mm256_and_si256(is_upper, or20);
            let lower = _mm256_or_si256(v, or_bits);
            let out = _mm256_and_si256(lower, printable);
            _mm256_storeu_si256(dst.as_mut_ptr().add(i) as *mut __m256i, out);
        }
        i += 32;
    }
    while i < len {
        dst[i] = normalize_byte_scalar(src[i]);
        i += 1;
    }
}

#[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
#[target_feature(enable = "neon")]
unsafe fn normalize_bytes_neon(src: &[u8], dst: &mut [u8]) {
    use std::arch::aarch64::*;
    let len = src.len();
    let mut i = 0;
    let v32 = vdupq_n_u8(32);
    let v127 = vdupq_n_u8(127);
    let va = vdupq_n_u8(b'A');
    let vz1 = vdupq_n_u8(b'Z' + 1);
    let v20 = vdupq_n_u8(0x20);

    while i + 16 <= len {
        unsafe {
            let v = vld1q_u8(src.as_ptr().add(i));
            // printable: v >= 32 AND v < 127
            let ge32 = vcgeq_u8(v, v32);
            let lt127 = vcltq_u8(v, v127);
            let print_mask = vandq_u8(ge32, lt127);
            // is_upper: v >= 'A' AND v < 'Z'+1
            let ge_a = vcgeq_u8(v, va);
            let lt_z1 = vcltq_u8(v, vz1);
            let upper_mask = vandq_u8(ge_a, lt_z1);
            let or_bits = vandq_u8(upper_mask, v20);
            let lower = vorrq_u8(v, or_bits);
            let out = vandq_u8(lower, print_mask);

            vst1q_u8(dst.as_mut_ptr().add(i), out);
        }
        i += 16;
    }
    while i < len {
        dst[i] = normalize_byte_scalar(src[i]);
        i += 1;
    }
}

pub fn extract_bigrams(content: &[u8]) -> Vec<u16> {
    if content.len() < 2 {
        return Vec::new();
    }
    // Use a flat bitset (65536 bits = 8 KB) for dedup — faster than HashSet.
    let mut seen = vec![0u64; 1024]; // 1024 * 64 = 65536 bits
    let mut bigrams = Vec::new();

    let mut prev = content[0];
    for &b in &content[1..] {
        if (32..=126).contains(&prev) && (32..=126).contains(&b) {
            let key = (prev.to_ascii_lowercase() as u16) << 8 | b.to_ascii_lowercase() as u16;
            let word = key as usize / 64;
            let bit = 1u64 << (key as usize % 64);
            if seen[word] & bit == 0 {
                seen[word] |= bit;
                bigrams.push(key);
            }
        }
        prev = b;
    }
    bigrams
}

/// Modified and added files store their own bigram sets. Deleted files are
/// tombstoned in a bitset so they can be excluded from base query results.
/// This overlay is updated by the background watcher on every file event
/// and cleared when the base index is rebuilt.
#[derive(Debug)]
pub struct BigramOverlay {
    /// Per-file bigram sets for files modified since the base was built.
    /// Key = file index in the base `Vec<FileItem>`.
    modified: AHashMap<usize, Vec<u16>>,

    /// Tombstone bitset — one bit per base file. Set bits are excluded
    /// from base query results.
    tombstones: Vec<u64>,

    /// Original files count this overlay was created for.
    base_file_count: usize,
}

impl BigramOverlay {
    pub(crate) fn new(base_file_count: usize) -> Self {
        let words = base_file_count.div_ceil(64);
        Self {
            modified: AHashMap::new(),
            tombstones: vec![0u64; words],
            base_file_count,
        }
    }

    pub(crate) fn modify_file(&mut self, file_idx: usize, content: &[u8]) {
        self.modified.insert(file_idx, extract_bigrams(content));
    }

    pub(crate) fn delete_file(&mut self, file_idx: usize) {
        if file_idx < self.base_file_count {
            let word = file_idx / 64;
            self.tombstones[word] |= 1u64 << (file_idx % 64);
        }
        self.modified.remove(&file_idx);
    }

    /// Return base file indices of modified files whose bigrams match ALL
    /// of the given `pattern_bigrams`.
    pub(crate) fn query_modified(&self, pattern_bigrams: &[u16]) -> Vec<usize> {
        if pattern_bigrams.is_empty() {
            return self.modified.keys().copied().collect();
        }
        self.modified
            .iter()
            .filter_map(|(&file_idx, bigrams)| {
                pattern_bigrams
                    .iter()
                    .all(|pb| bigrams.contains(pb))
                    .then_some(file_idx)
            })
            .collect()
    }

    /// Number of base files this overlay was created for.
    pub(crate) fn base_file_count(&self) -> usize {
        self.base_file_count
    }

    /// Get the tombstone bitset for clearing base candidates.
    pub(crate) fn tombstones(&self) -> &[u64] {
        &self.tombstones
    }

    /// Get all modified file indices (for conservative overlay merging when
    /// we can't extract precise bigrams, e.g. regex patterns).
    pub(crate) fn modified_indices(&self) -> Vec<usize> {
        self.modified.keys().copied().collect()
    }
}

const BIGRAM_CHUNK_FILES: usize = 4 * 64;

/// Sparse-column cutoff for the skip-1 sub-index. Rare skip columns add
/// little filtering power but ~25-30% of index memory, so we drop
/// anything appearing in < 12 % of populated files.
pub(crate) const SKIP_INDEX_MIN_DENSITY_PCT: u32 = 12;

thread_local! {
    /// Reusable read buffer that is allocated per thread and used for reading files
    static READ_BUF: std::cell::RefCell<Box<[u8]>> =
        std::cell::RefCell::new(vec![0u8; MAX_INDEXABLE_FILE_SIZE].into_boxed_slice());
}

/// Reads bigram chunk, we *SHOULD NOT* use mmap cache here because bigram is built off-lock
/// if the watcher thread tries to invalidate mmap during the borrow from it - UAB or segfaut
///
/// mmap should only be used by the locked version of grep which absolutely minimizes any riscs
#[inline]
fn read_bigram_chunk<'a>(
    file: &FileItem,
    base_fd: libc::c_int,
    base_path: &std::path::Path,
    arena: crate::simd_path::ArenaPtr,
    buf: &'a mut [u8],
    path_buf: &mut [u8; crate::simd_path::PATH_BUF_SIZE],
) -> Option<&'a [u8]> {
    let want = (file.size as usize).min(MAX_INDEXABLE_FILE_SIZE);
    let filled = file.read_trimmed_into_buf(base_fd, base_path, arena, path_buf, &mut buf[..want]);
    if filled == 0 {
        return None;
    }

    let data = &buf[..filled];

    Some(data)
}

#[tracing::instrument(skip_all, name = "Building Bigram Index", level = tracing::Level::DEBUG)]
pub(crate) fn build_bigram_index(
    files: &[crate::types::FileItem],
    base_path: &std::path::Path,
    arena: crate::simd_path::ArenaPtr,
) -> BigramFilter {
    let builder = BigramIndexBuilder::new(files.len());
    let skip_builder = BigramIndexBuilder::new(files.len());

    #[cfg(unix)]
    let base_fd: libc::c_int = open_base_dir_fd(base_path);
    #[cfg(not(unix))]
    let base_fd: i32 = -1;

    // Always reads each file into the thread-local READ_BUF — never aliases the
    // persistent mmap cache. See `read_bigram_chunk` for the rationale: this
    // pass runs detached on the background pool without holding the picker
    // read lock, so a watcher event mutating a `FileItem` would race any
    // borrow we took from a cached `Mmap`.
    crate::parallelism::BACKGROUND_THREAD_POOL.install(|| {
        files
            .par_chunks(BIGRAM_CHUNK_FILES)
            .enumerate()
            .for_each(|(chunk_idx, chunk)| {
                let base_idx = chunk_idx * BIGRAM_CHUNK_FILES;
                for (offset, file) in chunk.iter().enumerate() {
                    let file_idx = base_idx + offset;

                    if file.is_binary() || file.size == 0 {
                        continue;
                    }

                    READ_BUF.with(|read_cell| {
                        let mut buf = read_cell.borrow_mut();
                        let mut path_buf = [0u8; crate::simd_path::PATH_BUF_SIZE];

                        if let Some(content) = read_bigram_chunk(
                            file,
                            base_fd,
                            base_path,
                            arena,
                            &mut buf[..],
                            &mut path_buf,
                        ) {
                            // we have to manually ensure that every byte is a valid text byte to
                            // perform this we have to scan every file, first 512 bytes is not enough
                            // so basically we rely on the fact that first 2MB will always contain
                            // an invalid text sequence if this is not a binary file.
                            //
                            // Need to find a better way to do this.
                            file.set_binary(crate::types::detect_binary_content(content));

                            builder.add_file_content(&skip_builder, file_idx, content);
                        }
                    });
                }
            });
    });

    #[cfg(unix)]
    if base_fd >= 0 {
        unsafe { libc::close(base_fd) };
    }

    let mut index = builder.compress(None);
    let skip_index = skip_builder.compress(Some(SKIP_INDEX_MIN_DENSITY_PCT));
    index.set_skip_index(skip_index);

    // in progress bigram walk + rust's ignore crate allocates shit ton of garbage memory
    // all custom allocators would think this is available resource while we do not allocate
    // after the sync, so it's very important to let the unused memory go back to the OS
    crate::file_picker::hint_allocator_collect();

    index
}

#[tracing::instrument(skip_all, name = "Sniffing Large Files Binary", level = tracing::Level::DEBUG)]
pub(crate) fn sniff_binary_for_non_indexable(
    files: &[FileItem],
    base_path: &std::path::Path,
    arena: crate::simd_path::ArenaPtr,
    cancelled: &std::sync::atomic::AtomicBool,
) {
    // Non-indexable files are few in a typical repo, so a serial pass with a
    // single reused chunk buffer beats spinning up the thread pool.
    let mut path_buf = [0u8; crate::simd_path::PATH_BUF_SIZE];
    let mut chunk = vec![0u8; crate::types::BINARY_CLASSIFICATION_CHUNK_SIZE];
    use std::sync::atomic::Ordering;

    for (i, file) in files.iter().enumerate() {
        // check every 256 files to avoid useless work
        if (i & 0xFF) == 0 && cancelled.load(Ordering::Acquire) {
            return;
        }

        // check only the files that we are able to grep
        if file.size == 0 || file.size > constants::MAX_FFFILE_SIZE {
            continue;
        }

        let abs = file.write_absolute_path(arena, base_path, &mut path_buf);
        file.detect_binary_per_byte(abs, &mut chunk);
    }
}

/// Open the base directory for the `openat` fast path. Returns `-1` on
/// failure — callers interpret a negative fd as "fall back to absolute
/// paths".
#[cfg(unix)]
fn open_base_dir_fd(base_path: &std::path::Path) -> libc::c_int {
    use std::os::unix::ffi::OsStrExt;
    let mut cstr = [0u8; crate::simd_path::PATH_BUF_SIZE];
    let bytes = base_path.as_os_str().as_bytes();
    if bytes.len() >= cstr.len() {
        return -1;
    }
    cstr[..bytes.len()].copy_from_slice(bytes);
    // SAFETY: `cstr` is NUL-terminated by construction (zero-initialised,
    // and we only filled up to `bytes.len() < cstr.len()`).
    unsafe {
        libc::open(
            cstr.as_ptr() as *const std::os::raw::c_char,
            libc::O_RDONLY | libc::O_DIRECTORY,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a key the same way `add_file_content` does: two printable-ASCII
    /// bytes, lowercased, packed as `(hi << 8) | lo`.
    fn key(a: u8, b: u8) -> u16 {
        ((a.to_ascii_lowercase() as u16) << 8) | b.to_ascii_lowercase() as u16
    }

    /// Return the sorted list of (consec, skip) bigram keys that should appear
    /// for `content`. Used as the reference implementation.
    fn expected_bigrams(content: &[u8]) -> (Vec<u16>, Vec<u16>) {
        let mut consec: std::collections::BTreeSet<u16> = Default::default();
        let mut skip: std::collections::BTreeSet<u16> = Default::default();
        let printable = |b: u8| (32..=126).contains(&b);
        for i in 1..content.len() {
            let a = content[i - 1];
            let b = content[i];
            if printable(a) && printable(b) {
                consec.insert(key(a, b));
            }
            if i >= 2 {
                let a = content[i - 2];
                let b = content[i];
                if printable(a) && printable(b) {
                    skip.insert(key(a, b));
                }
            }
        }
        (consec.into_iter().collect(), skip.into_iter().collect())
    }

    /// Query: does the builder record file 0 as having this bigram set?
    fn builder_has_key_for_file_0(b: &BigramIndexBuilder, k: u16) -> bool {
        let col = b.lookup[k as usize].load(Ordering::Relaxed);
        if col == NO_COLUMN {
            return false;
        }
        b.column_bitset(col)[0] & 1 != 0
    }

    fn run_and_compare(content: &[u8]) {
        let consec = BigramIndexBuilder::new(1);
        let skip = BigramIndexBuilder::new(1);
        consec.add_file_content(&skip, 0, content);

        let (expected_consec, expected_skip) = expected_bigrams(content);

        // Every expected bigram must be recorded.
        for k in &expected_consec {
            assert!(
                builder_has_key_for_file_0(&consec, *k),
                "consec bigram 0x{k:04x} missing for content {content:?}",
            );
        }
        for k in &expected_skip {
            assert!(
                builder_has_key_for_file_0(&skip, *k),
                "skip bigram 0x{k:04x} missing for content {content:?}",
            );
        }

        // No unexpected bigrams — iterate lookup for set columns.
        for k in 0u32..=0xFFFF {
            let recorded_consec = builder_has_key_for_file_0(&consec, k as u16);
            let recorded_skip = builder_has_key_for_file_0(&skip, k as u16);
            if recorded_consec {
                assert!(
                    expected_consec.contains(&(k as u16)),
                    "unexpected consec bigram 0x{k:04x} in content {content:?}",
                );
            }
            if recorded_skip {
                assert!(
                    expected_skip.contains(&(k as u16)),
                    "unexpected skip bigram 0x{k:04x} in content {content:?}",
                );
            }
        }
    }

    #[test]
    fn add_file_empty_is_noop() {
        let consec = BigramIndexBuilder::new(1);
        let skip = BigramIndexBuilder::new(1);
        consec.add_file_content(&skip, 0, b"");
        assert_eq!(consec.columns_used(), 0);
        assert_eq!(skip.columns_used(), 0);
        // populated counter not incremented for empty input
        assert_eq!(consec.populated.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn add_file_single_byte_is_noop() {
        let consec = BigramIndexBuilder::new(1);
        let skip = BigramIndexBuilder::new(1);
        consec.add_file_content(&skip, 0, b"a");
        assert_eq!(consec.columns_used(), 0);
        assert_eq!(skip.columns_used(), 0);
    }

    #[test]
    fn add_file_two_bytes_consec_only() {
        // With exactly 2 bytes there's no skip bigram (needs i >= 2 in the loop).
        run_and_compare(b"ab");
    }

    #[test]
    fn add_file_three_bytes_has_skip() {
        // "abc" -> consec {"ab", "bc"}, skip {"ac"}
        run_and_compare(b"abc");
    }

    #[test]
    fn add_file_ascii_words() {
        run_and_compare(b"hello world");
        run_and_compare(b"the quick brown fox jumps over the lazy dog");
        run_and_compare(b"fn main() { println!(\"hi\"); }");
    }

    #[test]
    fn add_file_case_is_lowered() {
        // Uppercase should be lowercased before keying, so "AB" == "ab".
        let upper = BigramIndexBuilder::new(1);
        let upper_skip = BigramIndexBuilder::new(1);
        upper.add_file_content(&upper_skip, 0, b"ABC");

        let lower = BigramIndexBuilder::new(1);
        let lower_skip = BigramIndexBuilder::new(1);
        lower.add_file_content(&lower_skip, 0, b"abc");

        // Both should have identical bigram keys.
        for k in 0u32..=0xFFFF {
            let u = builder_has_key_for_file_0(&upper, k as u16);
            let l = builder_has_key_for_file_0(&lower, k as u16);
            assert_eq!(u, l, "consec 0x{k:04x}: upper={u} lower={l}");
            let u = builder_has_key_for_file_0(&upper_skip, k as u16);
            let l = builder_has_key_for_file_0(&lower_skip, k as u16);
            assert_eq!(u, l, "skip 0x{k:04x}: upper={u} lower={l}");
        }
    }

    #[test]
    fn add_file_rejects_non_printable() {
        // Bigrams where either byte is outside 32..=126 are rejected. But
        // the skip-1 bigram can still connect two printable bytes across a
        // non-printable one: for "\0a\0b", consec sees no valid pair but
        // skip sees (a,b) at i=3. Use the reference implementation.
        run_and_compare(b"\0a\0b");

        // All-zero input: truly nothing recorded.
        let consec = BigramIndexBuilder::new(1);
        let skip = BigramIndexBuilder::new(1);
        consec.add_file_content(&skip, 0, b"\0\0\0\0");
        assert_eq!(consec.columns_used(), 0);
        assert_eq!(skip.columns_used(), 0);
    }

    #[test]
    fn add_file_mixed_printable_and_control() {
        // "a\tb\nc d" — \t (9) and \n (10) are below 32. Consec:
        //   (a, \t) x, (\t, b) x, (b, \n) x, (\n, c) x, (c, ' ') ok, (' ', d) ok
        // Skip (i-2, i):
        //   (a, b) ok, (\t, \n) x, (b, c) ok, (\n, ' ') x, (c, d) ok
        run_and_compare(b"a\tb\nc d");
    }

    #[test]
    fn add_file_repeats_are_deduped() {
        // "ababab" has many repeats of "ab", "ba" — each unique bigram should
        // be recorded exactly once (the stack-local `seen_*` dedup works).
        run_and_compare(b"ababababab");
    }

    #[test]
    fn add_file_tombstone_separation() {
        // Two separate files share no bits; file 1's content doesn't bleed
        // into file 0's row and vice-versa.
        let consec = BigramIndexBuilder::new(2);
        let skip = BigramIndexBuilder::new(2);
        consec.add_file_content(&skip, 0, b"xy");
        consec.add_file_content(&skip, 1, b"zw");

        let key_xy = key(b'x', b'y');
        let key_zw = key(b'z', b'w');

        // file 0 has "xy" but not "zw"
        let col_xy = consec.lookup[key_xy as usize].load(Ordering::Relaxed);
        let col_zw = consec.lookup[key_zw as usize].load(Ordering::Relaxed);
        let bitset_xy = consec.column_bitset(col_xy)[0];
        let bitset_zw = consec.column_bitset(col_zw)[0];
        assert_eq!(bitset_xy & 0b01, 0b01, "file 0 should have xy");
        assert_eq!(bitset_zw & 0b01, 0, "file 0 should NOT have zw");
        assert_eq!(bitset_xy & 0b10, 0, "file 1 should NOT have xy");
        assert_eq!(bitset_zw & 0b10, 0b10, "file 1 should have zw");
    }

    #[test]
    fn add_file_long_content() {
        // Stress test: ~8 KB of printable ASCII. Should complete without
        // overflowing any stack-local bitset and produce the full set.
        let mut buf = Vec::with_capacity(8192);
        for i in 0..8192 {
            buf.push(32u8 + ((i * 7) % 95) as u8); // cycle through printable range
        }
        run_and_compare(&buf);
    }

    #[test]
    fn add_file_simd_and_scalar_agree() {
        // Cross-check: both code paths (scalar <128 bytes, SIMD ≥128) must
        // produce identical bigram sets for content that straddles the
        // threshold. Mix printable ASCII with some non-printable bytes and
        // repeats so the non-printable branch in the SIMD path exercises.
        let mut mixed = Vec::with_capacity(256);
        for i in 0..256usize {
            mixed.push(match i % 9 {
                0 => 0,     // NUL
                1 => 0x7F,  // DEL (just above 126)
                2 => b'\n', // below 32
                _ => 32 + ((i * 13) % 95) as u8,
            });
        }

        run_and_compare(&mixed[..127]); // scalar path
        run_and_compare(&mixed); // SIMD path (256 bytes)
        run_and_compare(&mixed[..192]); // SIMD path with scalar tail
    }

    #[test]
    fn add_file_long_short_paths_agree() {
        // Same mixed content checked just below, at, and above
        // LONG_CONTENT_MIN_LEN so both add_short_content and add_long_content
        // are validated against the reference implementation.
        let mut mixed = Vec::with_capacity(LONG_CONTENT_MIN_LEN * 2);
        for i in 0..LONG_CONTENT_MIN_LEN * 2 {
            mixed.push(match i % 11 {
                0 => 0,
                1 => 0x7F,
                2 => b'\n',
                _ => 32 + ((i * 31) % 95) as u8,
            });
        }
        run_and_compare(&mixed[..LONG_CONTENT_MIN_LEN - 1]);
        run_and_compare(&mixed[..LONG_CONTENT_MIN_LEN]);
        run_and_compare(&mixed);
    }

    #[test]
    fn add_file_respects_file_count_boundary() {
        // file_count=100, file_idx=63 (last bit in word 0) and file_idx=64
        // (first bit in word 1). Make sure the word_idx math is right.
        let consec = BigramIndexBuilder::new(100);
        let skip = BigramIndexBuilder::new(100);
        consec.add_file_content(&skip, 63, b"ab");
        consec.add_file_content(&skip, 64, b"cd");

        let kab = key(b'a', b'b');
        let kcd = key(b'c', b'd');
        let col_ab = consec.lookup[kab as usize].load(Ordering::Relaxed);
        let col_cd = consec.lookup[kcd as usize].load(Ordering::Relaxed);

        let ab_bitset = consec.column_bitset(col_ab);
        let cd_bitset = consec.column_bitset(col_cd);
        // ab in word 0, bit 63
        assert_eq!(ab_bitset[0], 1u64 << 63);
        assert_eq!(ab_bitset[1], 0);
        // cd in word 1, bit 0
        assert_eq!(cd_bitset[0], 0);
        assert_eq!(cd_bitset[1], 1);
    }

    /// `build_bigram_index` skips binary and empty files. A skipped file must
    /// not take the rest of its `BIGRAM_CHUNK_FILES` chunk down with it.
    #[test]
    fn a_skipped_file_does_not_drop_the_rest_of_its_chunk() {
        let tmp = tempfile::TempDir::new().unwrap();
        let base = tmp.path();

        // Index 0 is empty, so the guard fires on the very first file of the
        // chunk; the nine text files behind it must still be indexed.
        let mut names: Vec<String> = vec!["a_empty.txt".to_string()];
        std::fs::write(base.join("a_empty.txt"), "").unwrap();
        for i in 0..9 {
            let name = format!("f{i}.txt");
            let body = if i < 2 {
                "the unicorn line\n"
            } else {
                "plain filler content\n"
            };
            std::fs::write(base.join(&name), body).unwrap();
            names.push(name);
        }

        let mut files: Vec<FileItem> = names
            .iter()
            .map(|name| {
                let size = std::fs::metadata(base.join(name)).unwrap().len();
                FileItem::new_raw(0, size, 0, None, false)
            })
            .collect();
        let (store, strings) =
            crate::simd_path::build_chunked_path_store_from_strings(&names, &files);
        for (file, path) in files.iter_mut().zip(strings) {
            file.set_path(path);
        }

        let index = build_bigram_index(&files, base, store.as_arena_ptr());
        let candidates = index
            .query(b"unicorn")
            .expect("the text files must be in the index");

        assert!(
            BigramFilter::is_candidate(&candidates, 1),
            "f0.txt is behind the skipped file and must still be a candidate"
        );
        assert!(BigramFilter::is_candidate(&candidates, 2));
        // Files without the needle stay filtered out.
        assert!(!BigramFilter::is_candidate(&candidates, 5));
    }
}
