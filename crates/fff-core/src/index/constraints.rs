//! Constraint-based prefiltering for search queries.

use fff_query_parser::{Constraint, GitStatusFilter};
use smallvec::SmallVec;

use crate::git::is_modified_status;
use crate::simd_path::ArenaPtr;
use crate::simd_string_utils::memmem::find_case_insensitive_short;

const PAR_THRESHOLD: usize = 10_000;

pub(crate) trait Constrainable {
    fn write_file_name(&self, arena: ArenaPtr, out: &mut String);
    fn git_status(&self) -> Option<git2::Status>;
    fn write_relative_path(&self, arena: ArenaPtr, out: &mut String);
    fn is_overflow(&self) -> bool;
}

/// Stored/canonical paths use `/`; also accept `\` so a Windows user typing
/// a native separator in a query still matches.
#[inline]
fn is_path_sep(b: u8) -> bool {
    b == b'/' || b == b'\\'
}

#[inline]
fn path_slice_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).all(|(x, y)| {
        if is_path_sep(*x) && is_path_sep(*y) {
            true
        } else {
            x.eq_ignore_ascii_case(y)
        }
    })
}

/// Path ends with suffix at a path-separator boundary (case-insensitive).
#[inline]
pub fn path_ends_with_suffix(path: &str, suffix: &str) -> bool {
    let path_bytes = path.as_bytes();
    let suffix_bytes = suffix.as_bytes();
    if path_bytes.len() < suffix_bytes.len() {
        return false;
    }

    let start = path.len() - suffix.len();

    // Multi-byte UTF-8 may put `start` inside a char.
    if !path.is_char_boundary(start) {
        return false;
    }

    if !path_slice_eq(&path_bytes[start..], suffix_bytes) {
        return false;
    }

    // Exact or preceded by a separator. Scan backward past any multi-byte
    // continuation bytes to find the preceding ASCII byte.
    if start == 0 {
        return true;
    }
    let mut i = start;
    while i > 0 {
        i -= 1;
        if path_bytes[i] < 128 {
            return is_path_sep(path_bytes[i]);
        }
    }
    false
}

#[inline]
pub fn file_has_extension(file_name: &str, ext: &str) -> bool {
    let name_bytes = file_name.as_bytes();
    let ext_bytes = ext.as_bytes();
    if name_bytes.len() <= ext_bytes.len() + 1 {
        return false;
    }
    let start = name_bytes.len() - ext_bytes.len() - 1;
    if start > 0 && !file_name.is_char_boundary(start) {
        return false;
    }
    name_bytes.get(start) == Some(&b'.') && name_bytes[start + 1..].eq_ignore_ascii_case(ext_bytes)
}

/// Matches multi-segment queries like `libswscale/aarch64`.
#[inline]
pub fn path_contains_segment(path: &str, segment: &str) -> bool {
    let path_bytes = path.as_bytes();
    let segment_bytes = segment.as_bytes();
    let segment_len = segment_bytes.len();

    if path_bytes.len() > segment_len
        && is_path_sep(path_bytes[segment_len])
        && path.is_char_boundary(segment_len)
        && path_slice_eq(&path_bytes[..segment_len], segment_bytes)
    {
        return true;
    }

    if path_bytes.len() < segment_len + 2 {
        return false;
    }

    for i in 0..path_bytes.len().saturating_sub(segment_len + 1) {
        if is_path_sep(path_bytes[i]) {
            let start = i + 1;
            let end = start + segment_len;
            if end < path_bytes.len()
                && is_path_sep(path_bytes[end])
                && path.is_char_boundary(start)
                && path.is_char_boundary(end)
                && path_slice_eq(&path_bytes[start..end], segment_bytes)
            {
                return true;
            }
        }
    }
    false
}

/// Returns `None` if no constraints are present, `Some(filtered)` otherwise.
///
/// Constraint semantics:
/// - All `Extension` constraints OR together (file matches if ANY extension hits).
///   They're split out up front so the per-item loop reads the OR predicate as a
///   single short-circuit check, not as N AND-merged sub-constraints.
/// - Every other constraint kind ANDs (file matches only if ALL hold). They're
///   evaluated in order with short-circuit on first failure.
pub(crate) fn apply_constraints<'a, T: Constrainable + Sync>(
    items: &'a [T],
    constraints: &[Constraint<'_>],
    base_arena: ArenaPtr,
    overflow_arena: ArenaPtr,
) -> Option<Vec<&'a T>> {
    if constraints.is_empty() {
        return None;
    }
    let plan = ConstraintPlan::build(constraints, items, base_arena, overflow_arena);
    Some(plan.run(items, base_arena, overflow_arena))
}

#[cfg(feature = "zlob")]
pub(crate) type GlobPattern = zlob::ZlobPattern;
#[cfg(all(not(feature = "zlob"), feature = "ripgrep"))]
pub(crate) type GlobPattern = globset::GlobMatcher;

/// `PERIOD` on top of `RECOMMENDED`: we filter an index that already contains
/// dotfiles, so `**/x` must cross `.config`/`.pi` components. Without it zlob
/// applies fnmatch's leading-dot rule and diverges from the globset backend.
#[cfg(feature = "zlob")]
const GLOB_FLAGS: zlob::ZlobFlags = zlob::ZlobFlags::RECOMMENDED.union(zlob::ZlobFlags::PERIOD);

/// How `Constraint::Glob` is evaluated for each item.
enum GlobStrategy {
    /// No Glob constraint present.
    None,
    /// Pure-glob workload (no Extension filter to reject items first).
    /// Batch all paths through zlob/globset once; per-item check is a Vec<bool> lookup.
    Prepass(Vec<Vec<bool>>),
    /// Mixed workload (Extension filter present). Compile patterns up front, then
    /// only run them on items that survive the cheap Extension OR check.
    /// `None` slot = compile failure -> never matches; preserves index alignment.
    Inline(Vec<Option<GlobPattern>>),
}

/// Bundles preprocessed constraints for the per-item evaluator.
pub(crate) struct ConstraintPlan<'q, 'c> {
    /// OR semantics — file passes if ANY extension matches. Empty = no ext filter.
    extensions: SmallVec<[&'q str; 8]>,
    /// AND semantics — file passes only if ALL match.
    rest: SmallVec<[&'c Constraint<'q>; 8]>,
    glob: GlobStrategy,
}

pub(crate) struct ConstraintsBuffers {
    fname: String,
    path: String,
}

impl ConstraintsBuffers {
    pub(crate) fn new() -> Self {
        Self {
            fname: String::with_capacity(64),
            path: String::with_capacity(64),
        }
    }
}

impl<'q, 'c> ConstraintPlan<'q, 'c> {
    pub(crate) fn build<T: Constrainable>(
        constraints: &'c [Constraint<'q>],
        items: &[T],
        base_arena: ArenaPtr,
        overflow_arena: ArenaPtr,
    ) -> Self {
        let mut extensions = SmallVec::new();
        let mut rest: SmallVec<[&'c Constraint<'q>; 8]> = SmallVec::new();
        for c in constraints {
            match c {
                Constraint::Extension(ext) => extensions.push(*ext),
                _ => rest.push(c),
            }
        }
        let has_pre_filter = !extensions.is_empty() || rest.iter().any(|&c| !is_glob_node(c));
        let glob = build_glob_strategy(&rest, has_pre_filter, items, base_arena, overflow_arena);

        Self {
            extensions,
            rest,
            glob,
        }
    }

    fn run<'a, T: Constrainable + Sync>(
        &self,
        items: &'a [T],
        base_arean: ArenaPtr,
        overflow_arena: ArenaPtr,
    ) -> Vec<&'a T> {
        if items.len() >= PAR_THRESHOLD {
            use rayon::prelude::*;
            items
                .par_iter()
                .enumerate()
                .map_init(ConstraintsBuffers::new, |scratch, (i, item)| {
                    self.matches(item, i, base_arean, overflow_arena, scratch)
                        .then_some(item)
                })
                .flatten()
                .collect()
        } else {
            let mut scratch = ConstraintsBuffers::new();
            items
                .iter()
                .enumerate()
                .filter_map(|(i, item)| {
                    self.matches(item, i, base_arean, overflow_arena, &mut scratch)
                        .then_some(item)
                })
                .collect()
        }
    }

    #[inline]
    pub(crate) fn matches<T: Constrainable>(
        &self,
        item: &T,
        index: usize,
        base_arena: ArenaPtr,
        overflow_arena: ArenaPtr,
        scratch: &mut ConstraintsBuffers,
    ) -> bool {
        let arena = if item.is_overflow() {
            overflow_arena
        } else {
            base_arena
        };

        if !self.passes_extensions(item, arena, scratch) {
            return false;
        }

        let mut glob_idx = 0;
        self.rest.iter().all(|c| {
            evaluate(
                item,
                index,
                c,
                &self.glob,
                &mut glob_idx,
                false,
                arena,
                scratch,
            )
        })
    }

    #[inline]
    fn passes_extensions<T: Constrainable>(
        &self,
        item: &T,
        arena: ArenaPtr,
        scratch: &mut ConstraintsBuffers,
    ) -> bool {
        if self.extensions.is_empty() {
            return true;
        }
        item.write_file_name(arena, &mut scratch.fname);
        self.extensions
            .iter()
            .any(|ext| file_has_extension(&scratch.fname, ext))
    }
}

#[inline]
#[allow(clippy::too_many_arguments)]
fn evaluate<T: Constrainable>(
    item: &T,
    index: usize,
    constraint: &Constraint<'_>,
    glob: &GlobStrategy,
    glob_idx: &mut usize,
    negate: bool,
    arena: ArenaPtr,
    scratch: &mut ConstraintsBuffers,
) -> bool {
    let raw = match constraint {
        Constraint::Glob(_) => {
            let m = match glob {
                GlobStrategy::None => true,
                GlobStrategy::Prepass(masks) => masks
                    .get(*glob_idx)
                    .and_then(|mask| mask.get(index).copied())
                    .unwrap_or(false),
                GlobStrategy::Inline(patterns) => {
                    item.write_relative_path(arena, &mut scratch.path);
                    patterns
                        .get(*glob_idx)
                        .and_then(|p| p.as_ref())
                        .map(|p| compiled_matches(p, &scratch.path))
                        .unwrap_or(false)
                }
            };
            *glob_idx += 1;
            m
        }
        // Reachable only via `Not(Extension(_))` — bare extensions are split out
        // up front and handled in `passes_extensions`.
        Constraint::Extension(ext) => {
            item.write_file_name(arena, &mut scratch.fname);
            file_has_extension(&scratch.fname, ext)
        }
        Constraint::PathSegment(segment) => {
            item.write_relative_path(arena, &mut scratch.path);
            path_contains_segment(&scratch.path, segment)
        }
        Constraint::FilePath(suffix) => {
            item.write_relative_path(arena, &mut scratch.path);
            path_ends_with_suffix(&scratch.path, suffix)
        }
        Constraint::Text(text) => {
            // Only meaningful under negation (used as exclude filter).
            item.write_relative_path(arena, &mut scratch.path);
            find_case_insensitive_short(scratch.path.as_bytes(), text.as_bytes()).is_some()
        }
        Constraint::GitStatus(filter) => matches_git_status(item.git_status(), filter),
        Constraint::Not(inner) => {
            return evaluate(item, index, inner, glob, glob_idx, !negate, arena, scratch);
        }
        // Pass-throughs — handled at higher levels.
        Constraint::Parts(_) | Constraint::Exclude(_) | Constraint::FileType(_) => true,
    };
    if negate { !raw } else { raw }
}

#[inline]
fn matches_git_status(status: Option<git2::Status>, filter: &GitStatusFilter) -> bool {
    match (status, filter) {
        (Some(s), GitStatusFilter::Modified) => is_modified_status(s),
        (Some(s), GitStatusFilter::Untracked) => s.contains(git2::Status::WT_NEW),
        (Some(s), GitStatusFilter::Staged) => s.intersects(
            git2::Status::INDEX_NEW
                | git2::Status::INDEX_MODIFIED
                | git2::Status::INDEX_DELETED
                | git2::Status::INDEX_RENAMED
                | git2::Status::INDEX_TYPECHANGE,
        ),
        (Some(s), GitStatusFilter::Unmodified) => s.is_empty(),
        (None, GitStatusFilter::Unmodified) => true,
        (None, _) => false,
    }
}

#[inline]
#[cfg(feature = "zlob")]
pub(crate) fn compiled_matches(p: &GlobPattern, path: &str) -> bool {
    p.matches_default(path)
}

#[inline]
#[cfg(all(not(feature = "zlob"), feature = "ripgrep"))]
pub(crate) fn compiled_matches(p: &GlobPattern, path: &str) -> bool {
    p.is_match(path)
}

/// Append indices (into `rels`) of paths matching `p`, in input order.
/// zlob backend: ONE FFI call for the whole batch.
#[cfg(feature = "zlob")]
pub(crate) fn glob_matches_into(p: &GlobPattern, rels: &[&str], out: &mut Vec<usize>) {
    match p.match_indices(rels, p.flags()) {
        Ok(ix) => out.extend_from_slice(ix.as_slice()),
        Err(e) => {
            tracing::warn!(?e, "zlob batch match failed, falling back to per-path");
            out.extend((0..rels.len()).filter(|&i| p.matches_default(rels[i])));
        }
    }
}

#[cfg(all(not(feature = "zlob"), feature = "ripgrep"))]
pub(crate) fn glob_matches_into(p: &GlobPattern, rels: &[&str], out: &mut Vec<usize>) {
    out.extend((0..rels.len()).filter(|&i| p.is_match(rels[i])));
}

/// Decide between batch prepass and inline compiled patterns.
///
/// `has_pre_filter` = true when something cheaper than glob can reject items first
/// (extensions OR non-glob constraints in `rest`). In that case inline pays glob
/// cost only on survivors and beats prepass on every workload we benched. Pure-glob
/// (no pre-filter) takes prepass — single batched zlob call beats N inline matches.
fn build_glob_strategy<T: Constrainable>(
    rest: &[&Constraint<'_>],
    has_pre_filter: bool,
    items: &[T],
    arena: ArenaPtr,
    overflow_arena: ArenaPtr,
) -> GlobStrategy {
    if !contains_glob(rest) {
        return GlobStrategy::None;
    }
    if has_pre_filter {
        return GlobStrategy::Inline(compile_globs(rest));
    }
    let buf = PathBuffer::collect(items, arena, overflow_arena);
    let path_refs = buf.as_strs();
    GlobStrategy::Prepass(precompute_masks(rest, &path_refs))
}

/// `Glob` or `Not(Glob)` — the constraint kinds whose evaluation goes through
/// the GlobStrategy. Everything else can pre-reject items before glob runs.
fn is_glob_node(c: &Constraint<'_>) -> bool {
    match c {
        Constraint::Glob(_) => true,
        Constraint::Not(inner) => is_glob_node(inner),
        _ => false,
    }
}

fn contains_glob(rest: &[&Constraint<'_>]) -> bool {
    rest.iter().any(|c| is_glob_node(c))
}

/// Contiguous byte buffer holding every item's `relative_path`. Single allocation
/// instead of N `String`s. On Windows the in-place pass folds `\\` -> `/` so the
/// glob library sees a canonical separator.
struct PathBuffer {
    bytes: Vec<u8>,
    offsets: Vec<(usize, usize)>,
}

impl PathBuffer {
    fn collect<T: Constrainable>(items: &[T], arena: ArenaPtr, overflow_arena: ArenaPtr) -> Self {
        let mut bytes = Vec::<u8>::new();
        let mut offsets = Vec::with_capacity(items.len());
        let mut tmp = String::with_capacity(64);
        for item in items {
            let item_arena = if item.is_overflow() {
                overflow_arena
            } else {
                arena
            };
            let start = bytes.len();
            item.write_relative_path(item_arena, &mut tmp);
            bytes.extend_from_slice(tmp.as_bytes());
            offsets.push((start, bytes.len() - start));
        }
        Self { bytes, offsets }
    }

    fn as_strs(&self) -> Vec<&str> {
        self.offsets
            .iter()
            .map(|&(off, len)| unsafe {
                std::str::from_utf8_unchecked(&self.bytes[off..off + len])
            })
            .collect()
    }
}

fn precompute_masks(rest: &[&Constraint<'_>], paths: &[&str]) -> Vec<Vec<bool>> {
    let mut out = Vec::new();
    for c in rest {
        walk_globs(c, &mut |pattern| {
            out.push(match_glob_pattern(pattern, paths))
        });
    }
    out
}

fn compile_globs(rest: &[&Constraint<'_>]) -> Vec<Option<GlobPattern>> {
    let mut out = Vec::new();
    for c in rest {
        walk_globs(c, &mut |pattern| out.push(compile_one(pattern)));
    }
    out
}

/// Visit every Glob (including ones nested under Not) in constraint walk order.
/// Order matters: `glob_idx` in the per-item evaluator increments by one per Glob node.
fn walk_globs<F: FnMut(&str)>(c: &Constraint<'_>, f: &mut F) {
    match c {
        Constraint::Glob(p) => f(p),
        Constraint::Not(inner) => walk_globs(inner, f),
        _ => {}
    }
}

#[cfg(feature = "zlob")]
pub(crate) fn compile_one(pattern: &str) -> Option<GlobPattern> {
    zlob::ZlobPattern::compile(pattern, GLOB_FLAGS).ok()
}

#[cfg(all(not(feature = "zlob"), feature = "ripgrep"))]
pub(crate) fn compile_one(pattern: &str) -> Option<GlobPattern> {
    globset::Glob::new(pattern)
        .ok()
        .map(|g| g.compile_matcher())
}

/// Build a `paths.len()`-sized bitmap. Vec<bool> beats AHashSet ~2× in the per-item
/// filter loop — no hashing, plain array indexing, sequential prefetcher-friendly.
#[cfg(feature = "zlob")]
fn match_glob_pattern(pattern: &str, paths: &[&str]) -> Vec<bool> {
    let mut mask = vec![false; paths.len()];
    let Ok(hits) = zlob::zlob_match_paths_indices(pattern, paths, GLOB_FLAGS) else {
        return mask;
    };
    for i in hits.to_iter() {
        if i < mask.len() {
            mask[i] = true;
        }
    }
    mask
}

#[cfg(all(not(feature = "zlob"), feature = "ripgrep"))]
fn match_glob_pattern(pattern: &str, paths: &[&str]) -> Vec<bool> {
    let mut mask = vec![false; paths.len()];
    let Ok(glob) = globset::Glob::new(pattern) else {
        return mask;
    };
    let matcher = glob.compile_matcher();
    if paths.len() >= PAR_THRESHOLD {
        use rayon::prelude::*;
        mask.par_iter_mut()
            .zip(paths.par_iter())
            .for_each(|(slot, p)| *slot = matcher.is_match(p));
    } else {
        for (slot, p) in mask.iter_mut().zip(paths.iter()) {
            *slot = matcher.is_match(p);
        }
    }
    mask
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
    struct TestItem {
        relative_path: &'static str,
        file_name: &'static str,
    }

    impl Constrainable for TestItem {
        fn write_file_name(&self, _arena: ArenaPtr, out: &mut String) {
            out.clear();
            out.push_str(self.file_name);
        }

        fn write_relative_path(&self, _arena: ArenaPtr, out: &mut String) {
            out.clear();
            out.push_str(self.relative_path);
        }

        fn git_status(&self) -> Option<git2::Status> {
            None
        }

        fn is_overflow(&self) -> bool {
            false
        }
    }

    #[test]
    fn test_file_has_extension() {
        assert!(file_has_extension("file.rs", "rs"));
        assert!(file_has_extension("file.RS", "rs")); // case-insensitive
        assert!(file_has_extension("file.test.rs", "rs"));
        assert!(file_has_extension("a.rs", "rs"));

        assert!(!file_has_extension("file.tsx", "rs"));
        assert!(!file_has_extension("rs", "rs")); // too short
        assert!(!file_has_extension(".rs", "rs")); // just extension
        assert!(!file_has_extension("file.rsx", "rs")); // different extension
        assert!(!file_has_extension("filers", "rs")); // no dot
    }

    #[test]
    fn test_path_contains_segment() {
        // Segment at start
        assert!(path_contains_segment("src/lib.rs", "src"));
        assert!(path_contains_segment("SRC/lib.rs", "src")); // case-insensitive

        // Segment in middle
        assert!(path_contains_segment("app/src/lib.rs", "src"));
        assert!(path_contains_segment("app/SRC/lib.rs", "src"));

        // Multiple levels
        assert!(path_contains_segment("core/workflow/src/main.rs", "src"));
        assert!(path_contains_segment(
            "core/workflow/src/main.rs",
            "workflow"
        ));
        assert!(path_contains_segment("core/workflow/src/main.rs", "core"));

        // Should not match partial segments
        assert!(!path_contains_segment("source/lib.rs", "src"));
        assert!(!path_contains_segment("mysrc/lib.rs", "src"));

        // Should not match filename
        assert!(!path_contains_segment("lib/src", "src"));

        // Multi-segment constraints
        assert!(path_contains_segment(
            "libswscale/aarch64/input.S",
            "libswscale/aarch64"
        ));
        assert!(path_contains_segment(
            "foo/libswscale/aarch64/input.S",
            "libswscale/aarch64"
        ));
        assert!(path_contains_segment(
            "foo/LibSwscale/AArch64/input.S",
            "libswscale/aarch64"
        )); // case-insensitive
        assert!(!path_contains_segment(
            "xlibswscale/aarch64/input.S",
            "libswscale/aarch64"
        )); // partial match at start
        assert!(!path_contains_segment(
            "foo/libswscale/aarch64x/input.S",
            "libswscale/aarch64"
        )); // partial match at end
        assert!(path_contains_segment(
            "crates/fff-core/src/grep.rs",
            "fff-core/src"
        ));

        // Edge cases
        assert!(!path_contains_segment("", "src"));
        assert!(!path_contains_segment("src", "src")); // no trailing slash
    }

    #[cfg(windows)]
    #[test]
    fn test_path_contains_segment_accepts_backslash() {
        assert!(path_contains_segment("src\\lib.rs", "src"));
        assert!(path_contains_segment(
            "app\\modules\\src\\services\\x.lua",
            "src"
        ));
        assert!(path_contains_segment("app\\SRC\\x.lua", "src"));

        assert!(path_contains_segment(
            "foo\\libswscale\\aarch64\\input.S",
            "libswscale/aarch64"
        ));
        assert!(path_contains_segment(
            "crates\\fff-core\\src\\grep.rs",
            "fff-core/src"
        ));

        assert!(!path_contains_segment("mysrc\\lib.rs", "src"));
        assert!(!path_contains_segment(
            "xlibswscale\\aarch64\\in.S",
            "libswscale/aarch64"
        ));
    }

    #[test]
    fn test_path_ends_with_suffix() {
        // Exact match
        assert!(path_ends_with_suffix(
            "libswscale/input.c",
            "libswscale/input.c"
        ));

        // Suffix match at / boundary
        assert!(path_ends_with_suffix(
            "foo/libswscale/input.c",
            "libswscale/input.c"
        ));

        // Deep nesting
        assert!(path_ends_with_suffix(
            "a/b/c/libswscale/input.c",
            "libswscale/input.c"
        ));

        // No boundary — partial directory name
        assert!(!path_ends_with_suffix(
            "xlibswscale/input.c",
            "libswscale/input.c"
        ));

        // Case insensitive
        assert!(path_ends_with_suffix(
            "foo/LibSwscale/Input.C",
            "libswscale/input.c"
        ));

        // Single file name
        assert!(path_ends_with_suffix("input.c", "input.c"));
        assert!(!path_ends_with_suffix("xinput.c", "input.c"));

        // Suffix longer than path
        assert!(!path_ends_with_suffix("input.c", "foo/input.c"));

        // Simple path
        assert!(path_ends_with_suffix("src/main.rs", "src/main.rs"));
        assert!(path_ends_with_suffix("crates/src/main.rs", "src/main.rs"));
    }

    #[cfg(windows)]
    #[test]
    fn test_path_ends_with_suffix_accepts_backslash() {
        assert!(path_ends_with_suffix(
            "app\\modules\\src\\services\\handler.lua",
            "services/handler.lua"
        ));
        assert!(path_ends_with_suffix(
            "foo\\libswscale\\input.c",
            "libswscale/input.c"
        ));
        assert!(!path_ends_with_suffix(
            "xlibswscale\\input.c",
            "libswscale/input.c"
        ));
    }

    #[test]
    fn test_path_ends_with_suffix_does_not_panic_on_unicode_suffix() {
        assert!(!path_ends_with_suffix("유니코드_파일_테스트.csv", "트.c"));
        assert!(path_ends_with_suffix(
            "data/유니코드_파일_테스트.csv",
            "유니코드_파일_테스트.csv"
        ));
    }

    #[test]
    fn test_path_ends_with_suffix_unicode_apostrophe_mismatch() {
        assert!(!path_ends_with_suffix(
            "dir/\u{2019}bar/file.txt",
            "'bar/file.txt"
        ));
    }

    #[test]
    fn test_path_ends_with_suffix_unicode_space_mismatch() {
        assert!(!path_ends_with_suffix(
            "dir/\u{202f}am/file.txt",
            " am/file.txt"
        ));
    }

    #[test]
    fn test_path_contains_segment_does_not_panic_on_unicode_segment() {
        assert!(!path_contains_segment("문서/notes.txt", "문x"));
        assert!(path_contains_segment("프로젝트/문서/notes.txt", "문서"));
    }

    #[test]
    fn test_path_contains_segment_unicode_no_panic() {
        assert!(!path_contains_segment(
            "Library/Cloud/Project\u{2019}s Folder/books.ttl",
            "Project's Folder"
        ));
    }

    #[test]
    fn test_file_has_extension_unicode_no_panic() {
        assert!(!file_has_extension("cat\u{00e9}.rs", "s"));
    }

    #[test]
    fn test_file_has_extension_unicode_filename() {
        assert!(file_has_extension("운영-가이드.md", "md"));
        assert!(file_has_extension("테스트.csv", "csv"));
        assert!(!file_has_extension("테스트.csv", "md"));
    }

    #[test]
    fn test_apply_constraints_file_path_with_unicode_suffix() {
        let arena_ptr = ArenaPtr::null();

        let item = TestItem {
            relative_path: "data/유니코드_파일_테스트.csv",
            file_name: "유니코드_파일_테스트.csv",
        };

        let exact = [Constraint::FilePath("유니코드_파일_테스트.csv")];
        let mismatch = [Constraint::FilePath("트.c")];

        let exact_items = [item.clone()];
        let exact_matches = apply_constraints(&exact_items, &exact, arena_ptr, arena_ptr)
            .expect("constraints applied");
        assert_eq!(exact_matches.len(), 1);

        let mismatch_items = [item];
        let mismatch_matches = apply_constraints(&mismatch_items, &mismatch, arena_ptr, arena_ptr)
            .expect("constraints applied");
        assert!(mismatch_matches.is_empty());
    }

    #[test]
    fn test_unicode_path_no_panic_real_korean_cases() {
        // Real Korean paths that caused panics
        let path1 = "Downloads/(커리큘럼) hermes agent_정승현님 - 1차 커리큘럼 (강사님 작성).csv";
        let path2 = "hermes-agent-lecture-materials/세부_커리큘럼_최종.csv";
        let path3 = "projects/fastcampus-hermes-agent-curriculum/chapters/part-02-Hermes-설치-및-기본-사용/section-02-doctor로-설치-상태-검증/research/03-fix가-자동-수정하는-것과-못하는-것.md";

        // These must not panic regardless of segment/suffix used
        assert!(!path_contains_segment(path1, "작성"));
        assert!(!path_ends_with_suffix(path1, "작성.csv"));
        assert!(!path_contains_segment(path2, "최종"));
        assert!(!path_ends_with_suffix(path2, "최종.csv"));
        assert!(!path_contains_segment(path3, "수정"));
        assert!(!path_ends_with_suffix(path3, "것.md"));

        // Positive cases should still work
        assert!(path_contains_segment(
            path2,
            "hermes-agent-lecture-materials"
        ));
        assert!(path_ends_with_suffix(
            path1,
            "(커리큘럼) hermes agent_정승현님 - 1차 커리큘럼 (강사님 작성).csv"
        ));
        assert!(path_ends_with_suffix(path2, "세부_커리큘럼_최종.csv"));
    }

    #[test]
    fn test_negated_glob_excludes_matching_files() {
        let arena_ptr = ArenaPtr::null();

        let items = vec![
            TestItem {
                relative_path: "src/main.rs",
                file_name: "main.rs",
            },
            TestItem {
                relative_path: "src/lib.ts",
                file_name: "lib.ts",
            },
            TestItem {
                relative_path: "include/fff.h",
                file_name: "fff.h",
            },
        ];

        // Not(Glob("**/*.rs")) should exclude .rs files
        let constraints = vec![Constraint::Not(Box::new(Constraint::Glob("**/*.rs")))];
        let result = apply_constraints(&items, &constraints, arena_ptr, arena_ptr).unwrap();
        let paths: Vec<&str> = result.iter().map(|i| i.relative_path).collect();
        assert!(
            !paths.contains(&"src/main.rs"),
            "rs file should be excluded"
        );
        assert!(paths.contains(&"src/lib.ts"), "ts file should be included");
        assert!(
            paths.contains(&"include/fff.h"),
            "h file should be included"
        );
    }

    #[test]
    fn test_inline_glob_path_matches_prepass() {
        // Mixed (extensions + glob) takes the inline-compiled path.
        // Pure glob takes the prepass bitmap path. Both must give identical results.
        let arena_ptr = ArenaPtr::null();
        let items = vec![
            TestItem {
                relative_path: "src/main.rs",
                file_name: "main.rs",
            },
            TestItem {
                relative_path: "src/lib.ts",
                file_name: "lib.ts",
            },
            TestItem {
                relative_path: "tests/foo.rs",
                file_name: "foo.rs",
            },
            TestItem {
                relative_path: "docs/readme.md",
                file_name: "readme.md",
            },
        ];

        let mixed = vec![Constraint::Extension("rs"), Constraint::Glob("src/**")];
        let mixed_paths: Vec<&str> = apply_constraints(&items, &mixed, arena_ptr, arena_ptr)
            .unwrap()
            .iter()
            .map(|i| i.relative_path)
            .collect();
        assert_eq!(mixed_paths, vec!["src/main.rs"]);

        let pure_glob = vec![Constraint::Glob("src/**")];
        let glob_paths: Vec<&str> = apply_constraints(&items, &pure_glob, arena_ptr, arena_ptr)
            .unwrap()
            .iter()
            .map(|i| i.relative_path)
            .collect();
        assert!(glob_paths.contains(&"src/main.rs"));
        assert!(glob_paths.contains(&"src/lib.ts"));
        assert_eq!(glob_paths.len(), 2);
    }

    #[test]
    fn test_inline_negated_glob_with_extension() {
        // Mixed Not(Glob) on inline path — exercise the negate=true branch in
        // glob_matches_inline through the Not->Glob recursion.
        let arena_ptr = ArenaPtr::null();
        let items = vec![
            TestItem {
                relative_path: "src/main.rs",
                file_name: "main.rs",
            },
            TestItem {
                relative_path: "vendor/foo.rs",
                file_name: "foo.rs",
            },
            TestItem {
                relative_path: "vendor/foo.ts",
                file_name: "foo.ts",
            },
        ];

        let constraints = vec![
            Constraint::Extension("rs"),
            Constraint::Not(Box::new(Constraint::Glob("vendor/**"))),
        ];
        let paths: Vec<&str> = apply_constraints(&items, &constraints, arena_ptr, arena_ptr)
            .unwrap()
            .iter()
            .map(|i| i.relative_path)
            .collect();
        assert_eq!(paths, vec!["src/main.rs"]);
    }
}
