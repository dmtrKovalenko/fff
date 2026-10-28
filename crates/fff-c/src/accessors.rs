//! Stable accessor functions for `fff-c` FFI struct fields: a named API so
//! FFI callers (Emacs Lisp, Python `ctypes`, etc.) don't hardcode struct byte
//! offsets that break silently on layout changes. For array iteration use
//! `fff_search_result_get_item` / `fff_grep_result_get_match` in `lib.rs`.

use std::ffi::c_char;
use std::ptr;

use crate::ffi_types::{
    FffFileItem, FffGrepMatch, FffGrepResult, FffMatchRange, FffResult, FffScore, FffSearchResult,
};

// ── FffResult ────────────────────────────────────────────────────────────────

/// Returns whether the operation completed successfully. Returns `false` if `result` is null.
///
/// ## Safety
/// `result` must be a valid `FffResult` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_result_get_success(result: *const FffResult) -> bool {
    if result.is_null() {
        return false;
    }
    unsafe { (*result).success }
}

/// Returns the operation error message, or null when there is no error or `result` is null.
///
/// Do not free the returned pointer. It remains valid until `fff_free_result` is called.
///
/// ## Safety
/// `result` must be a valid `FffResult` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_result_get_error(result: *const FffResult) -> *const c_char {
    if result.is_null() {
        return ptr::null();
    }
    unsafe { (*result).error }
}

/// Returns the result payload handle, or null if `result` is null.
///
/// ## Safety
/// `result` must be a valid `FffResult` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_result_get_handle(result: *const FffResult) -> *mut std::ffi::c_void {
    if result.is_null() {
        return ptr::null_mut();
    }
    unsafe { (*result).handle }
}

/// Returns the result integer payload. Returns `0` if `result` is null.
///
/// ## Safety
/// `result` must be a valid `FffResult` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_result_get_int_value(result: *const FffResult) -> i64 {
    if result.is_null() {
        return 0;
    }
    unsafe { (*result).int_value }
}

// ── FffFileItem ──────────────────────────────────────────────────────────────

/// Relative path of a file item (e.g. `"src/main.rs"`); null if `item` is null. Do not free.
///
/// ## Safety
/// `item` must be a valid `FffFileItem` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_file_item_get_relative_path(
    item: *const FffFileItem,
) -> *const c_char {
    if item.is_null() {
        return ptr::null();
    }
    unsafe { (*item).relative_path }
}

/// File-name component of a file item (e.g. `"main.rs"`); null if `item` is null. Do not free.
///
/// ## Safety
/// `item` must be a valid `FffFileItem` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_file_item_get_file_name(item: *const FffFileItem) -> *const c_char {
    if item.is_null() {
        return ptr::null();
    }
    unsafe { (*item).file_name }
}

/// Git status string of a file item (e.g. `"M "`, `"??"`); null if git is unavailable,
/// the file is untracked, or `item` is null. Do not free.
///
/// ## Safety
/// `item` must be a valid `FffFileItem` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_file_item_get_git_status(item: *const FffFileItem) -> *const c_char {
    if item.is_null() {
        return ptr::null();
    }
    unsafe { (*item).git_status }
}

/// File size in bytes; `0` if `item` is null.
///
/// ## Safety
/// `item` must be a valid `FffFileItem` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_file_item_get_size(item: *const FffFileItem) -> u64 {
    if item.is_null() {
        return 0;
    }
    unsafe { (*item).size }
}

/// Last-modified time as seconds since the UNIX epoch; `0` if `item` is null.
///
/// ## Safety
/// `item` must be a valid `FffFileItem` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_file_item_get_modified(item: *const FffFileItem) -> u64 {
    if item.is_null() {
        return 0;
    }
    unsafe { (*item).modified }
}

/// Combined frecency score; `0` if `item` is null.
///
/// ## Safety
/// `item` must be a valid `FffFileItem` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_file_item_get_total_frecency_score(item: *const FffFileItem) -> i64 {
    if item.is_null() {
        return 0;
    }
    unsafe { (*item).total_frecency_score }
}

/// Access-based frecency score; `0` if `item` is null.
///
/// ## Safety
/// `item` must be a valid `FffFileItem` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_file_item_get_access_frecency_score(item: *const FffFileItem) -> i64 {
    if item.is_null() {
        return 0;
    }
    unsafe { (*item).access_frecency_score }
}

/// Modification-based frecency score; `0` if `item` is null.
///
/// ## Safety
/// `item` must be a valid `FffFileItem` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_file_item_get_modification_frecency_score(
    item: *const FffFileItem,
) -> i64 {
    if item.is_null() {
        return 0;
    }
    unsafe { (*item).modification_frecency_score }
}

/// `true` if the file was detected as binary; `false` if `item` is null.
///
/// ## Safety
/// `item` must be a valid `FffFileItem` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_file_item_get_is_binary(item: *const FffFileItem) -> bool {
    if item.is_null() {
        return false;
    }
    unsafe { (*item).is_binary }
}

// ── FffGrepMatch ─────────────────────────────────────────────────────────────

/// Relative path of the file containing this grep match; null if `m` is null. Do not free.
///
/// ## Safety
/// `m` must be a valid `FffGrepMatch` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_grep_match_get_relative_path(m: *const FffGrepMatch) -> *const c_char {
    if m.is_null() {
        return ptr::null();
    }
    unsafe { (*m).relative_path }
}

/// File-name component of the file containing this grep match; null if `m` is null. Do not free.
///
/// ## Safety
/// `m` must be a valid `FffGrepMatch` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_grep_match_get_file_name(m: *const FffGrepMatch) -> *const c_char {
    if m.is_null() {
        return ptr::null();
    }
    unsafe { (*m).file_name }
}

/// Git status string of the matched file (e.g. `"M "`, `"??"`); null if git is unavailable,
/// the file is untracked, or `m` is null. Do not free.
///
/// ## Safety
/// `m` must be a valid `FffGrepMatch` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_grep_match_get_git_status(m: *const FffGrepMatch) -> *const c_char {
    if m.is_null() {
        return ptr::null();
    }
    unsafe { (*m).git_status }
}

/// Full text content of the matched line; null if `m` is null. Do not free.
///
/// ## Safety
/// `m` must be a valid `FffGrepMatch` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_grep_match_get_line_content(m: *const FffGrepMatch) -> *const c_char {
    if m.is_null() {
        return ptr::null();
    }
    unsafe { (*m).line_content }
}

/// 1-based line number of the match within its file; `0` if `m` is null.
///
/// ## Safety
/// `m` must be a valid `FffGrepMatch` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_grep_match_get_line_number(m: *const FffGrepMatch) -> u64 {
    if m.is_null() {
        return 0;
    }
    unsafe { (*m).line_number }
}

/// 0-based column of the match start within its line; `0` if `m` is null.
///
/// ## Safety
/// `m` must be a valid `FffGrepMatch` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_grep_match_get_col(m: *const FffGrepMatch) -> u32 {
    if m.is_null() {
        return 0;
    }
    unsafe { (*m).col }
}

/// Byte offset of the match start from the beginning of the file; `0` if `m` is null.
///
/// ## Safety
/// `m` must be a valid `FffGrepMatch` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_grep_match_get_byte_offset(m: *const FffGrepMatch) -> u64 {
    if m.is_null() {
        return 0;
    }
    unsafe { (*m).byte_offset }
}

/// File size in bytes of the matched file; `0` if `m` is null.
///
/// ## Safety
/// `m` must be a valid `FffGrepMatch` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_grep_match_get_size(m: *const FffGrepMatch) -> u64 {
    if m.is_null() {
        return 0;
    }
    unsafe { (*m).size }
}

/// Combined frecency score of the matched file; `0` if `m` is null.
///
/// ## Safety
/// `m` must be a valid `FffGrepMatch` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_grep_match_get_total_frecency_score(m: *const FffGrepMatch) -> i64 {
    if m.is_null() {
        return 0;
    }
    unsafe { (*m).total_frecency_score }
}

/// Access-based frecency score of the matched file; `0` if `m` is null.
///
/// ## Safety
/// `m` must be a valid `FffGrepMatch` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_grep_match_get_access_frecency_score(m: *const FffGrepMatch) -> i64 {
    if m.is_null() {
        return 0;
    }
    unsafe { (*m).access_frecency_score }
}

/// Modification-based frecency score of the matched file; `0` if `m` is null.
///
/// ## Safety
/// `m` must be a valid `FffGrepMatch` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_grep_match_get_modification_frecency_score(
    m: *const FffGrepMatch,
) -> i64 {
    if m.is_null() {
        return 0;
    }
    unsafe { (*m).modification_frecency_score }
}

/// Last-modified time of the matched file as seconds since the UNIX epoch; `0` if `m` is null.
///
/// ## Safety
/// `m` must be a valid `FffGrepMatch` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_grep_match_get_modified(m: *const FffGrepMatch) -> u64 {
    if m.is_null() {
        return 0;
    }
    unsafe { (*m).modified }
}

/// Number of highlight ranges in this match; `0` if `m` is null.
/// Use with [`fff_grep_match_get_match_range`] to iterate the highlight spans.
///
/// ## Safety
/// `m` must be a valid `FffGrepMatch` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_grep_match_get_match_ranges_count(m: *const FffGrepMatch) -> u32 {
    if m.is_null() {
        return 0;
    }
    unsafe { (*m).match_ranges_count }
}

/// Pointer to the `index`-th [`FffMatchRange`] highlight span; null if `m` is null,
/// `index >= match_ranges_count`, or the ranges array is null. Valid until the owning `FffGrepResult` is freed; do not free.
///
/// ## Safety
/// `m` must be a valid `FffGrepMatch` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_grep_match_get_match_range(
    m: *const FffGrepMatch,
    index: u32,
) -> *const FffMatchRange {
    if m.is_null() {
        return ptr::null();
    }
    let m = unsafe { &*m };
    if index >= m.match_ranges_count || m.match_ranges.is_null() {
        return ptr::null();
    }
    unsafe { m.match_ranges.add(index as usize) }
}

/// Number of context lines captured before the match; `0` if `m` is null.
/// Use with [`fff_grep_match_get_context_before`] to read each line.
///
/// ## Safety
/// `m` must be a valid `FffGrepMatch` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_grep_match_get_context_before_count(m: *const FffGrepMatch) -> u32 {
    if m.is_null() {
        return 0;
    }
    unsafe { (*m).context_before_count }
}

/// The `index`-th context line before the match; null if `m` is null,
/// `index >= context_before_count`, or the context array is null. Do not free.
///
/// ## Safety
/// `m` must be a valid `FffGrepMatch` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_grep_match_get_context_before(
    m: *const FffGrepMatch,
    index: u32,
) -> *const c_char {
    if m.is_null() {
        return ptr::null();
    }
    let m = unsafe { &*m };
    if index >= m.context_before_count || m.context_before.is_null() {
        return ptr::null();
    }
    unsafe { *m.context_before.add(index as usize) }
}

/// Number of context lines captured after the match; `0` if `m` is null.
/// Use with [`fff_grep_match_get_context_after`] to read each line.
///
/// ## Safety
/// `m` must be a valid `FffGrepMatch` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_grep_match_get_context_after_count(m: *const FffGrepMatch) -> u32 {
    if m.is_null() {
        return 0;
    }
    unsafe { (*m).context_after_count }
}

/// The `index`-th context line after the match; null if `m` is null,
/// `index >= context_after_count`, or the context array is null. Do not free.
///
/// ## Safety
/// `m` must be a valid `FffGrepMatch` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_grep_match_get_context_after(
    m: *const FffGrepMatch,
    index: u32,
) -> *const c_char {
    if m.is_null() {
        return ptr::null();
    }
    let m = unsafe { &*m };
    if index >= m.context_after_count || m.context_after.is_null() {
        return ptr::null();
    }
    unsafe { *m.context_after.add(index as usize) }
}

/// Fuzzy match score; `0` if `m` is null or no fuzzy score is present.
/// Always check [`fff_grep_match_get_has_fuzzy_score`] first; `0` is ambiguous without that flag.
///
/// ## Safety
/// `m` must be a valid `FffGrepMatch` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_grep_match_get_fuzzy_score(m: *const FffGrepMatch) -> u16 {
    if m.is_null() {
        return 0;
    }
    unsafe { (*m).fuzzy_score }
}

/// `true` if this match carries a valid fuzzy score; `false` if `m` is null.
///
/// ## Safety
/// `m` must be a valid `FffGrepMatch` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_grep_match_get_has_fuzzy_score(m: *const FffGrepMatch) -> bool {
    if m.is_null() {
        return false;
    }
    unsafe { (*m).has_fuzzy_score }
}

/// `true` if the match was identified as a symbol definition; `false` if `m` is null.
///
/// ## Safety
/// `m` must be a valid `FffGrepMatch` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_grep_match_get_is_definition(m: *const FffGrepMatch) -> bool {
    if m.is_null() {
        return false;
    }
    unsafe { (*m).is_definition }
}

/// `true` if the matched file was detected as binary; `false` if `m` is null.
///
/// ## Safety
/// `m` must be a valid `FffGrepMatch` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_grep_match_get_is_binary(m: *const FffGrepMatch) -> bool {
    if m.is_null() {
        return false;
    }
    unsafe { (*m).is_binary }
}

// ── FffScore ─────────────────────────────────────────────────────────────────

/// Final combined score used for ranking. Returns `0` if `s` is null.
///
/// ## Safety
/// `s` must be a valid `FffScore` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_score_get_total(s: *const FffScore) -> i32 {
    if s.is_null() {
        return 0;
    }
    unsafe { (*s).total }
}

/// Raw fuzzy match score before bonuses and penalties. Returns `0` if `s` is null.
///
/// ## Safety
/// `s` must be a valid `FffScore` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_score_get_base_score(s: *const FffScore) -> i32 {
    if s.is_null() {
        return 0;
    }
    unsafe { (*s).base_score }
}

/// Bonus for matching the file name rather than only the path. Returns `0` if `s` is null.
///
/// ## Safety
/// `s` must be a valid `FffScore` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_score_get_filename_bonus(s: *const FffScore) -> i32 {
    if s.is_null() {
        return 0;
    }
    unsafe { (*s).filename_bonus }
}

/// Bonus for special file names (e.g. `README`, `main.rs`). Returns `0` if `s` is null.
///
/// ## Safety
/// `s` must be a valid `FffScore` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_score_get_special_filename_bonus(s: *const FffScore) -> i32 {
    if s.is_null() {
        return 0;
    }
    unsafe { (*s).special_filename_bonus }
}

/// Boost from the frecency database. Returns `0` if `s` is null.
///
/// ## Safety
/// `s` must be a valid `FffScore` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_score_get_frecency_boost(s: *const FffScore) -> i32 {
    if s.is_null() {
        return 0;
    }
    unsafe { (*s).frecency_boost }
}

/// Penalty based on directory distance from the current file. Returns `0` if `s` is null.
///
/// ## Safety
/// `s` must be a valid `FffScore` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_score_get_distance_penalty(s: *const FffScore) -> i32 {
    if s.is_null() {
        return 0;
    }
    unsafe { (*s).distance_penalty }
}

/// Penalty applied to the currently open file. Returns `0` if `s` is null.
///
/// ## Safety
/// `s` must be a valid `FffScore` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_score_get_current_file_penalty(s: *const FffScore) -> i32 {
    if s.is_null() {
        return 0;
    }
    unsafe { (*s).current_file_penalty }
}

/// Boost when several scoring signals agree. Returns `0` if `s` is null.
///
/// ## Safety
/// `s` must be a valid `FffScore` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_score_get_combo_match_boost(s: *const FffScore) -> i32 {
    if s.is_null() {
        return 0;
    }
    unsafe { (*s).combo_match_boost }
}

/// Bonus for query segments aligning with path segments. Returns `0` if `s` is null.
///
/// ## Safety
/// `s` must be a valid `FffScore` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_score_get_path_alignment_bonus(s: *const FffScore) -> i32 {
    if s.is_null() {
        return 0;
    }
    unsafe { (*s).path_alignment_bonus }
}

/// `true` if the query matched the file name or directory exactly; `false` if `s` is null.
///
/// ## Safety
/// `s` must be a valid `FffScore` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_score_get_exact_match(s: *const FffScore) -> bool {
    if s.is_null() {
        return false;
    }
    unsafe { (*s).exact_match }
}

/// Match type label (e.g. `"exact_filename"`, `"fuzzy_path"`); null if `s` is null. Do not free.
///
/// ## Safety
/// `s` must be a valid `FffScore` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_score_get_match_type(s: *const FffScore) -> *const c_char {
    if s.is_null() {
        return ptr::null();
    }
    unsafe { (*s).match_type }
}

// ── FffSearchResult ──────────────────────────────────────────────────────────

/// Number of items in the result; `0` if `r` is null.
///
/// ## Safety
/// `r` must be a valid `FffSearchResult` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_search_result_get_count(r: *const FffSearchResult) -> u32 {
    if r.is_null() {
        return 0;
    }
    unsafe { (*r).count }
}

/// Total number of files that matched before truncation to the page size; `0` if `r` is null.
///
/// ## Safety
/// `r` must be a valid `FffSearchResult` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_search_result_get_total_matched(r: *const FffSearchResult) -> u32 {
    if r.is_null() {
        return 0;
    }
    unsafe { (*r).total_matched }
}

/// Total number of indexed files considered during search; `0` if `r` is null.
///
/// ## Safety
/// `r` must be a valid `FffSearchResult` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_search_result_get_total_files(r: *const FffSearchResult) -> u32 {
    if r.is_null() {
        return 0;
    }
    unsafe { (*r).total_files }
}

// ── FffGrepResult ─────────────────────────────────────────────────────────────

/// Number of matches in the result; `0` if `r` is null.
///
/// ## Safety
/// `r` must be a valid `FffGrepResult` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_grep_result_get_count(r: *const FffGrepResult) -> u32 {
    if r.is_null() {
        return 0;
    }
    unsafe { (*r).count }
}

/// Total number of matches found across all pages; `0` if `r` is null.
///
/// ## Safety
/// `r` must be a valid `FffGrepResult` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_grep_result_get_total_matched(r: *const FffGrepResult) -> u32 {
    if r.is_null() {
        return 0;
    }
    unsafe { (*r).total_matched }
}

/// Number of files actually opened and searched in this call; `0` if `r` is null.
///
/// ## Safety
/// `r` must be a valid `FffGrepResult` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_grep_result_get_total_files_searched(r: *const FffGrepResult) -> u32 {
    if r.is_null() {
        return 0;
    }
    unsafe { (*r).total_files_searched }
}

/// Total number of indexed files before any filtering; `0` if `r` is null.
///
/// ## Safety
/// `r` must be a valid `FffGrepResult` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_grep_result_get_total_files(r: *const FffGrepResult) -> u32 {
    if r.is_null() {
        return 0;
    }
    unsafe { (*r).total_files }
}

/// Number of files eligible for search after path/type filtering; `0` if `r` is null.
///
/// ## Safety
/// `r` must be a valid `FffGrepResult` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_grep_result_get_filtered_file_count(r: *const FffGrepResult) -> u32 {
    if r.is_null() {
        return 0;
    }
    unsafe { (*r).filtered_file_count }
}

/// File offset for the next page; `0` if all files have been searched or `r` is null.
/// Pass as `file_offset` to a subsequent `fff_live_grep`/`fff_multi_grep` call to continue pagination.
///
/// ## Safety
/// `r` must be a valid `FffGrepResult` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_grep_result_get_next_file_offset(r: *const FffGrepResult) -> u32 {
    if r.is_null() {
        return 0;
    }
    unsafe { (*r).next_file_offset }
}

/// Regex compilation error string if the engine fell back to literal matching;
/// null if there was no error or `r` is null. Do not free.
///
/// ## Safety
/// `r` must be a valid `FffGrepResult` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_grep_result_get_regex_fallback_error(
    r: *const FffGrepResult,
) -> *const c_char {
    if r.is_null() {
        return ptr::null();
    }
    unsafe { (*r).regex_fallback_error }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;
    use std::ptr;

    // ── helpers ──────────────────────────────────────────────────────────────

    fn make_file_item(path: &str, name: &str) -> FffFileItem {
        FffFileItem {
            relative_path: CString::new(path).unwrap().into_raw(),
            file_name: CString::new(name).unwrap().into_raw(),
            git_status: ptr::null_mut(),
            size: 1024,
            modified: 1_700_000_000,
            access_frecency_score: 10,
            modification_frecency_score: 20,
            total_frecency_score: 30,
            is_binary: false,
        }
    }

    unsafe fn free_file_item(item: &mut FffFileItem) {
        unsafe {
            if !item.relative_path.is_null() {
                drop(CString::from_raw(item.relative_path));
            }
            if !item.file_name.is_null() {
                drop(CString::from_raw(item.file_name));
            }
            if !item.git_status.is_null() {
                drop(CString::from_raw(item.git_status));
            }
        }
    }

    fn make_grep_match(path: &str, line: &str) -> FffGrepMatch {
        FffGrepMatch {
            relative_path: CString::new(path).unwrap().into_raw(),
            file_name: CString::new("file.rs").unwrap().into_raw(),
            git_status: ptr::null_mut(),
            line_content: CString::new(line).unwrap().into_raw(),
            match_ranges: ptr::null_mut(),
            context_before: ptr::null_mut(),
            context_after: ptr::null_mut(),
            size: 512,
            modified: 1_600_000_000,
            total_frecency_score: 5,
            access_frecency_score: 6,
            modification_frecency_score: 7,
            line_number: 42,
            byte_offset: 100,
            col: 8,
            match_ranges_count: 0,
            context_before_count: 0,
            context_after_count: 0,
            fuzzy_score: 0,
            has_fuzzy_score: false,
            is_binary: false,
            is_definition: true,
        }
    }

    unsafe fn free_grep_match(m: &mut FffGrepMatch) {
        unsafe {
            if !m.relative_path.is_null() {
                drop(CString::from_raw(m.relative_path));
            }
            if !m.file_name.is_null() {
                drop(CString::from_raw(m.file_name));
            }
            if !m.line_content.is_null() {
                drop(CString::from_raw(m.line_content));
            }
        }
    }

    fn make_search_result(count: u32, total: u32, files: u32) -> FffSearchResult {
        FffSearchResult {
            items: ptr::null_mut(),
            scores: ptr::null_mut(),
            count,
            total_matched: total,
            total_files: files,
            location: crate::ffi_types::FffLocation {
                tag: 0,
                line: 0,
                col: 0,
                end_line: 0,
                end_col: 0,
            },
        }
    }

    fn make_grep_result() -> FffGrepResult {
        FffGrepResult {
            items: ptr::null_mut(),
            count: 3,
            total_matched: 10,
            total_files_searched: 50,
            total_files: 200,
            filtered_file_count: 80,
            next_file_offset: 51,
            regex_fallback_error: ptr::null_mut(),
        }
    }

    // ── null-guard tests: every function returns its zero-value on NULL ───────

    #[test]
    fn null_file_item_returns_null_or_zero() {
        let null: *const FffFileItem = ptr::null();
        unsafe {
            assert!(fff_file_item_get_relative_path(null).is_null());
            assert!(fff_file_item_get_file_name(null).is_null());
            assert!(fff_file_item_get_git_status(null).is_null());
            assert_eq!(fff_file_item_get_size(null), 0);
            assert_eq!(fff_file_item_get_modified(null), 0);
            assert_eq!(fff_file_item_get_access_frecency_score(null), 0);
            assert_eq!(fff_file_item_get_modification_frecency_score(null), 0);
            assert_eq!(fff_file_item_get_total_frecency_score(null), 0);
            assert!(!fff_file_item_get_is_binary(null));
        }
    }

    #[test]
    fn null_grep_match_returns_null_or_zero() {
        let null: *const FffGrepMatch = ptr::null();
        unsafe {
            assert!(fff_grep_match_get_relative_path(null).is_null());
            assert!(fff_grep_match_get_file_name(null).is_null());
            assert!(fff_grep_match_get_git_status(null).is_null());
            assert!(fff_grep_match_get_line_content(null).is_null());
            assert_eq!(fff_grep_match_get_line_number(null), 0);
            assert_eq!(fff_grep_match_get_byte_offset(null), 0);
            assert_eq!(fff_grep_match_get_col(null), 0);
            assert_eq!(fff_grep_match_get_size(null), 0);
            assert_eq!(fff_grep_match_get_modified(null), 0);
            assert_eq!(fff_grep_match_get_total_frecency_score(null), 0);
            assert_eq!(fff_grep_match_get_access_frecency_score(null), 0);
            assert_eq!(fff_grep_match_get_modification_frecency_score(null), 0);
            assert_eq!(fff_grep_match_get_match_ranges_count(null), 0);
            assert_eq!(fff_grep_match_get_context_before_count(null), 0);
            assert_eq!(fff_grep_match_get_context_after_count(null), 0);
            assert!(!fff_grep_match_get_has_fuzzy_score(null));
            assert_eq!(fff_grep_match_get_fuzzy_score(null), 0);
            assert!(!fff_grep_match_get_is_binary(null));
            assert!(!fff_grep_match_get_is_definition(null));
            assert!(fff_grep_match_get_context_before(null, 0).is_null());
            assert!(fff_grep_match_get_context_after(null, 0).is_null());
            assert!(fff_grep_match_get_match_range(null, 0).is_null());
        }
    }

    #[test]
    fn null_search_result_returns_zero() {
        let null: *const FffSearchResult = ptr::null();
        unsafe {
            assert_eq!(fff_search_result_get_count(null), 0);
            assert_eq!(fff_search_result_get_total_matched(null), 0);
            assert_eq!(fff_search_result_get_total_files(null), 0);
        }
    }

    #[test]
    fn null_grep_result_returns_zero_or_null() {
        let null: *const FffGrepResult = ptr::null();
        unsafe {
            assert_eq!(fff_grep_result_get_count(null), 0);
            assert_eq!(fff_grep_result_get_total_matched(null), 0);
            assert_eq!(fff_grep_result_get_total_files_searched(null), 0);
            assert_eq!(fff_grep_result_get_total_files(null), 0);
            assert_eq!(fff_grep_result_get_filtered_file_count(null), 0);
            assert_eq!(fff_grep_result_get_next_file_offset(null), 0);
            assert!(fff_grep_result_get_regex_fallback_error(null).is_null());
        }
    }

    #[test]
    fn null_result_returns_zero_or_null() {
        let null: *const FffResult = ptr::null();
        unsafe {
            assert!(!fff_result_get_success(null));
            assert!(fff_result_get_error(null).is_null());
            assert!(fff_result_get_handle(null).is_null());
            assert_eq!(fff_result_get_int_value(null), 0);
        }
    }

    // ── data correctness tests ────────────────────────────────────────────────

    #[test]
    fn result_getters_return_correct_values() {
        let error = CString::new("failed").unwrap();
        let handle = 0x1234usize as *mut std::ffi::c_void;
        let result = FffResult {
            success: false,
            error: error.as_ptr() as *mut std::ffi::c_char,
            handle,
            int_value: -7,
        };
        let p = &result as *const FffResult;
        unsafe {
            assert!(!fff_result_get_success(p));
            assert_eq!(
                std::ffi::CStr::from_ptr(fff_result_get_error(p)),
                error.as_c_str()
            );
            assert_eq!(fff_result_get_handle(p), handle);
            assert_eq!(fff_result_get_int_value(p), -7);
        }
    }

    #[test]
    fn file_item_getters_return_correct_values() {
        let mut item = make_file_item("src/main.rs", "main.rs");
        let p = &item as *const FffFileItem;
        unsafe {
            let path = std::ffi::CStr::from_ptr(fff_file_item_get_relative_path(p));
            assert_eq!(path.to_str().unwrap(), "src/main.rs");

            let name = std::ffi::CStr::from_ptr(fff_file_item_get_file_name(p));
            assert_eq!(name.to_str().unwrap(), "main.rs");

            assert!(fff_file_item_get_git_status(p).is_null());
            assert_eq!(fff_file_item_get_size(p), 1024);
            assert_eq!(fff_file_item_get_modified(p), 1_700_000_000);
            assert_eq!(fff_file_item_get_access_frecency_score(p), 10);
            assert_eq!(fff_file_item_get_modification_frecency_score(p), 20);
            assert_eq!(fff_file_item_get_total_frecency_score(p), 30);
            assert!(!fff_file_item_get_is_binary(p));

            free_file_item(&mut item);
        }
    }

    #[test]
    fn grep_match_getters_return_correct_values() {
        let mut m = make_grep_match("src/lib.rs", "fn hello()");
        let p = &m as *const FffGrepMatch;
        unsafe {
            let path = std::ffi::CStr::from_ptr(fff_grep_match_get_relative_path(p));
            assert_eq!(path.to_str().unwrap(), "src/lib.rs");

            let line = std::ffi::CStr::from_ptr(fff_grep_match_get_line_content(p));
            assert_eq!(line.to_str().unwrap(), "fn hello()");

            assert_eq!(fff_grep_match_get_line_number(p), 42);
            assert_eq!(fff_grep_match_get_byte_offset(p), 100);
            assert_eq!(fff_grep_match_get_col(p), 8);
            assert_eq!(fff_grep_match_get_size(p), 512);
            assert_eq!(fff_grep_match_get_modified(p), 1_600_000_000);
            assert_eq!(fff_grep_match_get_total_frecency_score(p), 5);
            assert_eq!(fff_grep_match_get_access_frecency_score(p), 6);
            assert_eq!(fff_grep_match_get_modification_frecency_score(p), 7);
            assert_eq!(fff_grep_match_get_match_ranges_count(p), 0);
            assert!(!fff_grep_match_get_has_fuzzy_score(p));
            assert!(!fff_grep_match_get_is_binary(p));
            assert!(fff_grep_match_get_is_definition(p));

            free_grep_match(&mut m);
        }
    }

    #[test]
    fn search_result_getters_return_correct_values() {
        let r = make_search_result(5, 20, 100);
        let p = &r as *const FffSearchResult;
        unsafe {
            assert_eq!(fff_search_result_get_count(p), 5);
            assert_eq!(fff_search_result_get_total_matched(p), 20);
            assert_eq!(fff_search_result_get_total_files(p), 100);
        }
    }

    #[test]
    fn grep_result_getters_return_correct_values() {
        let r = make_grep_result();
        let p = &r as *const FffGrepResult;
        unsafe {
            assert_eq!(fff_grep_result_get_count(p), 3);
            assert_eq!(fff_grep_result_get_total_matched(p), 10);
            assert_eq!(fff_grep_result_get_total_files_searched(p), 50);
            assert_eq!(fff_grep_result_get_total_files(p), 200);
            assert_eq!(fff_grep_result_get_filtered_file_count(p), 80);
            assert_eq!(fff_grep_result_get_next_file_offset(p), 51);
            assert!(fff_grep_result_get_regex_fallback_error(p).is_null());
        }
    }
}
