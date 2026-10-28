use super::languages::Language;

#[derive(Clone, Copy)]
struct Suffix {
    mask: u64,
    value: u64,
    language: Language,
}

pub(super) struct SuffixMatcher<const N: usize> {
    buckets: [u16; 1024],
    suffixes: [Suffix; N],
}

impl<const N: usize> SuffixMatcher<N> {
    pub(super) const fn new(extensions: &[(&[u8], Language)]) -> Self {
        assert!(extensions.len() == N && N < 256);
        let mut matcher = Self {
            buckets: [0; 1024],
            suffixes: [Suffix {
                mask: 0,
                value: 0,
                language: Language::Unsupported,
            }; N],
        };
        let mut bucket = 0;
        let mut count = 0;
        while bucket < matcher.buckets.len() {
            let start = count;
            let mut i = 0;
            while i < N {
                let suffix = Suffix::new(extensions[i].0, extensions[i].1);
                if suffix_bucket(suffix.value) == bucket {
                    matcher.suffixes[count] = suffix;
                    count += 1;
                }
                i += 1;
            }
            // Pack the suffix range into two bytes: start, count.
            matcher.buckets[bucket] = start as u16 | (((count - start) as u16) << 8);
            bucket += 1;
        }
        matcher
    }

    #[inline]
    pub(super) fn classify(&self, filename: &[u8]) -> Language {
        let tail = if let Some(bytes) = filename.last_chunk::<8>() {
            u64::from_be_bytes(*bytes)
        } else {
            let mut bytes = [0; 8];
            bytes[8 - filename.len()..].copy_from_slice(filename);
            u64::from_be_bytes(bytes)
        };
        let bucket = self.buckets[suffix_bucket(tail)];
        let start = usize::from(bucket & 255);
        let len = usize::from(bucket >> 8);
        for suffix in &self.suffixes[start..start + len] {
            if tail & suffix.mask == suffix.value {
                return suffix.language;
            }
        }
        Language::Unsupported
    }
}

impl Suffix {
    const fn new(extension: &[u8], language: Language) -> Self {
        assert!(!extension.is_empty() && extension.len() < 8);
        let mut suffix = Self {
            mask: 255,
            value: b'.' as u64,
            language,
        };
        let mut i = 0;
        while i < extension.len() {
            // Ignore ASCII case bits only for letters; dots and '+' must match exactly.
            let mask = if extension[i].is_ascii_alphabetic() {
                0xdf
            } else {
                0xff
            };
            suffix.mask = suffix.mask << 8 | mask as u64;
            suffix.value = suffix.value << 8 | (extension[i] & mask) as u64;
            i += 1;
        }
        suffix
    }
}

const fn suffix_bucket(tail: u64) -> usize {
    // The last two bytes select candidates; the full masked suffix resolves collisions.
    (((tail >> 8) & 31) * 32 + (tail & 31)) as usize
}
