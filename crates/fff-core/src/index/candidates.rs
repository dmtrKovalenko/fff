use super::{BigramFilter, BigramOverlay, extract_bigrams};
use super::{fuzzy_to_bigram_query, regex_to_bigram_query};

/// Number of evenly-spaced probe bigrams used by the fuzzy candidate query.
const FUZZY_PROBE_COUNT: usize = 7;

#[inline]
fn set_bit(candidates: &mut [u64], file_idx: usize) {
    let word = file_idx / 64;
    if word < candidates.len() {
        candidates[word] |= 1u64 << (file_idx % 64);
    }
}

#[inline]
fn clear_tombstones(candidates: &mut [u64], overlay: &BigramOverlay) {
    for (r, t) in candidates.iter_mut().zip(overlay.tombstones().iter()) {
        *r &= !t;
    }
}

/// Number of base files covered by the bigram bitset; files past this
/// boundary (overflow, max 1024) are always scanned.
#[inline]
pub(crate) fn bigram_boundary(overlay: Option<&BigramOverlay>, files_len: usize) -> usize {
    overlay.map(|o| o.base_file_count()).unwrap_or(files_len)
}

/// Candidate bitset for literal patterns, OR-ed across all of them: a file is
/// a candidate when it contains the bigrams of ANY pattern. Overlay-modified
/// files are re-checked against each pattern's bigrams.
pub(crate) fn literal_candidates(
    index: Option<&BigramFilter>,
    overlay: Option<&BigramOverlay>,
    patterns: &[&str],
) -> Option<Vec<u64>> {
    let index = ready_index(index)?;

    let mut combined: Option<Vec<u64>> = None;
    for pattern in patterns {
        // A pattern the index can't narrow matches any file, and the union with
        // it is every file — so bail out of prefiltering instead of dropping it.
        let candidates = index.query(pattern.as_bytes())?;
        combined = Some(match combined {
            None => candidates,
            Some(mut acc) => {
                acc.iter_mut()
                    .zip(candidates.iter())
                    .for_each(|(a, b)| *a |= *b);
                acc
            }
        });
    }

    let mut candidates = combined?;
    if let Some(overlay) = overlay {
        clear_tombstones(&mut candidates, overlay);
        for pattern in patterns {
            let pattern_bigrams = extract_bigrams(pattern.as_bytes());
            for file_idx in overlay.query_modified(&pattern_bigrams) {
                set_bit(&mut candidates, file_idx);
            }
        }
    }
    Some(candidates)
}

/// Candidate bitset for a regex pattern: the regex HIR is decomposed into an
/// AND/OR bigram query tree (supports alternation, optional groups, character
/// classes, and sparse-1 bigrams across single-byte wildcards). Since modified
/// file contents can't be re-checked against a regex cheaply, all
/// overlay-modified files are conservatively added.
pub(crate) fn regex_candidates(
    index: Option<&BigramFilter>,
    overlay: Option<&BigramOverlay>,
    pattern: &str,
) -> Option<Vec<u64>> {
    let index = ready_index(index)?;

    let bq = regex_to_bigram_query(pattern);
    if bq.is_any() {
        return None;
    }
    let candidates = bq.evaluate(index)?;
    Some(add_all_modified(candidates, overlay))
}

/// Candidate bitset for a fuzzy pattern: evenly-spaced probe bigrams with a
/// typo allowance (widely-spaced probes are far more selective than sliding
/// windows of adjacent bigrams). All overlay-modified files are added.
pub(crate) fn fuzzy_candidates(
    index: Option<&BigramFilter>,
    overlay: Option<&BigramOverlay>,
    pattern: &str,
) -> Option<Vec<u64>> {
    let index = ready_index(index)?;

    let bq = fuzzy_to_bigram_query(pattern, FUZZY_PROBE_COUNT);
    if bq.is_any() {
        return None;
    }
    let candidates = bq.evaluate(index)?;
    Some(add_all_modified(candidates, overlay))
}

#[inline]
fn ready_index(index: Option<&BigramFilter>) -> Option<&BigramFilter> {
    index.filter(|idx| idx.is_ready())
}

fn add_all_modified(mut candidates: Vec<u64>, overlay: Option<&BigramOverlay>) -> Vec<u64> {
    if let Some(overlay) = overlay {
        clear_tombstones(&mut candidates, overlay);
        for file_idx in overlay.modified_indices() {
            set_bit(&mut candidates, file_idx);
        }
    }
    candidates
}
