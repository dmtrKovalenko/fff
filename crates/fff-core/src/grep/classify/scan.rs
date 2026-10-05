//! Byte-class scanning 16 bytes at a time.
//!
//! Two classes matter for the classifier: blanks (space, tab) and identifier
//! bytes (`[A-Za-z0-9_$]` plus every non-ASCII byte, so UTF-8 identifiers stay
//! whole). SSE2 and NEON are baseline on x86_64 and aarch64, so there is no
//! runtime feature detection; other targets use the scalar mask.
//!
//! Tokens are compared as packed little-endian `u128` words: every keyword the
//! grammars know fits in 16 bytes, and identifiers never contain a zero byte,
//! so a masked 16-byte load is a unique key for any token up to 16 bytes.

const LANES: usize = 16;

/// First index at or after `from` that is not a space or tab.
#[inline]
pub(super) fn skip_blank(s: &[u8], from: usize) -> usize {
    scan(s, from, blank_mask)
}

/// First index at or after `from` that is not an identifier byte.
#[inline]
pub(super) fn ident_end(s: &[u8], from: usize) -> usize {
    scan(s, from, ident_mask)
}

#[inline]
pub(super) const fn is_blank(b: u8) -> bool {
    b == b' ' || b == b'\t'
}

#[inline]
pub(super) const fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$' || b >= 0x80
}

/// Packs `s[start..end]` into a little-endian `u128`; 0 when the token is
/// empty or longer than 16 bytes (0 is never a keyword).
#[inline]
pub(super) fn word(s: &[u8], start: usize, end: usize) -> u128 {
    let len = end - start;
    if len == 0 || len > LANES {
        return 0;
    }
    let v = u128::from_le_bytes(load(s, start));
    if len == LANES {
        v
    } else {
        v & ((1u128 << (len * 8)) - 1)
    }
}

/// Compile-time counterpart of [`word`] for keyword constants.
pub(super) const fn kw(b: &[u8]) -> u128 {
    assert!(!b.is_empty() && b.len() <= LANES);
    let mut v = 0u128;
    let mut i = 0;
    while i < b.len() {
        v |= (b[i] as u128) << (i * 8);
        i += 1;
    }
    v
}

/// 16 bytes starting at `i`, zero padded past the end of `s`. Zero is neither
/// blank nor an identifier byte, so padding always stops a scan.
#[inline(always)]
fn load(s: &[u8], i: usize) -> [u8; LANES] {
    match s.get(i..i + LANES) {
        Some(chunk) => chunk.try_into().unwrap(),
        None => {
            let mut buf = [0u8; LANES];
            let tail = &s[i.min(s.len())..];
            buf[..tail.len()].copy_from_slice(tail);
            buf
        }
    }
}

#[inline(always)]
fn scan(s: &[u8], mut i: usize, mask: impl Fn(&[u8; LANES]) -> u32) -> usize {
    while i < s.len() {
        // bit n set = byte n is in the class; bit 16 of the inverse is always set
        let stop = (!mask(&load(s, i))).trailing_zeros() as usize;
        if stop < LANES {
            return (i + stop).min(s.len());
        }
        i += LANES;
    }
    s.len()
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
fn blank_mask(c: &[u8; LANES]) -> u32 {
    use std::arch::x86_64::*;
    // SAFETY: SSE2 is part of the x86_64 baseline and `c` is 16 readable bytes.
    unsafe {
        let v = _mm_loadu_si128(c.as_ptr().cast());
        let space = _mm_cmpeq_epi8(v, _mm_set1_epi8(b' ' as i8));
        let tab = _mm_cmpeq_epi8(v, _mm_set1_epi8(b'\t' as i8));
        _mm_movemask_epi8(_mm_or_si128(space, tab)) as u32
    }
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
fn ident_mask(c: &[u8; LANES]) -> u32 {
    use std::arch::x86_64::*;
    // SAFETY: SSE2 is part of the x86_64 baseline and `c` is 16 readable bytes.
    unsafe {
        let v = _mm_loadu_si128(c.as_ptr().cast());
        // Signed compares: bytes >= 0x80 are negative, so the ASCII ranges
        // below never include them; they are added back as `non_ascii`.
        let lower = _mm_or_si128(v, _mm_set1_epi8(0x20));
        let alpha = _mm_and_si128(
            _mm_cmpgt_epi8(lower, _mm_set1_epi8(b'a' as i8 - 1)),
            _mm_cmplt_epi8(lower, _mm_set1_epi8(b'z' as i8 + 1)),
        );
        let digit = _mm_and_si128(
            _mm_cmpgt_epi8(v, _mm_set1_epi8(b'0' as i8 - 1)),
            _mm_cmplt_epi8(v, _mm_set1_epi8(b'9' as i8 + 1)),
        );
        let under = _mm_cmpeq_epi8(v, _mm_set1_epi8(b'_' as i8));
        let dollar = _mm_cmpeq_epi8(v, _mm_set1_epi8(b'$' as i8));
        let non_ascii = _mm_cmplt_epi8(v, _mm_setzero_si128());
        let m = _mm_or_si128(
            _mm_or_si128(alpha, digit),
            _mm_or_si128(_mm_or_si128(under, dollar), non_ascii),
        );
        _mm_movemask_epi8(m) as u32
    }
}

#[cfg(target_arch = "aarch64")]
#[inline(always)]
fn blank_mask(c: &[u8; LANES]) -> u32 {
    use std::arch::aarch64::*;
    // SAFETY: NEON is part of the aarch64 baseline and `c` is 16 readable bytes.
    unsafe {
        let v = vld1q_u8(c.as_ptr());
        let m = vorrq_u8(
            vceqq_u8(v, vdupq_n_u8(b' ')),
            vceqq_u8(v, vdupq_n_u8(b'\t')),
        );
        neon_movemask(m)
    }
}

#[cfg(target_arch = "aarch64")]
#[inline(always)]
fn ident_mask(c: &[u8; LANES]) -> u32 {
    use std::arch::aarch64::*;
    // SAFETY: NEON is part of the aarch64 baseline and `c` is 16 readable bytes.
    unsafe {
        let v = vld1q_u8(c.as_ptr());
        let lower = vorrq_u8(v, vdupq_n_u8(0x20));
        let alpha = vandq_u8(
            vcgeq_u8(lower, vdupq_n_u8(b'a')),
            vcleq_u8(lower, vdupq_n_u8(b'z')),
        );
        let digit = vandq_u8(vcgeq_u8(v, vdupq_n_u8(b'0')), vcleq_u8(v, vdupq_n_u8(b'9')));
        let under = vceqq_u8(v, vdupq_n_u8(b'_'));
        let dollar = vceqq_u8(v, vdupq_n_u8(b'$'));
        let non_ascii = vcgeq_u8(v, vdupq_n_u8(0x80));
        let m = vorrq_u8(
            vorrq_u8(alpha, digit),
            vorrq_u8(vorrq_u8(under, dollar), non_ascii),
        );
        neon_movemask(m)
    }
}

/// x86 `movemask` for NEON: one bit per 0x00/0xFF lane.
#[cfg(target_arch = "aarch64")]
#[inline(always)]
unsafe fn neon_movemask(m: std::arch::aarch64::uint8x16_t) -> u32 {
    use std::arch::aarch64::*;
    const WEIGHTS: [u8; 16] = [1, 2, 4, 8, 16, 32, 64, 128, 1, 2, 4, 8, 16, 32, 64, 128];
    // SAFETY: the caller is in a NEON context; WEIGHTS is 16 readable bytes.
    unsafe {
        let bits = vandq_u8(m, vld1q_u8(WEIGHTS.as_ptr()));
        let lo = vaddv_u8(vget_low_u8(bits)) as u32;
        let hi = vaddv_u8(vget_high_u8(bits)) as u32;
        lo | (hi << 8)
    }
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
#[inline(always)]
fn blank_mask(c: &[u8; LANES]) -> u32 {
    scalar_mask(c, is_blank)
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
#[inline(always)]
fn ident_mask(c: &[u8; LANES]) -> u32 {
    scalar_mask(c, is_ident)
}

#[cfg_attr(any(target_arch = "x86_64", target_arch = "aarch64"), allow(dead_code))]
#[inline(always)]
fn scalar_mask(c: &[u8; LANES], class: fn(u8) -> bool) -> u32 {
    c.iter()
        .enumerate()
        .fold(0, |m, (i, &b)| m | ((class(b) as u32) << i))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn masks_match_scalar_for_every_byte() {
        for b in 0..=255u8 {
            let chunk = [b; LANES];
            let expect_blank = if is_blank(b) { 0xFFFF } else { 0 };
            let expect_ident = if is_ident(b) { 0xFFFF } else { 0 };
            assert_eq!(blank_mask(&chunk), expect_blank, "blank {b:#x}");
            assert_eq!(ident_mask(&chunk), expect_ident, "ident {b:#x}");
        }
    }

    #[test]
    fn words_match_keyword_constants() {
        let s = b"  macro_rules! foo";
        let end = ident_end(s, 2);
        assert_eq!(end, 13);
        assert_eq!(word(s, 2, end), kw(b"macro_rules"));
        assert_eq!(word(b"abcdefghijklmnopq", 0, 17), 0);
        assert_eq!(word(b"abcdefghijklmnop", 0, 16), kw(b"abcdefghijklmnop"));
    }

    proptest! {
        #[test]
        fn scans_match_scalar(s in proptest::collection::vec(any::<u8>(), 0..80), from in 0usize..80) {
            let from = from.min(s.len());
            let blank = s[from..].iter().position(|&b| !is_blank(b)).map_or(s.len(), |p| from + p);
            let ident = s[from..].iter().position(|&b| !is_ident(b)).map_or(s.len(), |p| from + p);
            prop_assert_eq!(skip_blank(&s, from), blank);
            prop_assert_eq!(ident_end(&s, from), ident);
        }

        #[test]
        fn long_runs_cross_chunks(n in 0usize..70, tail in any::<u8>()) {
            let mut s = vec![b' '; n];
            s.push(tail);
            let blank = if is_blank(tail) { n + 1 } else { n };
            prop_assert_eq!(skip_blank(&s, 0), blank);
            let mut id = vec![b'a'; n];
            id.push(tail);
            let ident = if is_ident(tail) { n + 1 } else { n };
            prop_assert_eq!(ident_end(&id, 0), ident);
        }
    }
}
