use crate::index::bigram_filter::BigramFilter;
use regex_syntax::hir::{Class, Hir, HirKind};
use smallvec::SmallVec;
use std::borrow::Cow;

const MAX_CLASS_EXPAND: usize = 16;

// stack inlined array padded with 0 and tracked length
#[derive(Clone, Copy, PartialEq, Eq)]
struct InlineArray {
    bytes: [u8; MAX_CLASS_EXPAND],
    len: usize,
}

impl InlineArray {
    const fn new() -> Self {
        Self {
            bytes: [0; MAX_CLASS_EXPAND],
            len: 0,
        }
    }

    fn from_byte(b: u8) -> Self {
        let mut set = Self::new();
        set.push(b);
        set
    }

    /// Append a byte; no-op if already full (callers guard against this).
    fn push(&mut self, b: u8) {
        if self.len < MAX_CLASS_EXPAND {
            self.bytes[self.len] = b;
            self.len += 1;
        }
    }
}

impl std::ops::Deref for InlineArray {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

#[inline]
fn consec_key(a: u8, b: u8) -> Option<u16> {
    let al = a.to_ascii_lowercase();
    let bl = b.to_ascii_lowercase();
    if (32..=126).contains(&al) && (32..=126).contains(&bl) {
        Some((al as u16) << 8 | bl as u16)
    } else {
        None
    }
}

#[derive(Debug, Clone)]
pub enum BigramQuery {
    Any,
    /// A consecutive bigram key to look up in the main index.
    Consec(u16),
    /// A skip-1 bigram key to look up in the skip sub-index.
    Skip1(u16),
    /// All children must match (intersect posting lists).
    And(Vec<BigramQuery>),
    /// At least one child must match (union posting lists).
    Or(Vec<BigramQuery>),
}

/// SIMD-friendly bitwise OR of two equal-length bitsets.
#[inline]
fn bitset_or(a: &mut [u64], b: &[u64]) {
    a.iter_mut().zip(b.iter()).for_each(|(x, y)| *x |= *y);
}

/// SIMD-friendly bitwise AND of two equal-length bitsets.
#[inline]
fn bitset_and(a: &mut [u64], b: &[u64]) {
    a.iter_mut().zip(b.iter()).for_each(|(x, y)| *x &= *y);
}

impl BigramQuery {
    pub fn is_any(&self) -> bool {
        matches!(self, BigramQuery::Any)
    }

    pub(crate) fn evaluate(&self, index: &BigramFilter) -> Option<Vec<u64>> {
        self.evaluate_cow(index).map(Cow::into_owned)
    }

    fn evaluate_cow<'a>(&self, index: &'a BigramFilter) -> Option<Cow<'a, [u64]>> {
        match self {
            BigramQuery::Any => None,

            BigramQuery::Consec(key) => index.column_bitset(*key),

            BigramQuery::Skip1(key) => index.skip_index()?.column_bitset(*key),

            BigramQuery::And(children) => {
                let mut result: Option<Vec<u64>> = None;
                for child in children {
                    if let Some(child_bits) = child.evaluate_cow(index) {
                        result = Some(match result {
                            None => child_bits.into_owned(),
                            Some(mut r) => {
                                bitset_and(&mut r, &child_bits);
                                r
                            }
                        });
                    }
                }
                result.map(Cow::Owned)
            }

            BigramQuery::Or(children) => {
                if children.is_empty() {
                    return None;
                }
                let mut result: Option<Vec<u64>> = None;
                for child in children {
                    // Any branch can't be filtered -> whole OR can't be filtered
                    let child_bits = child.evaluate_cow(index)?;
                    result = Some(match result {
                        None => child_bits.into_owned(),
                        Some(mut r) => {
                            bitset_or(&mut r, &child_bits);
                            r
                        }
                    });
                }
                result.map(Cow::Owned)
            }
        }
    }
}

struct HirInfo {
    query: BigramQuery,
    first: Option<InlineArray>,
    last: Option<InlineArray>,
    can_be_empty: bool,
}

impl HirInfo {
    fn empty() -> Self {
        Self {
            query: BigramQuery::Any,
            first: None,
            last: None,
            can_be_empty: true,
        }
    }
}

/// Prefilter fuzzy query. The algorithm is the following:
/// we allow max_typos = min(len/3,2) every typo destroys at most 2 consecutive bigrams
/// So out of N bigrams at least N - 2 * max_typos have to present in the matching fil
pub(crate) fn fuzzy_to_bigram_query(query: &str, num_probes: usize) -> BigramQuery {
    let lower: Vec<u8> = query.bytes().map(|b| b.to_ascii_lowercase()).collect();

    if lower.len() < 2 {
        return BigramQuery::Any;
    }

    let max_typos = (lower.len() / 3).min(2);

    // Extract all consecutive bigram keys.
    let bigram_keys: Vec<u16> = lower
        .windows(2)
        .filter_map(|w| consec_key(w[0], w[1]))
        .collect();

    if bigram_keys.is_empty() {
        return BigramQuery::Any;
    }

    // the simplest case, just check that every bigram is present either consec or not
    if max_typos == 0 {
        return simplify_and(
            bigram_keys
                .iter()
                .map(|&k| BigramQuery::Consec(k))
                .collect(),
        );
    }

    // Pick evenly-spaced probe bigrams.
    let n = num_probes.min(bigram_keys.len());
    if n <= max_typos {
        // Too few probes to require anything useful.
        return simplify_or(
            bigram_keys
                .iter()
                .map(|&k| BigramQuery::Consec(k))
                .collect(),
        );
    }

    let probes: Vec<u16> = if n == bigram_keys.len() {
        bigram_keys
    } else {
        (0..n)
            .map(|i| {
                let idx = i * (bigram_keys.len() - 1) / (n - 1);
                bigram_keys[idx]
            })
            .collect()
    };

    let required = n - max_typos;

    // If required == n, just AND all probes.
    if required >= n {
        return simplify_and(probes.iter().map(|&k| BigramQuery::Consec(k)).collect());
    }

    // Generate all C(n, required) subsets as OR(AND(subset), ...)
    let mut branches = Vec::new();
    let mut combo = vec![0u16; required];
    combine(&probes, required, 0, 0, &mut combo, &mut branches);

    simplify_or(branches)
}

/// Build C(n, k) combination branches in-place on a fixed-size slice.
fn combine(
    items: &[u16],
    k: usize,
    start: usize,
    depth: usize,
    combo: &mut [u16],
    branches: &mut Vec<BigramQuery>,
) {
    if depth == k {
        branches.push(simplify_and(
            combo.iter().map(|&key| BigramQuery::Consec(key)).collect(),
        ));
        return;
    }
    let remaining = k - depth;
    for i in start..=items.len() - remaining {
        combo[depth] = items[i];
        combine(items, k, i + 1, depth + 1, combo, branches);
    }
}

pub(crate) fn regex_to_bigram_query(pattern: &str) -> BigramQuery {
    let mut parser = regex_syntax::ParserBuilder::new()
        .unicode(false)
        .utf8(false)
        .build();

    let hir = match parser.parse(pattern) {
        Ok(h) => h,
        Err(_) => return BigramQuery::Any,
    };

    decompose(&hir).query
}

fn decompose(hir: &Hir) -> HirInfo {
    let can_be_empty = hir.properties().minimum_len().is_none_or(|n| n == 0);

    match hir.kind() {
        HirKind::Empty => HirInfo::empty(),

        HirKind::Literal(lit) => decompose_literal(lit.0.as_ref()),

        HirKind::Class(class) => {
            let bytes = expand_class(class);
            match bytes {
                Some(b) if !b.is_empty() => HirInfo {
                    query: BigramQuery::Any,
                    first: Some(b),
                    last: Some(b),
                    can_be_empty,
                },
                _ => HirInfo {
                    query: BigramQuery::Any,
                    first: None,
                    last: None,
                    can_be_empty,
                },
            }
        }

        HirKind::Look(_) => HirInfo::empty(),

        HirKind::Repetition(rep) => {
            let inner = decompose(&rep.sub);
            if rep.min == 0 {
                HirInfo {
                    query: BigramQuery::Any,
                    first: inner.first,
                    last: inner.last,
                    can_be_empty: true,
                }
            } else {
                // min >= 1: inner bigrams guaranteed
                let mut qs = Vec::new();
                if !inner.query.is_any() {
                    qs.push(inner.query.clone());
                }
                // min >= 2: cross-boundary between consecutive occurrences
                if rep.min >= 2 {
                    push_cross_consec(&mut qs, inner.last.as_deref(), inner.first.as_deref());
                }
                HirInfo {
                    query: simplify_and(qs),
                    first: inner.first,
                    last: inner.last,
                    can_be_empty,
                }
            }
        }

        HirKind::Capture(cap) => decompose(&cap.sub),

        HirKind::Concat(parts) => decompose_concat(parts),

        HirKind::Alternation(alts) => decompose_alternation(alts),
    }
}

/// Extract bigrams from a literal byte sequence.
fn decompose_literal(bytes: &[u8]) -> HirInfo {
    if bytes.is_empty() {
        return HirInfo::empty();
    }

    let lower: SmallVec<[u8; 64]> = bytes.iter().map(|b| b.to_ascii_lowercase()).collect();

    if lower.len() == 1 {
        let b = lower[0];
        let first = if (32..=126).contains(&b) {
            Some(InlineArray::from_byte(b))
        } else {
            None
        };
        return HirInfo {
            query: BigramQuery::Any,
            first,
            last: first,
            can_be_empty: false,
        };
    }

    let mut qs: Vec<BigramQuery> = Vec::new();

    // Consecutive bigrams
    for w in lower.windows(2) {
        if let Some(k) = consec_key(w[0], w[1]) {
            qs.push(BigramQuery::Consec(k));
        }
    }

    // Skip-1 bigrams from the literal itself
    if lower.len() >= 3 {
        for i in 0..lower.len() - 2 {
            if let Some(k) = consec_key(lower[i], lower[i + 2]) {
                qs.push(BigramQuery::Skip1(k));
            }
        }
    }

    let first_byte = lower[0];
    let last_byte = *lower.last().unwrap();

    HirInfo {
        query: simplify_and(qs),
        first: if (32..=126).contains(&first_byte) {
            Some(InlineArray::from_byte(first_byte))
        } else {
            None
        },
        last: if (32..=126).contains(&last_byte) {
            Some(InlineArray::from_byte(last_byte))
        } else {
            None
        },
        can_be_empty: false,
    }
}

fn decompose_concat(parts: &[Hir]) -> HirInfo {
    if parts.is_empty() {
        return HirInfo::empty();
    }

    let infos: Vec<HirInfo> = parts.iter().map(decompose).collect();
    let mut qs: Vec<BigramQuery> = Vec::new();

    for info in &infos {
        if !info.query.is_any() {
            qs.push(info.query.clone());
        }
    }

    //  Dense cross-boundary between adjacent mandatory parts
    for pair in infos.windows(2) {
        if !pair[0].can_be_empty && !pair[1].can_be_empty {
            push_cross_consec(&mut qs, pair[0].last.as_deref(), pair[1].first.as_deref());
        }
    }

    //  Sparse-1 cross-boundary: across a single 1 byte wide middle part
    if parts.len() >= 3 {
        for i in 0..parts.len() - 2 {
            let left = &infos[i];
            let mid = &parts[i + 1];
            let right = &infos[i + 2];

            let min_len = mid.properties().minimum_len();
            let max_len = mid.properties().maximum_len();
            let is_1byte = min_len == Some(1) && max_len == Some(1);

            if is_1byte && !left.can_be_empty && !right.can_be_empty {
                push_cross_skip1(&mut qs, left.last.as_deref(), right.first.as_deref());
            }
        }
    }

    let first = collect_first(&infos);
    let last = collect_last(&infos);
    let can_be_empty = infos.iter().all(|i| i.can_be_empty);

    HirInfo {
        query: simplify_and(qs),
        first,
        last,
        can_be_empty,
    }
}

fn decompose_alternation(alts: &[Hir]) -> HirInfo {
    if alts.is_empty() {
        return HirInfo::empty();
    }

    let infos: Vec<HirInfo> = alts.iter().map(decompose).collect();
    let query = simplify_or(infos.iter().map(|i| i.query.clone()).collect());
    let first = merge_byte_sets(infos.iter().map(|i| &i.first));
    let last = merge_byte_sets(infos.iter().map(|i| &i.last));
    let can_be_empty = infos.iter().any(|i| i.can_be_empty);

    HirInfo {
        query,
        first,
        last,
        can_be_empty,
    }
}

fn expand_class(class: &Class) -> Option<InlineArray> {
    let mut bytes = InlineArray::new();
    match class {
        Class::Bytes(bc) => {
            for range in bc.ranges() {
                let count = (range.end() as usize) - (range.start() as usize) + 1;
                if bytes.len() + count > MAX_CLASS_EXPAND {
                    return None;
                }

                for b in range.start()..=range.end() {
                    if (32..=126).contains(&b) {
                        let lower = b.to_ascii_lowercase();
                        if !bytes.contains(&lower) {
                            bytes.push(lower);
                        }
                    }
                }
            }
        }
        Class::Unicode(uc) => {
            for range in uc.ranges() {
                let start = range.start() as u32;
                let end = range.end() as u32;
                if start > 127 {
                    continue;
                }
                let ascii_end = end.min(126) as u8;
                let ascii_start = start.max(32) as u8;
                if ascii_start > ascii_end {
                    continue;
                }
                let count = (ascii_end - ascii_start) as usize + 1;
                if bytes.len() + count > MAX_CLASS_EXPAND {
                    return None;
                }
                for b in ascii_start..=ascii_end {
                    let lower = b.to_ascii_lowercase();
                    if !bytes.contains(&lower) {
                        bytes.push(lower);
                    }
                }
            }
        }
    }
    if bytes.is_empty() { None } else { Some(bytes) }
}

/// Push consecutive cross-product bigrams into `qs`.
fn push_cross_consec(qs: &mut Vec<BigramQuery>, last: Option<&[u8]>, first: Option<&[u8]>) {
    if let Some(q) = cross_product(last, first, false) {
        qs.push(q);
    }
}

/// Push skip-1 cross-product bigrams into `qs`.
fn push_cross_skip1(qs: &mut Vec<BigramQuery>, last: Option<&[u8]>, first: Option<&[u8]>) {
    if let Some(q) = cross_product(last, first, true) {
        qs.push(q);
    }
}

fn cross_product(last: Option<&[u8]>, first: Option<&[u8]>, skip: bool) -> Option<BigramQuery> {
    let last = last?;
    let first = first?;
    let n = last.len() * first.len();
    if n == 0 || n > MAX_CLASS_EXPAND * MAX_CLASS_EXPAND {
        return None;
    }

    let mut bigrams: Vec<BigramQuery> = Vec::with_capacity(n);
    for &l in last {
        for &f in first {
            if let Some(k) = consec_key(l, f) {
                let node = if skip {
                    BigramQuery::Skip1(k)
                } else {
                    BigramQuery::Consec(k)
                };
                bigrams.push(node);
            }
        }
    }

    match bigrams.len() {
        0 => None,
        1 => Some(bigrams.into_iter().next().unwrap()),
        _ => Some(simplify_or(bigrams)),
    }
}

fn collect_first(infos: &[HirInfo]) -> Option<InlineArray> {
    let mut result = InlineArray::new();
    for info in infos {
        if let Some(ref bytes) = info.first {
            for &b in bytes.iter() {
                if !result.contains(&b) {
                    if result.len() >= MAX_CLASS_EXPAND {
                        return None;
                    }
                    result.push(b);
                }
            }
        } else if !info.can_be_empty {
            return None;
        }
        if !info.can_be_empty {
            break;
        }
    }
    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

fn collect_last(infos: &[HirInfo]) -> Option<InlineArray> {
    let mut result = InlineArray::new();
    for info in infos.iter().rev() {
        if let Some(ref bytes) = info.last {
            for &b in bytes.iter() {
                if !result.contains(&b) {
                    if result.len() >= MAX_CLASS_EXPAND {
                        return None;
                    }
                    result.push(b);
                }
            }
        } else if !info.can_be_empty {
            return None;
        }
        if !info.can_be_empty {
            break;
        }
    }
    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

fn merge_byte_sets<'a>(iter: impl Iterator<Item = &'a Option<InlineArray>>) -> Option<InlineArray> {
    let mut result = InlineArray::new();
    for opt in iter {
        let bytes = opt.as_ref()?;

        for &b in bytes.iter() {
            if !result.contains(&b) {
                if result.len() >= MAX_CLASS_EXPAND {
                    return None;
                }
                result.push(b);
            }
        }
    }
    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

fn simplify_and(children: Vec<BigramQuery>) -> BigramQuery {
    let mut flat: Vec<BigramQuery> = Vec::new();
    for child in children {
        match child {
            BigramQuery::Any => {}
            BigramQuery::And(inner) => flat.extend(inner),
            other => flat.push(other),
        }
    }
    match flat.len() {
        0 => BigramQuery::Any,
        1 => flat.into_iter().next().unwrap(),
        _ => BigramQuery::And(flat),
    }
}

fn simplify_or(children: Vec<BigramQuery>) -> BigramQuery {
    if children.iter().any(|c| c.is_any()) {
        return BigramQuery::Any;
    }
    let mut flat: Vec<BigramQuery> = Vec::new();
    for child in children {
        match child {
            BigramQuery::Or(inner) => flat.extend(inner),
            other => flat.push(other),
        }
    }
    match flat.len() {
        0 => BigramQuery::Any,
        1 => flat.into_iter().next().unwrap(),
        _ => BigramQuery::Or(flat),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::bigram_filter::BigramIndexBuilder;

    /// Build a tiny index from the given file contents for testing.
    fn build_test_index(files: &[&[u8]]) -> BigramFilter {
        let n = files.len();
        let consec_builder = BigramIndexBuilder::new(n);
        let skip_builder = BigramIndexBuilder::new(n);
        for (i, content) in files.iter().enumerate() {
            consec_builder.add_file_content(&skip_builder, i, content);
        }
        let mut idx = consec_builder.compress(Some(0));
        idx.set_skip_index(skip_builder.compress(Some(0)));
        idx
    }

    #[test]
    fn literal_pattern() {
        let idx = build_test_index(&[
            b"hello world",     // 0: contains "hello"
            b"goodbye world",   // 1: no "hello"
            b"say hello there", // 2: contains "hello"
        ]);

        let q = regex_to_bigram_query("hello");
        assert!(!q.is_any());

        let candidates = q.evaluate(&idx).unwrap();
        assert!(BigramFilter::is_candidate(&candidates, 0));
        assert!(!BigramFilter::is_candidate(&candidates, 1));
        assert!(BigramFilter::is_candidate(&candidates, 2));
    }

    #[test]
    fn alternation() {
        let idx = build_test_index(&[
            b"has foo in it", // 0
            b"has bar in it", // 1
            b"has xyz in it", // 2
        ]);

        let q = regex_to_bigram_query("foo|bar");
        assert!(!q.is_any());

        let candidates = q.evaluate(&idx).unwrap();
        assert!(BigramFilter::is_candidate(&candidates, 0));
        assert!(BigramFilter::is_candidate(&candidates, 1));
        // xyz doesn't contain foo or bar bigrams
        assert!(!BigramFilter::is_candidate(&candidates, 2));
    }

    #[test]
    fn wildcard_concat() {
        let idx = build_test_index(&[
            b"foo something bar", // 0
            b"foo only",          // 1: has foo but not bar
            b"only bar",          // 2: has bar but not foo
        ]);

        let q = regex_to_bigram_query("foo.*bar");
        assert!(!q.is_any());

        let candidates = q.evaluate(&idx).unwrap();
        assert!(BigramFilter::is_candidate(&candidates, 0));
        // file 1 and 2 should be filtered (missing bigrams from "bar" / "foo")
        assert!(!BigramFilter::is_candidate(&candidates, 1));
        assert!(!BigramFilter::is_candidate(&candidates, 2));
    }

    #[test]
    fn sparse1_across_dot() {
        // "a.b" should produce a skip-1 bigram (a,b)
        let idx = build_test_index(&[
            b"axb", // 0: has sparse-1 (a,b)
            b"ayb", // 1: has sparse-1 (a,b)
            b"xyz", // 2: no (a,b) at all
        ]);

        let q = regex_to_bigram_query("a.b");
        assert!(!q.is_any());

        let candidates = q.evaluate(&idx).unwrap();
        assert!(BigramFilter::is_candidate(&candidates, 0));
        assert!(BigramFilter::is_candidate(&candidates, 1));
        assert!(!BigramFilter::is_candidate(&candidates, 2));
    }

    #[test]
    fn sparse1_across_digit() {
        // "foo\dbar" -> sparse-1 (o,b) across \d
        let idx = build_test_index(&[
            b"foo3bar baz", // 0: has all bigrams
            b"foobar baz",  // 1: has consecutive (o,b) but pattern needs sparse-1
            b"xyz only",    // 2: no relevant bigrams
        ]);

        let q = regex_to_bigram_query(r"foo\dbar");
        assert!(!q.is_any());

        let candidates = q.evaluate(&idx).unwrap();
        assert!(BigramFilter::is_candidate(&candidates, 0));
        // file 1 may or may not match depending on what bigrams are in the index
        // (it has all the literal bigrams and also o,b as both consec and skip-1)
        // The important thing is file 2 is excluded:
        assert!(!BigramFilter::is_candidate(&candidates, 2));
    }

    #[test]
    fn pure_wildcard_is_any() {
        let q = regex_to_bigram_query(".*");
        assert!(q.is_any());
    }

    #[test]
    fn single_char_is_any() {
        let q = regex_to_bigram_query("a");
        assert!(q.is_any());
    }

    #[test]
    fn invalid_regex_is_any() {
        let q = regex_to_bigram_query("[invalid");
        assert!(q.is_any());
    }

    #[test]
    fn optional_group_excluded() {
        let q = regex_to_bigram_query("foo(bar)?baz");
        assert!(!q.is_any());

        let idx = build_test_index(&[
            b"foobaz content",    // 0: has foo+baz bigrams (bar absent)
            b"foobarbaz content", // 1: has everything
            b"xyz only",          // 2: nothing
        ]);

        let candidates = q.evaluate(&idx).unwrap();
        assert!(BigramFilter::is_candidate(&candidates, 0));
        assert!(BigramFilter::is_candidate(&candidates, 1));
        assert!(!BigramFilter::is_candidate(&candidates, 2));
    }

    #[test]
    fn repetition_min2_cross_boundary() {
        // (ab){2,} -> bigram "ab" + cross-boundary "b","a"
        let q = regex_to_bigram_query("(ab){2,}");
        assert!(!q.is_any());

        let idx = build_test_index(&[
            b"ababab", // 0: has "ab" and "b"->"a"
            b"abonly", // 1: has "ab" but not "b"->"a"
            b"xyz",    // 2: nothing
        ]);

        let candidates = q.evaluate(&idx).unwrap();
        assert!(BigramFilter::is_candidate(&candidates, 0));
        assert!(!BigramFilter::is_candidate(&candidates, 2));
    }

    #[test]
    fn two_dots_no_sparse1() {
        let q = regex_to_bigram_query("a..b");
        // Single-char literals with 2 unknown bytes between -> Any
        assert!(q.is_any());
    }

    #[test]
    fn character_class_cross_boundary() {
        // [abc]de -> cross-boundary OR(ad,bd,cd) + bigram de
        // All three class variants must appear in the corpus so the OR
        // branches are tracked in the index (untracked bigrams make the
        // OR conservatively return None, which is correct but untestable).
        let idx = build_test_index(&[
            b"ade content", // 0: has ad
            b"bde content", // 1: has bd
            b"cde content", // 2: has cd
            b"xde content", // 3: has de but not ad/bd/cd
        ]);

        let q = regex_to_bigram_query("[abc]de");
        assert!(!q.is_any());

        let candidates = q.evaluate(&idx).unwrap();
        assert!(BigramFilter::is_candidate(&candidates, 0));
        assert!(BigramFilter::is_candidate(&candidates, 1));
        assert!(BigramFilter::is_candidate(&candidates, 2));
        // file 3 doesn't have ad/bd/cd so should be filtered
        assert!(!BigramFilter::is_candidate(&candidates, 3));
    }

    fn has_consec(q: &BigramQuery, a: u8, b: u8) -> bool {
        let Some(key) = consec_key(a, b) else {
            return false;
        };
        match q {
            BigramQuery::Consec(k) => *k == key,
            BigramQuery::And(cs) | BigramQuery::Or(cs) => cs.iter().any(|c| has_consec(c, a, b)),
            _ => false,
        }
    }

    fn has_skip1(q: &BigramQuery, a: u8, b: u8) -> bool {
        let Some(key) = consec_key(a, b) else {
            return false;
        };
        match q {
            BigramQuery::Skip1(k) => *k == key,
            BigramQuery::And(cs) | BigramQuery::Or(cs) => cs.iter().any(|c| has_skip1(c, a, b)),
            _ => false,
        }
    }

    /// Bigram expectation: `("ab", is_skip1)`.
    /// The 2-char str is the byte pair; C = consecutive, S = skip-1.
    type Bg = (&'static str, bool);
    const C: bool = false;
    const S: bool = true;

    /// Top 15+ commonly used regex patterns from
    /// https://digitalfortress.tech/tips/top-15-commonly-used-regex/
    /// plus typical grep patterns used by agentic tools.
    ///
    /// Each entry: `(regex, Option<&[Bg]>)`.
    ///   - `None`        -> pure classes / unsupported syntax, Any is acceptable.
    ///   - `Some(&[..])` -> must be non-Any, and every listed bigram must appear.
    #[test]
    fn common_regex_patterns() {
        #[rustfmt::skip]
        let cases: &[(&str, Option<&[Bg]>)] = &[
            (r"^\d+$",                                                      None), // 1.  whole numbers
            (r"^\d*\.\d+$",                                                 None), // 2.  decimals
            (r"^\d*(\.\d+)?$",                                              None), // 3.  whole + decimal
            (r"^-?\d*(\.\d+)?$",                                            None), // 4.  neg/pos decimal
            (r"[-]?[0-9]+[,.]?[0-9]*([/][0-9]+[,.]?[0-9]*)*",             None), // 5.  fractions
            (r"^[a-zA-Z0-9]*$",                                             None), // 6.  alphanumeric
            (r"^[a-zA-Z0-9 ]*$",                                            None), // 7.  alphanum + space
            (r"^([a-zA-Z0-9._%-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,6})*$",      None), // 8.  email
            (r"^([a-z0-9_\.\+-]+)@([\da-z\.-]+)\.([a-z\.]{2,6})$",         None), // 9.  email v2
            (r"(?=(.*[0-9]))(?=.*[!@#$%^&*()\[\]{}\-_+=~`|:;<>,./?\x5c])(?=.*[a-z])(?=(.*[A-Z]))(?=(.*)).{8,}", None), // 10. complex pw
            (r"(?=(.*[0-9]))((?=.*[A-Za-z0-9])(?=.*[A-Z])(?=.*[a-z]))^.{8,}$", None), // 11. moderate pw
            (r"^[a-z0-9_-]{3,16}$",                                        None), // 12. username
            (r"(https?://)?(www\.)?[-a-zA-Z0-9@:%._\+~#=]{2,256}\.[a-z]{2,6}\b([-a-zA-Z0-9@:%_\+.~#?&//=]*)", None), // 14. URL optional
            (r"^(([0-9]|[1-9][0-9]|1[0-9]{2}|2[0-4][0-9]|25[0-5])\.){3}([0-9]|[1-9][0-9]|1[0-9]{2}|2[0-4][0-9]|25[0-5])$", None), // 15. IPv4
            (r"(([0-9a-fA-F]{1,4}:){7,7}[0-9a-fA-F]{1,4}|([0-9a-fA-F]{1,4}:){1,7}:|([0-9a-fA-F]{1,4}:){1,6}:[0-9a-fA-F]{1,4}|([0-9a-fA-F]{1,4}:){1,5}(:[0-9a-fA-F]{1,4}){1,2}|([0-9a-fA-F]{1,4}:){1,4}(:[0-9a-fA-F]{1,4}){1,3}|([0-9a-fA-F]{1,4}:){1,3}(:[0-9a-fA-F]{1,4}){1,4}|([0-9a-fA-F]{1,4}:){1,2}(:[0-9a-fA-F]{1,4}){1,5}|[0-9a-fA-F]{1,4}:((:[0-9a-fA-F]{1,4}){1,6})|:((:[0-9a-fA-F]{1,4}){1,7}|:)|fe80:(:[0-9a-fA-F]{0,4}){0,4}%[0-9a-zA-Z]{1,}|::(ffff(:0{1,4}){0,1}:){0,1}((25[0-5]|(2[0-4]|1{0,1}[0-9]){0,1}[0-9])\.){3,3}(25[0-5]|(2[0-4]|1{0,1}[0-9]){0,1}[0-9])|([0-9a-fA-F]{1,4}:){1,4}:((25[0-5]|(2[0-4]|1{0,1}[0-9]){0,1}[0-9])\.){3,3}(25[0-5]|(2[0-4]|1{0,1}[0-9]){0,1}[0-9]))", None), // 16. IPv6
            (r"[12]\d{3}-(0[1-9]|1[0-2])-(0[1-9]|[12]\d|3[01])",          None), // 17. date
            (r"^(0?[1-9]|1[0-2]):[0-5][0-9]$",                            None), // 18. time 12h
            (r"((1[0-2]|0?[1-9]):([0-5][0-9]) ?([AaPp][Mm]))",            None), // 19. time AM/PM
            (r"^(0[0-9]|1[0-9]|2[0-3]):[0-5][0-9]$",                     None), // 20. time 24h
            (r"^([0-9]|0[0-9]|1[0-9]|2[0-3]):[0-5][0-9]$",               None), // 21. time 24h v2
            (r"(?:[01]\d|2[0123]):(?:[012345]\d):(?:[012345]\d)",          None), // 22. time+sec
            (r"</?[\w\s]*>|<.+[\W]>",                                      None), // 23. HTML tag
            (r"\bon\w+=\S+(?=.*>)",                                        None), // 24. inline JS
            (r"^[a-z0-9]+(?:-[a-z0-9]+)*$",                               None), // 25. slug
            (r"(\b\w+\b)(?=.*\b\1\b)",                                    None), // 26. dup words
            (r"^[\w,\s-]+\.[A-Za-z]{3}$",                                 None), // 27. filename
            (r"^[A-PR-WY][1-9]\d\s?\d{4}[1-9]$",                         None), // 28. HK ID

            // 13. URL with required protocol
            (r"https?://(www\.)?[-a-zA-Z0-9@:%._\+~#=]{2,256}\.[a-z]{2,6}\b([-a-zA-Z0-9@:%_\+.~#?&//=]*)", Some(&[
                ("ht", C), ("tt", C), ("tp", C),    // from "http"
                ("ht", S), ("tp", S),               // from "http" skip-1
                (":/", C), ("//", C),               // from "://"
            ])),

            // 29. fn\s+\w+
            (r"fn\s+\w+", Some(&[
                ("fn", C),                          // from "fn"
                ("n ", C),                          // cross-boundary: 'n' -> \s starts ' '
            ])),

            // 30. use\s+crate::
            (r"use\s+crate::", Some(&[
                ("us", C), ("se", C), ("ue", S),    // from "use"
                ("cr", C), ("ra", C), ("at", C),    // from "crate"
                ("te", C), ("::", C),
                ("ca", S), ("rt", S), ("ae", S),    // "crate" skip-1
            ])),

            // 31. unwrap\(\)|expect\(
            (r"unwrap\(\)|expect\(", Some(&[
                ("nw", C), ("wr", C), ("ra", C),    // "unwrap("
                ("ap", C), ("p(", C),
                ("xp", C), ("pe", C), ("ec", C),    // "expect("
                ("ct", C), ("t(", C),
            ])),

            // 32. TODO|FIXME|HACK
            (r"TODO|FIXME|HACK", Some(&[
                ("to", C), ("od", C), ("do", C),    // "TODO"
                ("fi", C), ("ix", C), ("xm", C),    // "FIXME"
                ("me", C),
                ("ha", C), ("ac", C), ("ck", C),    // "HACK"
                ("hc", S), ("ak", S),               // "HACK" skip-1
            ])),
        ];

        for (i, &(pattern, expected)) in cases.iter().enumerate() {
            let q = regex_to_bigram_query(pattern);

            if let Some(bigrams) = expected {
                assert!(
                    !q.is_any(),
                    "#{i} {pattern:?}: expected bigrams but got Any"
                );

                for &(pair, skip) in bigrams {
                    let b = pair.as_bytes();
                    debug_assert_eq!(b.len(), 2, "bigram must be 2 chars: {pair:?}");
                    let found = if skip {
                        has_skip1(&q, b[0], b[1])
                    } else {
                        has_consec(&q, b[0], b[1])
                    };
                    let kind = if skip { "skip-1" } else { "consec" };
                    assert!(found, "#{i} {pattern:?}: missing {kind} bigram {pair:?}");
                }
            }
        }
    }
}
