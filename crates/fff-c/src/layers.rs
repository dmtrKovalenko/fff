use std::ffi::{c_char, c_void};
use std::path::Path;

use fff::file_picker::FilePicker;
use fff::{IndexEntry, LayerBuilder, LayerSnapshot};

use crate::ffi_types::FffResult;
use crate::{cstr_to_str, instance_ref, optional_cstr};

/// Seal the writable layer and return the new layer ID in `int_value`.
/// # Safety
/// Provide a live instance and a NULL or NUL-terminated UTF-8 label.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_layer_create(
    fff_handle: *mut c_void,
    label: *const c_char,
) -> *mut FffResult {
    let label = unsafe { optional_cstr(label) };
    unsafe {
        with_picker_mut(fff_handle, |picker| {
            FffResult::ok_int(picker.create_layer(label) as i64)
        })
    }
}

/// Return the writable layer ID in `int_value`.
/// # Safety
/// Provide a live instance.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_layer_current(fff_handle: *mut c_void) -> *mut FffResult {
    unsafe {
        with_picker(fff_handle, |picker| {
            FffResult::ok_int(picker.current_layer() as i64)
        })
    }
}

/// Return the number of live files in the layer (0 for the base scan).
/// # Safety
/// Provide a live instance.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_layer_file_count(
    fff_handle: *mut c_void,
    layer: u8,
) -> *mut FffResult {
    unsafe {
        with_picker(fff_handle, |picker| {
            let count = picker
                .files_in_layer(layer)
                .iter()
                .filter(|f| !f.is_deleted())
                .count();
            FffResult::ok_int(count as i64)
        })
    }
}

/// Return overlay metadata as a JSON array in `handle`. Free it with `fff_free_string`.
/// # Safety
/// Provide a live instance.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_layer_list(fff_handle: *mut c_void) -> *mut FffResult {
    unsafe {
        with_picker(fff_handle, |picker| {
            let layers: Vec<serde_json::Value> = picker
                .layers()
                .iter()
                .map(|l| {
                    serde_json::json!({
                        "id": l.id,
                        "label": l.label,
                        "file_count": l.file_count,
                        "live_file_count": l.live_file_count,
                        "dir_count": l.dir_count,
                        "arena_bytes": l.arena_bytes,
                        "is_open": l.is_open,
                    })
                })
                .collect();
            FffResult::ok_string(&serde_json::Value::Array(layers).to_string())
        })
    }
}

/// Append a batch of base-relative paths and return the number of files added.
/// # Safety
/// Provide a live instance and `paths_len` readable bytes containing exactly `count` NUL-terminated UTF-8 paths.
/// For an empty batch `paths` may be NULL. Each metadata array is NULL or holds `count` readable little-endian u64s (unaligned allowed).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_layer_add_paths(
    fff_handle: *mut c_void,
    paths: *const c_char,
    paths_len: usize,
    sizes: *const u64,
    modified: *const u64,
    count: usize,
) -> *mut FffResult {
    let entries = match unsafe { entries_from_raw(paths, paths_len, sizes, modified, count) } {
        Ok(e) => e,
        Err(e) => return e,
    };

    unsafe {
        with_picker_mut(fff_handle, |picker| match picker.add_entries(entries) {
            Ok(added) => FffResult::ok_int(added as i64),
            Err(e) => FffResult::err(&format!("Failed to add entries: {}", e)),
        })
    }
}

/// Write a snapshot from a path batch and return the number of bytes written.
/// # Safety
/// Use the buffers specified by `fff_layer_add_paths`. Provide a NUL-terminated UTF-8 output path and an optional NUL-terminated UTF-8 label.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_layer_build(
    label: *const c_char,
    paths: *const c_char,
    paths_len: usize,
    sizes: *const u64,
    modified: *const u64,
    count: usize,
    out_path: *const c_char,
) -> *mut FffResult {
    let label = unsafe { optional_cstr(label) };
    let Some(out_path) = (unsafe { cstr_to_str(out_path) }) else {
        return FffResult::err("Output path is null or invalid UTF-8");
    };
    let entries = match unsafe { entries_from_raw(paths, paths_len, sizes, modified, count) } {
        Ok(e) => e,
        Err(e) => return e,
    };

    let mut builder = LayerBuilder::with_capacity(label, entries.len());
    for entry in &entries {
        builder.push(entry);
    }

    match builder.finish().save(Path::new(out_path)) {
        Ok(written) => FffResult::ok_int(written as i64),
        Err(e) => FffResult::err(&format!("Failed to save layer: {}", e)),
    }
}

/// Save a layer (0 for the base scan) and return the number of bytes written.
/// # Safety
/// Provide a live instance and a NUL-terminated UTF-8 path.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_layer_save(
    fff_handle: *mut c_void,
    layer: u8,
    path: *const c_char,
) -> *mut FffResult {
    let Some(path) = (unsafe { cstr_to_str(path) }) else {
        return FffResult::err("Path is null or invalid UTF-8");
    };

    // Export under the read lock, write to disk with no lock held.
    let snapshot = unsafe {
        with_picker_value(fff_handle, |picker| {
            picker
                .export_layer(layer)
                .ok_or_else(|| FffResult::err("No such layer"))
        })
    };
    match snapshot.and_then(|s| {
        s.save(path)
            .map_err(|e| FffResult::err(&format!("Failed to save layer: {}", e)))
    }) {
        Ok(written) => FffResult::ok_int(written as i64),
        Err(e) => e,
    }
}

/// Load a sealed overlay and return its layer ID.
/// # Safety
/// Provide a live instance and a NUL-terminated UTF-8 path.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_layer_load(
    fff_handle: *mut c_void,
    path: *const c_char,
) -> *mut FffResult {
    let Some(path) = (unsafe { cstr_to_str(path) }) else {
        return FffResult::err("Path is null or invalid UTF-8");
    };
    let snapshot = match LayerSnapshot::load(path) {
        Ok(s) => s,
        Err(e) => return FffResult::err(&format!("Failed to load layer: {}", e)),
    };

    unsafe {
        with_picker_mut(fff_handle, |picker| match picker.import_layer(snapshot) {
            Ok(id) => FffResult::ok_int(id as i64),
            Err(e) => FffResult::err(&format!("Failed to import layer: {}", e)),
        })
    }
}

/// Start a base scan from a snapshot and return its file count. Wait with `fff_wait_for_scan`.
/// # Safety
/// Provide a live instance and a NUL-terminated UTF-8 path.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_base_load(
    fff_handle: *mut c_void,
    path: *const c_char,
) -> *mut FffResult {
    let inst = match unsafe { instance_ref(fff_handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };
    let Some(path) = (unsafe { cstr_to_str(path) }) else {
        return FffResult::err("Path is null or invalid UTF-8");
    };
    let snapshot = match LayerSnapshot::load(path) {
        Ok(s) => s,
        Err(e) => return FffResult::err(&format!("Failed to load layer: {}", e)),
    };
    let file_count = snapshot.file_count();

    match inst.picker.import_base(snapshot, &inst.frecency) {
        Ok(()) => FffResult::ok_int(file_count as i64),
        Err(e) => FffResult::err(&format!("Failed to import base: {}", e)),
    }
}

unsafe fn with_picker(
    fff_handle: *mut c_void,
    f: impl FnOnce(&FilePicker) -> *mut FffResult,
) -> *mut FffResult {
    match unsafe { with_picker_value(fff_handle, |picker| Ok(f(picker))) } {
        Ok(r) | Err(r) => r,
    }
}

unsafe fn with_picker_value<T>(
    fff_handle: *mut c_void,
    f: impl FnOnce(&FilePicker) -> Result<T, *mut FffResult>,
) -> Result<T, *mut FffResult> {
    let inst = unsafe { instance_ref(fff_handle) }?;
    let guard = inst
        .picker
        .read()
        .map_err(|e| FffResult::err(&format!("Failed to acquire file picker lock: {}", e)))?;
    match guard.as_ref() {
        Some(picker) => f(picker),
        None => Err(FffResult::err("File picker not initialized")),
    }
}

unsafe fn with_picker_mut(
    fff_handle: *mut c_void,
    f: impl FnOnce(&mut FilePicker) -> *mut FffResult,
) -> *mut FffResult {
    let inst = match unsafe { instance_ref(fff_handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };
    let mut guard = match inst.picker.write() {
        Ok(g) => g,
        Err(e) => return FffResult::err(&format!("Failed to acquire file picker lock: {}", e)),
    };
    match guard.as_mut() {
        Some(picker) => f(picker),
        None => FffResult::err("File picker not initialized"),
    }
}

unsafe fn entries_from_raw<'a>(
    paths: *const c_char,
    paths_len: usize,
    sizes: *const u64,
    modified: *const u64,
    count: usize,
) -> Result<Vec<IndexEntry<'a>>, *mut FffResult> {
    if count == 0 && paths_len == 0 {
        return Ok(Vec::new());
    }
    if paths.is_null() || paths_len > isize::MAX as usize || count > isize::MAX as usize / 8 {
        return Err(FffResult::err(
            "Invalid layer batch buffer length or pointer",
        ));
    }
    let bytes = unsafe { std::slice::from_raw_parts(paths.cast::<u8>(), paths_len) };
    let text = std::str::from_utf8(bytes).map_err(|_| FffResult::err("Paths are not UTF-8"))?;
    let records = text
        .strip_suffix('\0')
        .ok_or_else(|| FffResult::err("Missing path terminator"))?;
    if records.split('\0').count() != count {
        return Err(FffResult::err(
            "Layer path count does not match the batch count",
        ));
    }

    let field = |arr: *const u64, i: usize| {
        if arr.is_null() {
            0
        } else {
            u64::from_le(unsafe { arr.add(i).read_unaligned() })
        }
    };
    records
        .split('\0')
        .enumerate()
        .map(|(i, path)| {
            if path.is_empty() {
                return Err(FffResult::err("Layer paths must not be empty"));
            }
            Ok(IndexEntry::new(path).with_metadata(field(sizes, i), field(modified, i)))
        })
        .collect()
}
