const IDENTIFIER_BYTES: [bool; 256] = {
    let mut table = [false; 256];
    let mut index = 0;
    while index < table.len() {
        let byte = index as u8;
        table[index] = byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$' || byte >= 128;
        index += 1;
    }
    table
};

#[cfg(all(
    target_arch = "aarch64",
    target_feature = "neon",
    target_endian = "little"
))]
const MASK_BITS: usize = 4;
#[cfg(target_arch = "x86_64")]
const MASK_BITS: usize = 1;

#[inline(always)]
pub(super) fn identifier_end(line: &[u8], mut pos: usize) -> usize {
    // Short keywords finish before loading the SIMD tables.
    for _ in 0..3 {
        if pos >= line.len() || !IDENTIFIER_BYTES[line[pos] as usize] {
            return pos;
        }
        pos += 1;
    }
    #[cfg(any(
        target_arch = "x86_64",
        all(
            target_arch = "aarch64",
            target_feature = "neon",
            target_endian = "little"
        )
    ))]
    {
        while line.len() - pos >= 16 {
            // SAFETY: the length check guarantees 16 readable bytes.
            let mask = unsafe { identifier_mask(line.as_ptr().add(pos)) };
            let count = (!mask).trailing_zeros() as usize / MASK_BITS;
            pos += count;
            if count < 16 {
                return pos;
            }
        }
        if line.len() >= 16 && pos < line.len() {
            let base = line.len() - 16;
            // SAFETY: this overlapping load stays inside the slice; ignore bytes before pos.
            let mask =
                unsafe { identifier_mask(line.as_ptr().add(base)) } >> ((pos - base) * MASK_BITS);
            return pos + (!mask).trailing_zeros() as usize / MASK_BITS;
        }
    }
    while pos < line.len() && IDENTIFIER_BYTES[line[pos] as usize] {
        pos += 1;
    }
    pos
}

#[cfg(all(
    target_arch = "aarch64",
    target_feature = "neon",
    target_endian = "little"
))]
#[inline(always)]
unsafe fn identifier_mask(ptr: *const u8) -> u64 {
    use std::arch::aarch64::*;
    // Bits select ASCII high nibbles; bit zero admits every non-ASCII byte.
    const LOW: [u8; 16] = {
        let mut table = [1; 16];
        let mut byte = 0;
        while byte < 128 {
            if IDENTIFIER_BYTES[byte] {
                table[byte & 15] |= 1 << (byte >> 4);
            }
            byte += 1;
        }
        table
    };
    const HIGH: [u8; 16] = [0, 0, 4, 8, 16, 32, 64, 128, 1, 1, 1, 1, 1, 1, 1, 1];
    // SAFETY: NEON is enabled for this target; the caller supplies 16 readable bytes.
    unsafe {
        let bytes = vld1q_u8(ptr);
        let low = vqtbl1q_u8(vld1q_u8(LOW.as_ptr()), vandq_u8(bytes, vdupq_n_u8(15)));
        let high = vqtbl1q_u8(vld1q_u8(HIGH.as_ptr()), vshrq_n_u8::<4>(bytes));
        let valid = vtstq_u8(low, high);
        // Four bits per input byte preserve the first non-identifier position.
        vget_lane_u64::<0>(vreinterpret_u64_u8(vshrn_n_u16::<4>(vreinterpretq_u16_u8(
            valid,
        ))))
    }
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn identifier_mask(ptr: *const u8) -> u64 {
    use std::arch::x86_64::*;
    // SAFETY: SSE2 is baseline on x86_64; the caller supplies 16 readable bytes.
    unsafe {
        let bytes = _mm_loadu_si128(ptr.cast());
        let lower = _mm_or_si128(bytes, _mm_set1_epi8(0x20));
        let letters = _mm_and_si128(
            _mm_cmpgt_epi8(lower, _mm_set1_epi8(b'a' as i8 - 1)),
            _mm_cmplt_epi8(lower, _mm_set1_epi8(b'z' as i8 + 1)),
        );
        let digits = _mm_and_si128(
            _mm_cmpgt_epi8(bytes, _mm_set1_epi8(b'0' as i8 - 1)),
            _mm_cmplt_epi8(bytes, _mm_set1_epi8(b'9' as i8 + 1)),
        );
        let symbols = _mm_or_si128(
            _mm_cmpeq_epi8(bytes, _mm_set1_epi8(b'_' as i8)),
            _mm_cmpeq_epi8(bytes, _mm_set1_epi8(b'$' as i8)),
        );
        let non_ascii = _mm_cmplt_epi8(bytes, _mm_setzero_si128());
        let valid = _mm_or_si128(
            _mm_or_si128(letters, digits),
            _mm_or_si128(symbols, non_ascii),
        );
        _mm_movemask_epi8(valid) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifier_scans_match_scalar_at_every_byte_and_boundary() {
        for offset in 0..16 {
            for length in [
                0usize, 1, 2, 3, 4, 7, 8, 15, 16, 17, 18, 19, 31, 32, 33, 63, 64, 65, 127, 128,
                129, 255, 256,
            ] {
                let mut storage = [b'a'; 288];
                for byte in 0..=255 {
                    for stop in [
                        0,
                        1,
                        2,
                        3,
                        7,
                        8,
                        15,
                        16,
                        17,
                        31,
                        32,
                        length.saturating_sub(1),
                    ] {
                        if stop >= length {
                            continue;
                        }
                        storage[offset + stop] = byte;
                        let line = &storage[offset..offset + length];
                        assert_eq!(
                            identifier_end(line, 0),
                            scalar_end(line, 0),
                            "{offset} {length} {stop} {byte}"
                        );
                        storage[offset + stop] = b'a';
                    }
                }
                let line = &storage[offset..offset + length];
                for from in 0..=length {
                    assert_eq!(
                        identifier_end(line, from),
                        length,
                        "{offset} {length} {from}"
                    );
                }
            }
        }
    }

    #[test]
    fn identifier_scans_match_scalar_for_mixed_bytes() {
        let mut random = 0xdecafbad_u64;
        let mut bytes = [0; 256];
        for length in 0..=bytes.len() {
            for _ in 0..16 {
                for byte in &mut bytes[..length] {
                    random ^= random << 13;
                    random ^= random >> 7;
                    random ^= random << 17;
                    *byte = random as u8;
                }
                for from in 0..=length {
                    assert_eq!(
                        identifier_end(&bytes[..length], from),
                        scalar_end(&bytes[..length], from)
                    );
                }
            }
        }
    }

    fn scalar_end(line: &[u8], from: usize) -> usize {
        from + line[from..]
            .iter()
            .take_while(|&&b| b.is_ascii_alphanumeric() || b == b'_' || b == b'$' || b >= 128)
            .count()
    }
}
