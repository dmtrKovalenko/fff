use std::ffi::{CString, c_char, c_void};
use std::ptr;
use std::sync::Arc;
use std::sync::Mutex;

use fff::{WatchEvent, WatchId, WatchOptions};

use crate::ffi_types::FffResult;
use crate::instance_ref;

/// Current version of [`FffWatchOptions`].
pub const FFF_WATCH_OPTIONS_VERSION: u32 = 1;

/// Options for `fff_watch`. Versioned: new fields are only appended.
#[repr(C)]
pub struct FffWatchOptions {
    /// Set to [`FFF_WATCH_OPTIONS_VERSION`] when allocating.
    pub version: u32,
    /// Per-subscription excludes (parcel-watcher style): entries with wildcards
    /// are base-relative globs, entries without are path prefixes. NULL when
    /// `ignore_count` is 0.
    pub ignore: *const *const c_char,
    pub ignore_count: u32,
    // ----- new version 2+ fields go here, ALWAYS appended -----
}

/// A single watch event. `kind`: 0 = created, 1 = modified, 2 = removed,
/// 3 = rescan (events were lost; re-stat what you care about),
/// 4 = renamed (`path` is the destination; read the source with
/// `fff_watch_events_get_from_path`).
///
/// Layout is frozen: this is an array element, so growing it would change the
/// stride every existing binding compiled against.
#[repr(C)]
pub struct FffWatchEvent {
    /// Absolute path (heap C string owned by the parent batch).
    pub path: *mut c_char,
    pub kind: u8,
}

/// A batch of watch events. Free with `fff_free_watch_events`.
/// Versioned by append only: existing field offsets never move.
#[repr(C)]
pub struct FffWatchEventBatch {
    pub events: *mut FffWatchEvent,
    pub count: u32,
    /// Parallel to `events`, `count` long: the pre-rename path for entries
    /// with `kind == 4`, null for every other entry. Null when the batch holds
    /// no renames at all.
    pub rename_sources: *mut *mut c_char,
}

/// Instance-wide callback invoked with `(watch_id, batch)` for every `fff_watch`
/// subscription. The callee owns and frees `batch` via `fff_free_watch_events`.
pub type FffWatchCallback =
    unsafe extern "C" fn(watch_id: u64, batch: *mut FffWatchEventBatch, user_data: *mut c_void);

fn batch_into_raw(events: &[WatchEvent]) -> *mut FffWatchEventBatch {
    let items: Vec<FffWatchEvent> = events
        .iter()
        .map(|ev| FffWatchEvent {
            path: path_into_raw(&ev.path),
            kind: ev.kind as u8,
        })
        .collect();

    let count = items.len() as u32;
    let events_ptr = leak_slice(items);

    let rename_sources = if events.iter().any(|ev| ev.from.is_some()) {
        let sources: Vec<*mut c_char> = events
            .iter()
            .map(|ev| {
                ev.from
                    .as_ref()
                    .map_or(ptr::null_mut(), |p| path_into_raw(p))
            })
            .collect();
        leak_slice(sources)
    } else {
        ptr::null_mut()
    };

    Box::into_raw(Box::new(FffWatchEventBatch {
        events: events_ptr,
        count,
        rename_sources,
    }))
}

fn path_into_raw(path: &std::path::Path) -> *mut c_char {
    CString::new(path.to_string_lossy().as_bytes())
        .unwrap_or_default()
        .into_raw()
}

fn leak_slice<T>(items: Vec<T>) -> *mut T {
    if items.is_empty() {
        return ptr::null_mut();
    }
    let mut boxed = items.into_boxed_slice();
    let p = boxed.as_mut_ptr();
    std::mem::forget(boxed);
    p
}

unsafe fn watch_options_from_ffi(
    opts: *const FffWatchOptions,
) -> Result<WatchOptions, *mut FffResult> {
    if opts.is_null() {
        return Ok(WatchOptions::default());
    }
    let opts = unsafe { &*opts };
    if opts.version == 0 || opts.version > FFF_WATCH_OPTIONS_VERSION {
        return Err(FffResult::err(&format!(
            "Unsupported FffWatchOptions version {} (library understands up to {})",
            opts.version, FFF_WATCH_OPTIONS_VERSION
        )));
    }

    let mut ignore = Vec::with_capacity(opts.ignore_count as usize);
    if opts.ignore_count > 0 {
        if opts.ignore.is_null() {
            return Err(FffResult::err("ignore_count > 0 but ignore is NULL"));
        }
        for i in 0..opts.ignore_count as usize {
            let entry = unsafe { *opts.ignore.add(i) };
            match unsafe { crate::cstr_to_str(entry) } {
                Some(s) if !s.is_empty() => ignore.push(s.to_string()),
                Some(_) => {}
                None => return Err(FffResult::err("ignore entry is NULL or invalid UTF-8")),
            }
        }
    }

    Ok(WatchOptions { ignore })
}

// The caller guarantees user_data is safe on the callback thread.
struct UserData(*mut c_void);
unsafe impl Send for UserData {}
unsafe impl Sync for UserData {}

// Shared so a closure surviving an unwatch race never dangles.
#[derive(Default)]
pub(crate) struct WatchCallbackSlot(Mutex<Option<(FffWatchCallback, UserData)>>);

impl WatchCallbackSlot {
    fn get(&self) -> Option<(FffWatchCallback, *mut c_void)> {
        self.0
            .lock()
            .ok()
            .and_then(|guard| guard.as_ref().map(|(cb, ud)| (*cb, ud.0)))
    }

    fn set(&self, callback: FffWatchCallback, user_data: *mut c_void) {
        if let Ok(mut guard) = self.0.lock() {
            *guard = Some((callback, UserData(user_data)));
        }
    }

    pub(crate) fn clear(&self) {
        if let Ok(mut guard) = self.0.lock() {
            *guard = None;
        }
    }
}

/// Register the instance-wide watch callback used by all `fff_watch`
/// subscriptions; call before the first `fff_watch`, calling again replaces it.
///
/// ## Safety
/// * `fff_handle` must be a valid instance pointer from `fff_create_instance`.
/// * `callback` must remain callable until fff_unwatch called
///   `fff_destroy(fff_handle)` returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_set_watch_callback(
    fff_handle: *mut c_void,
    callback: FffWatchCallback,
    user_data: *mut c_void,
) -> *mut FffResult {
    let inst = match unsafe { instance_ref(fff_handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };
    inst.watch_callback.set(callback, user_data);
    FffResult::ok_empty()
}

/// Subscribe to filesystem changes, delivered through the instance callback
/// registered by `fff_set_watch_callback`.
///
/// Returns the watch id, pass it to `fff_unwatch` to stop.
///
/// `pattern` if non `NULL` can be wildcard pattern, absolute, or relative path
/// that will be used to filter the events triggering exact subscription.
///
/// ## Safety
/// * `fff_handle` must be a valid instance pointer from `fff_create_instance`.
/// * `pattern` must be NULL or valid null-terminated UTF-8.
/// * `opts` must be NULL or a valid `FffWatchOptions` pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_watch(
    fff_handle: *mut c_void,
    pattern: *const c_char,
    opts: *const FffWatchOptions,
) -> *mut FffResult {
    let inst = match unsafe { instance_ref(fff_handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };
    // NULL pattern = watch the entire indexed tree ("" in core).
    let pattern_str = if pattern.is_null() {
        ""
    } else {
        match unsafe { crate::cstr_to_str(pattern) } {
            Some(s) => s,
            None => return FffResult::err("Pattern is not valid UTF-8"),
        }
    };
    let options = match unsafe { watch_options_from_ffi(opts) } {
        Ok(o) => o,
        Err(e) => return e,
    };
    if inst.watch_callback.get().is_none() {
        return FffResult::err("No watch callback registered. Call fff_set_watch_callback first.");
    }

    let slot = Arc::clone(&inst.watch_callback);
    let result = inst.picker.watch(pattern_str, options, move |id, events| {
        if let Some((cb, user_data)) = slot.get() {
            let batch = batch_into_raw(events);
            unsafe { cb(id.0, batch, user_data) };
        }
    });

    match result {
        Ok(id) => FffResult::ok_int(id.0 as i64),
        Err(e) => FffResult::err(&format!("Failed to subscribe: {}", e)),
    }
}

/// [`fff_watch`] adapter with flattened options, for FFI libraries that cannot
/// marshal pointer arrays inside structs (e.g. Node's `ffi-rs`).
///
/// ## Safety
/// * `fff_handle` must be a valid instance pointer from `fff_create_instance`.
/// * `pattern` must be NULL (watch everything) or valid null-terminated UTF-8.
/// * `ignore` must be NULL or point to `ignore_count` valid C strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_watch_args(
    fff_handle: *mut c_void,
    pattern: *const c_char,
    ignore: *const *const c_char,
    ignore_count: u32,
) -> *mut FffResult {
    let opts = FffWatchOptions {
        version: FFF_WATCH_OPTIONS_VERSION,
        ignore,
        ignore_count,
    };
    unsafe { fff_watch(fff_handle, pattern, &opts) }
}

/// Remove a watch subscription. `int_value` = 1 if the id existed, 0 otherwise.
///
/// ## Safety
/// `fff_handle` must be a valid instance pointer from `fff_create_instance`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_unwatch(fff_handle: *mut c_void, watch_id: u64) -> *mut FffResult {
    let inst = match unsafe { instance_ref(fff_handle) } {
        Ok(i) => i,
        Err(e) => return e,
    };
    FffResult::ok_int(inst.picker.unwatch(WatchId(watch_id)) as i64)
}

/// Number of events in a batch, 0 if `batch` is null.
///
/// ## Safety
/// `batch` must be a valid `FffWatchEventBatch` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_watch_events_count(batch: *const FffWatchEventBatch) -> u32 {
    if batch.is_null() {
        return 0;
    }
    unsafe { (*batch).count }
}

/// Absolute path of event `index`, will be null when out of bounds
///
/// ## Safety
/// `batch` must be a valid `FffWatchEventBatch` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_watch_events_get_path(
    batch: *const FffWatchEventBatch,
    index: u32,
) -> *const c_char {
    match unsafe { watch_event_at(batch, index) } {
        Some(ev) => ev.path,
        None => ptr::null(),
    }
}

/// Pre-rename path of event `index`, null unless its kind is 4 (renamed).
/// Owned by the batch; do not free separately.
///
/// ## Safety
/// `batch` must be a valid `FffWatchEventBatch` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_watch_events_get_from_path(
    batch: *const FffWatchEventBatch,
    index: u32,
) -> *const c_char {
    if batch.is_null() {
        return ptr::null();
    }
    let batch = unsafe { &*batch };
    if batch.rename_sources.is_null() || index >= batch.count {
        return ptr::null();
    }
    unsafe { *batch.rename_sources.add(index as usize) }
}

/// Kind of event `index` (0 = created, 1 = modified, 2 = removed, 3 = rescan,
/// 4 = renamed).
/// 3 (rescan aka "re-stat something" kind) returned when OS based buffer
/// has been overflown and some events might be loss. Paths will contain a list of
/// directories that needs to be rescanned to ensure consistency.
/// 4 reports a move: `path` is the destination and
/// `fff_watch_events_get_from_path` yields the source.
///
/// ## Safety
/// `batch` must be a valid `FffWatchEventBatch` pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_watch_events_get_kind(
    batch: *const FffWatchEventBatch,
    index: u32,
) -> u8 {
    match unsafe { watch_event_at(batch, index) } {
        Some(ev) => ev.kind,
        None => 3,
    }
}

unsafe fn watch_event_at<'a>(
    batch: *const FffWatchEventBatch,
    index: u32,
) -> Option<&'a FffWatchEvent> {
    if batch.is_null() {
        return None;
    }
    let batch = unsafe { &*batch };
    if batch.events.is_null() || index >= batch.count {
        return None;
    }
    Some(unsafe { &*batch.events.add(index as usize) })
}

/// Free a watch event batch delivered to the instance callback.
///
/// ## Safety
/// `batch` must be a pointer produced by this library, or null (no-op).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fff_free_watch_events(batch: *mut FffWatchEventBatch) {
    if batch.is_null() {
        return;
    }
    unsafe {
        let batch = Box::from_raw(batch);
        let count = batch.count as usize;
        if !batch.events.is_null() {
            let events = Vec::from_raw_parts(batch.events, count, count);
            for ev in events {
                if !ev.path.is_null() {
                    drop(CString::from_raw(ev.path));
                }
            }
        }
        if !batch.rename_sources.is_null() {
            let sources = Vec::from_raw_parts(batch.rename_sources, count, count);
            for source in sources {
                if !source.is_null() {
                    drop(CString::from_raw(source));
                }
            }
        }
    }
}

// THESE TESTS MUST NEVER BE UPDATED, ONLY EXTENDED WITH NEW FIELDS —
// bindings hardcode these offsets (ABI stability).
#[cfg(test)]
mod layout_tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn watch_ffi_layouts_are_stable_64bit() {
        assert_eq!(size_of::<FffWatchOptions>(), 24);
        assert_eq!(offset_of!(FffWatchOptions, version), 0);
        assert_eq!(offset_of!(FffWatchOptions, ignore), 8);
        assert_eq!(offset_of!(FffWatchOptions, ignore_count), 16);

        assert_eq!(size_of::<FffWatchEvent>(), 16);
        assert_eq!(offset_of!(FffWatchEvent, path), 0);
        assert_eq!(offset_of!(FffWatchEvent, kind), 8);

        assert_eq!(offset_of!(FffWatchEventBatch, events), 0);
        assert_eq!(offset_of!(FffWatchEventBatch, count), 8);
        // Appended in the renamed-event release. The batch is a single
        // library-allocated struct reached only through a pointer, so growing
        // its tail leaves every previously published offset valid.
        assert_eq!(size_of::<FffWatchEventBatch>(), 24);
        assert_eq!(offset_of!(FffWatchEventBatch, rename_sources), 16);
    }
}
