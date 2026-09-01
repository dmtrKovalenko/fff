/**
 * Bun FFI bindings for the fff-c native library
 *
 * This module uses Bun's native FFI to call into the Rust C library.
 * All functions follow the Result pattern for error handling.
 *
 * The API is instance-based: `ffiCreate` returns an opaque handle that must
 * be passed to all subsequent calls and freed with `ffiDestroy`.
 */

import {
  CString,
  dlopen,
  FFIType,
  type JSCallback,
  type Pointer,
  ptr,
  read,
} from "bun:ffi";
import { findBinary } from "./download";
import { embeddedLibPath } from "./embedded";
import type {
  DirItem,
  DirSearchResult,
  FileItem,
  GrepMatch,
  GrepResult,
  Location,
  MixedItem,
  MixedSearchResult,
  Result,
  ScanProgress,
  Score,
  SearchResult,
  WatchEvent,
  WatchEventKind,
} from "./fff-api";
import { createGrepCursor, err } from "./fff-api";

/** Grep mode constants matching the C API (u8). */
const GREP_MODE_PLAIN = 0;
const GREP_MODE_REGEX = 1;
const GREP_MODE_FUZZY = 2;

/** Map string mode to u8 */
function grepModeToU8(mode?: string): number {
  switch (mode) {
    case "regex":
      return GREP_MODE_REGEX;
    case "fuzzy":
      return GREP_MODE_FUZZY;
    default:
      return GREP_MODE_PLAIN;
  }
}

const ffiDefinition = {
  fff_create_instance2: {
    args: [
      FFIType.cstring, // base_path
      FFIType.cstring, // frecency_db_path
      FFIType.cstring, // history_db_path
      FFIType.bool, // use_unsafe_no_lock
      FFIType.bool, // enable_mmap_cache
      FFIType.bool, // enable_content_indexing
      FFIType.bool, // watch
      FFIType.bool, // ai_mode
      FFIType.cstring, // log_file_path
      FFIType.cstring, // log_level
      FFIType.u64, // cache_budget_max_files
      FFIType.u64, // cache_budget_max_bytes
      FFIType.u64, // cache_budget_max_file_size
    ],
    returns: FFIType.ptr,
  },
  fff_create_instance_with: {
    args: [FFIType.ptr], // *const FffCreateOptions
    returns: FFIType.ptr,
  },
  fff_destroy: {
    args: [FFIType.ptr],
    returns: FFIType.void,
  },

  // Search
  fff_search: {
    args: [
      FFIType.ptr, // handle
      FFIType.cstring, // query
      FFIType.cstring, // current_file
      FFIType.u32, // max_threads
      FFIType.u32, // page_index
      FFIType.u32, // page_size
      FFIType.i32, // combo_boost_multiplier
      FFIType.u32, // min_combo_count
    ],
    returns: FFIType.ptr,
  },

  // Glob-only search (bypasses query parser)
  fff_glob: {
    args: [
      FFIType.ptr, // handle
      FFIType.cstring, // pattern
      FFIType.cstring, // current_file
      FFIType.u32, // max_threads
      FFIType.u32, // page_index
      FFIType.u32, // page_size
    ],
    returns: FFIType.ptr,
  },

  // Directory search
  fff_search_directories: {
    args: [
      FFIType.ptr, // handle
      FFIType.cstring, // query
      FFIType.cstring, // current_file
      FFIType.u32, // max_threads
      FFIType.u32, // page_index
      FFIType.u32, // page_size
    ],
    returns: FFIType.ptr,
  },

  // Mixed search (files + directories)
  fff_search_mixed: {
    args: [
      FFIType.ptr, // handle
      FFIType.cstring, // query
      FFIType.cstring, // current_file
      FFIType.u32, // max_threads
      FFIType.u32, // page_index
      FFIType.u32, // page_size
      FFIType.i32, // combo_boost_multiplier
      FFIType.u32, // min_combo_count
    ],
    returns: FFIType.ptr,
  },

  // Live grep (content search)
  fff_live_grep: {
    args: [
      FFIType.ptr, // handle
      FFIType.cstring, // query
      FFIType.u8, // mode
      FFIType.u64, // max_file_size
      FFIType.u32, // max_matches_per_file
      FFIType.bool, // smart_case
      FFIType.u32, // file_offset
      FFIType.u32, // page_limit
      FFIType.u64, // time_budget_ms
      FFIType.u32, // before_context
      FFIType.u32, // after_context
      FFIType.bool, // classify_definitions
    ],
    returns: FFIType.ptr,
  },

  // Multi-pattern grep (Aho-Corasick)
  fff_multi_grep: {
    args: [
      FFIType.ptr, // handle
      FFIType.cstring, // patterns_joined (\n-separated)
      FFIType.cstring, // constraints
      FFIType.u64, // max_file_size
      FFIType.u32, // max_matches_per_file
      FFIType.bool, // smart_case
      FFIType.u32, // file_offset
      FFIType.u32, // page_limit
      FFIType.u64, // time_budget_ms
      FFIType.u32, // before_context
      FFIType.u32, // after_context
      FFIType.bool, // classify_definitions
    ],
    returns: FFIType.ptr,
  },

  // File index
  fff_scan_files: {
    args: [FFIType.ptr],
    returns: FFIType.ptr,
  },
  fff_is_scanning: {
    args: [FFIType.ptr],
    returns: FFIType.bool,
  },
  fff_get_base_path: {
    args: [FFIType.ptr],
    returns: FFIType.ptr,
  },
  fff_get_scan_progress: {
    args: [FFIType.ptr],
    returns: FFIType.ptr,
  },
  fff_wait_for_scan: {
    args: [FFIType.ptr, FFIType.u64],
    returns: FFIType.ptr,
  },
  fff_restart_index: {
    args: [FFIType.ptr, FFIType.cstring],
    returns: FFIType.ptr,
  },

  // Watch subscriptions
  fff_set_watch_callback: {
    args: [
      FFIType.ptr, // handle
      FFIType.function, // FffWatchCallback (instance-wide)
      FFIType.ptr, // user_data
    ],
    returns: FFIType.ptr,
  },
  fff_watch: {
    args: [
      FFIType.ptr, // handle
      FFIType.cstring, // pattern
      FFIType.ptr, // *const FffWatchOptions (or NULL)
    ],
    returns: FFIType.ptr,
  },
  fff_unwatch: {
    args: [FFIType.ptr, FFIType.u64], // handle, watch_id
    returns: FFIType.ptr,
  },
  fff_free_watch_events: {
    args: [FFIType.ptr],
    returns: FFIType.void,
  },
  fff_watch_events_count: {
    args: [FFIType.ptr],
    returns: FFIType.u32,
  },
  fff_watch_events_get_path: {
    args: [FFIType.ptr, FFIType.u32],
    returns: FFIType.ptr,
  },
  fff_watch_events_get_kind: {
    args: [FFIType.ptr, FFIType.u32],
    returns: FFIType.u8,
  },
  fff_watch_events_get_from_path: {
    args: [FFIType.ptr, FFIType.u32],
    returns: FFIType.ptr,
  },

  // Git
  fff_refresh_git_status: {
    args: [FFIType.ptr],
    returns: FFIType.ptr,
  },

  // Query tracking
  fff_track_query: {
    args: [FFIType.ptr, FFIType.cstring, FFIType.cstring],
    returns: FFIType.ptr,
  },
  fff_get_historical_query: {
    args: [FFIType.ptr, FFIType.u64],
    returns: FFIType.ptr,
  },

  // Utilities
  fff_health_check: {
    args: [FFIType.ptr, FFIType.cstring],
    returns: FFIType.ptr,
  },

  // Search result accessors / free
  fff_free_search_result: {
    args: [FFIType.ptr],
    returns: FFIType.void,
  },
  fff_search_result_get_item: {
    args: [FFIType.ptr, FFIType.u32],
    returns: FFIType.ptr,
  },
  fff_search_result_get_score: {
    args: [FFIType.ptr, FFIType.u32],
    returns: FFIType.ptr,
  },

  // Dir search result accessors / free
  fff_free_dir_search_result: {
    args: [FFIType.ptr],
    returns: FFIType.void,
  },
  fff_dir_search_result_get_item: {
    args: [FFIType.ptr, FFIType.u32],
    returns: FFIType.ptr,
  },
  fff_dir_search_result_get_score: {
    args: [FFIType.ptr, FFIType.u32],
    returns: FFIType.ptr,
  },

  // Mixed search result accessors / free
  fff_free_mixed_search_result: {
    args: [FFIType.ptr],
    returns: FFIType.void,
  },
  fff_mixed_search_result_get_item: {
    args: [FFIType.ptr, FFIType.u32],
    returns: FFIType.ptr,
  },
  fff_mixed_search_result_get_score: {
    args: [FFIType.ptr, FFIType.u32],
    returns: FFIType.ptr,
  },

  // Grep result accessors / free
  fff_free_grep_result: {
    args: [FFIType.ptr],
    returns: FFIType.void,
  },
  fff_grep_result_get_match: {
    args: [FFIType.ptr, FFIType.u32],
    returns: FFIType.ptr,
  },

  // Memory management
  fff_free_result: {
    args: [FFIType.ptr],
    returns: FFIType.void,
  },
  fff_free_string: {
    args: [FFIType.ptr],
    returns: FFIType.void,
  },
  fff_free_scan_progress: {
    args: [FFIType.ptr],
    returns: FFIType.void,
  },
} as const;

type FFFLibrary = ReturnType<typeof dlopen<typeof ffiDefinition>>;

let lib: FFFLibrary | null = null;

/**
 * Load the native library
 */
function loadLibrary(): FFFLibrary {
  if (lib) return lib;

  const isEmbedded = embeddedLibPath?.includes("$bunfs") ?? false;
  const binaryPath = isEmbedded ? embeddedLibPath : (findBinary() ?? embeddedLibPath);
  if (!binaryPath) {
    throw new Error(libNotFoundMessage());
  }

  lib = dlopen(binaryPath, ffiDefinition);
  return lib;
}

function libNotFoundMessage(): string {
  if (import.meta.url.includes("$bunfs")) {
    if (process.platform === "linux") {
      return [
        "You are running bun --compile with fff native library which CAN NOT resolve a binary",
        "On Linux the libc must be supplied at compile time so the native lib is bundled.",
        "Rebuild with:",
        "  bun build --compile --define FFF_LIBC='\"gnu\"'  ...   # glibc",
        "  bun build --compile --define FFF_LIBC='\"musl\"' ...   # musl / Alpine",
      ].join("\n");
    }

    return "fff native library was not embedded into this executable. Rebuild with `bun build --compile` and ensure the @ff-labs/fff-bin-* package for this platform is installed.";
  }

  return "fff native library not found. Build from source with `cargo build --release -p fff-c` or install the platform package.";
}

/**
 * Encode a string for FFI (null-terminated)
 */
function encodeString(s: string): Uint8Array {
  return new TextEncoder().encode(`${s}\0`);
}

/**
 * Read a C string from a pointer
 * Note: read.ptr() returns number but CString expects Pointer - we cast through unknown
 */
function readCString(pointer: Pointer | number | null): string | null {
  if (pointer === null || pointer === 0) return null;
  // CString constructor accepts Pointer, but read.ptr returns number
  // Cast through unknown for runtime compatibility
  return new CString(pointer as unknown as Pointer).toString();
}

/**
 * Convert snake_case keys to camelCase recursively
 */
function snakeToCamel(obj: unknown): unknown {
  if (obj === null || obj === undefined) return obj;
  if (typeof obj !== "object") return obj;
  if (Array.isArray(obj)) return obj.map(snakeToCamel);

  const result: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(obj as Record<string, unknown>)) {
    const camelKey = key.replace(/_([a-z])/g, (_, letter) => letter.toUpperCase());
    result[camelKey] = snakeToCamel(value);
  }
  return result;
}

// ---------------------------------------------------------------------------
// FffResult byte offsets (must match #[repr(C)] layout on 64-bit)
// { success: bool(1+7pad), error: *char(8), handle: *void(8), int_value: i64(8) }
// ---------------------------------------------------------------------------
const RES_SUCCESS = 0; // bool (1 + 7 padding)
const RES_ERROR = 8; // *mut c_char (8)
const RES_HANDLE = 16; // *mut c_void (8)
const RES_INT_VALUE = 24; // i64         (8)

// MUST match `crates/fff-c/src/ffi_types.rs::FffCreateOptions`
const FFF_CREATE_OPTIONS_VERSION = 1;
const FFF_CREATE_OPTIONS_SIZE = 88;
const FCO_VERSION = 0;
const FCO_BASE_PATH = 8;
const FCO_FRECENCY_DB_PATH = 16;
const FCO_HISTORY_DB_PATH = 24;
const FCO_ENABLE_MMAP_CACHE = 32;
const FCO_ENABLE_CONTENT_INDEXING = 33;
const FCO_WATCH = 34;
const FCO_AI_MODE = 35;
const FCO_LOG_FILE_PATH = 40;
const FCO_LOG_LEVEL = 48;
const FCO_CACHE_BUDGET_MAX_FILES = 56;
const FCO_CACHE_BUDGET_MAX_BYTES = 64;
const FCO_CACHE_BUDGET_MAX_FILE_SIZE = 72;
const FCO_ENABLE_FS_ROOT_SCANNING = 80;
const FCO_ENABLE_HOME_DIR_SCANNING = 81;

function readResultEnvelope(
  resultPtr: Pointer | null,
): { success: true; handlePtr: number; intValue: number } | Result<never> {
  if (resultPtr === null) {
    return err("FFI returned null pointer");
  }

  const success = read.u8(resultPtr, RES_SUCCESS) !== 0;
  const library = loadLibrary();

  if (!success) {
    const errorPtr = read.ptr(resultPtr, RES_ERROR);
    const errorMsg = readCString(errorPtr) || "Unknown error";
    library.symbols.fff_free_result(resultPtr);
    return err(errorMsg);
  }

  const handlePtr = read.ptr(resultPtr, RES_HANDLE);
  const intValue = Number(read.i64(resultPtr, RES_INT_VALUE));
  library.symbols.fff_free_result(resultPtr);
  return { success: true, handlePtr, intValue };
}

/** Parse a FffResult that carries a bool in int_value (0 = false, nonzero = true). */
function parseBoolResult(resultPtr: Pointer | null): Result<boolean> {
  const envelope = readResultEnvelope(resultPtr);
  if (!("success" in envelope)) return envelope;
  return { ok: true, value: envelope.intValue !== 0 };
}

/** Parse a FffResult that carries an integer in int_value. */
function parseIntResult(resultPtr: Pointer | null): Result<number> {
  const envelope = readResultEnvelope(resultPtr);
  if (!("success" in envelope)) return envelope;
  return { ok: true, value: envelope.intValue };
}

/** Parse a FffResult that carries a string in handle (freed with fff_free_string). */
function parseStringResult(resultPtr: Pointer | null): Result<string | null> {
  const envelope = readResultEnvelope(resultPtr);
  if (!("success" in envelope)) return envelope;

  if (envelope.handlePtr === 0) return { ok: true, value: null };

  const library = loadLibrary();
  const str = readCString(envelope.handlePtr);
  library.symbols.fff_free_string(asPtr(envelope.handlePtr));
  return { ok: true, value: str };
}

/** Parse a FffResult that carries a JSON string in handle. */
function parseJsonResult<T>(resultPtr: Pointer | null): Result<T> {
  const envelope = readResultEnvelope(resultPtr);
  if (!("success" in envelope)) return envelope;

  if (envelope.handlePtr === 0) return { ok: true, value: undefined as T };

  const library = loadLibrary();
  const jsonStr = readCString(envelope.handlePtr);
  library.symbols.fff_free_string(asPtr(envelope.handlePtr));

  if (jsonStr === null || jsonStr === "") return { ok: true, value: undefined as T };

  try {
    return { ok: true, value: snakeToCamel(JSON.parse(jsonStr)) as T };
  } catch {
    return { ok: true, value: jsonStr as T };
  }
}

/** Parse a FffResult with no payload (void, success/error only). */
function parseVoidResult(resultPtr: Pointer | null): Result<void> {
  const envelope = readResultEnvelope(resultPtr);
  if (!("success" in envelope)) return envelope;
  return { ok: true, value: undefined };
}

/**
 * Opaque native handle type. Callers must not inspect or modify this value.
 */
export type NativeHandle = Pointer;

/**
 * Create a new file finder instance.
 *
 * Hand-encodes a [`FffCreateOptions`] struct (88 bytes, locked offsets — see
 * `crates/fff-c/src/ffi_types.rs::options_layout_tests`) into a Buffer and
 * passes its pointer to `fff_create_instance_with`. Inner cstring addresses
 * come from Bun's native `ptr(buffer)` primitive — no round-trip helpers,
 * no struct support gaps.
 *
 * Adding new options later means: (1) appending the field to
 * `FffCreateOptions` in Rust, (2) bumping `FFF_CREATE_OPTIONS_VERSION`,
 * (3) extending `FFF_CREATE_OPTIONS_SIZE` + offsets here. The C entry point
 * never changes.
 */
export function ffiCreate(
  basePath: string,
  frecencyDbPath: string,
  historyDbPath: string,
  _useUnsafeNoLock: boolean,
  enableMmapCache: boolean,
  enableContentIndexing: boolean,
  watch: boolean,
  aiMode: boolean,
  logFilePath: string,
  logLevel: string,
  cacheBudgetMaxFiles: bigint,
  cacheBudgetMaxBytes: bigint,
  cacheBudgetMaxFileSize: bigint,
  enableFsRootScanning: boolean,
  enableHomeDirScanning: boolean,
): Result<NativeHandle> {
  const library = loadLibrary();

  // Keep cstring buffers alive across the FFI call. Bun's `ptr()` returns
  // the underlying memory address of each Buffer — no round-trips.
  const basePathCStr = encodeCStringBuf(basePath);
  const frecencyCStr = encodeCStringBuf(frecencyDbPath);
  const historyCStr = encodeCStringBuf(historyDbPath);
  const logFileCStr = encodeCStringBuf(logFilePath);
  const logLevelCStr = encodeCStringBuf(logLevel);

  const opts = Buffer.alloc(FFF_CREATE_OPTIONS_SIZE);
  opts.writeUInt32LE(FFF_CREATE_OPTIONS_VERSION, FCO_VERSION);
  writePtrLE(opts, FCO_BASE_PATH, basePathCStr);
  writePtrLE(opts, FCO_FRECENCY_DB_PATH, frecencyCStr);
  writePtrLE(opts, FCO_HISTORY_DB_PATH, historyCStr);
  opts.writeUInt8(enableMmapCache ? 1 : 0, FCO_ENABLE_MMAP_CACHE);
  opts.writeUInt8(enableContentIndexing ? 1 : 0, FCO_ENABLE_CONTENT_INDEXING);
  opts.writeUInt8(watch ? 1 : 0, FCO_WATCH);
  opts.writeUInt8(aiMode ? 1 : 0, FCO_AI_MODE);
  writePtrLE(opts, FCO_LOG_FILE_PATH, logFileCStr);
  writePtrLE(opts, FCO_LOG_LEVEL, logLevelCStr);
  opts.writeBigUInt64LE(cacheBudgetMaxFiles, FCO_CACHE_BUDGET_MAX_FILES);
  opts.writeBigUInt64LE(cacheBudgetMaxBytes, FCO_CACHE_BUDGET_MAX_BYTES);
  opts.writeBigUInt64LE(cacheBudgetMaxFileSize, FCO_CACHE_BUDGET_MAX_FILE_SIZE);
  opts.writeUInt8(enableFsRootScanning ? 1 : 0, FCO_ENABLE_FS_ROOT_SCANNING);
  opts.writeUInt8(enableHomeDirScanning ? 1 : 0, FCO_ENABLE_HOME_DIR_SCANNING);

  const resultPtr = library.symbols.fff_create_instance_with(ptr(opts));

  if (resultPtr === null) {
    return err("FFI returned null pointer");
  }

  const success = read.u8(resultPtr, RES_SUCCESS) !== 0;

  if (success) {
    const handlePtr = read.ptr(resultPtr, RES_HANDLE);
    const handle = handlePtr as unknown as Pointer;
    library.symbols.fff_free_result(resultPtr);

    if (!handle || handle === (0 as unknown as Pointer)) {
      return err("fff_create_instance_with returned null handle");
    }

    return { ok: true, value: handle };
  } else {
    const errorPtr = read.ptr(resultPtr, RES_ERROR);
    const errorMsg = readCString(errorPtr) || "Unknown error";
    library.symbols.fff_free_result(resultPtr);
    return err(errorMsg);
  }
}

/** NUL-terminated UTF-8 buffer for `s`, or `null` for empty input. */
function encodeCStringBuf(s: string | null | undefined): Buffer | null {
  if (!s) return null;
  return Buffer.from(s + "\0", "utf-8");
}

/** Write Bun's native pointer-to-buffer address into the options buffer. */
function writePtrLE(buf: Buffer, offset: number, target: Buffer | null): void {
  if (target == null) {
    buf.writeBigUInt64LE(0n, offset);
    return;
  }
  buf.writeBigUInt64LE(BigInt(ptr(target) as unknown as number), offset);
}

/**
 * Destroy and clean up an instance.
 */
export function ffiDestroy(handle: NativeHandle): void {
  const library = loadLibrary();
  library.symbols.fff_destroy(handle);
}

// ---------------------------------------------------------------------------
// Struct byte offsets (must match #[repr(C)] layout on 64-bit)
// ---------------------------------------------------------------------------

// FffSearchResult { items: *mut, scores: *mut, count: u32, total_matched: u32, total_files: u32, location: FffLocation }
const SR_ITEMS = 0; // *mut FffFileItem (8)
const SR_SCORES = 8; // *mut FffScore    (8)
const SR_COUNT = 16; // u32              (4)
const SR_MATCHED = 20; // u32              (4)
const SR_TOTAL = 24; // u32              (4)
// FffLocation is inlined at offset 28
const SR_LOC_TAG = 28; // u8               (1 + 3 padding)
const SR_LOC_LINE = 32; // i32              (4)
const SR_LOC_COL = 36; // i32              (4)
const SR_LOC_END_LINE = 40; // i32           (4)
const SR_LOC_END_COL = 44; // i32           (4)

// FffFileItem (72 bytes)
const FI_RELPATH = 0; // *mut c_char (8)
const FI_FNAME = 8; // *mut c_char (8)
const FI_GIT = 16; // *mut c_char (8)
const FI_SIZE = 24; // u64         (8)
const FI_MODIFIED = 32; // u64         (8)
const FI_ACCESS = 40; // i64         (8)
const FI_MODFR = 48; // i64         (8)
const FI_TOTAL_FR = 56; // i64         (8)
const FI_SIZE_OF = 72;

// FffScore (48 bytes)
const SC_TOTAL = 0; // i32         (4)
const SC_BASE = 4; // i32         (4)
const SC_FNAME = 8; // i32         (4)
const SC_SPECIAL = 12; // i32         (4)
const SC_FREC = 16; // i32         (4)
const SC_DIST = 20; // i32         (4)
const SC_CURFILE = 24; // i32         (4)
const SC_COMBO = 28; // i32         (4)
const SC_EXACT = 32; // bool        (1 + 7 pad)
const SC_MTYPE = 40; // *mut c_char (8)
const SC_SIZE_OF = 48;

/** Cast a number (raw address from pointer math) to Pointer for read.*. */
function asPtr(n: number): Pointer {
  return n as unknown as Pointer;
}

/**
 * Read an FffFileItem struct at the given raw address.
 */
function readFileItemStruct(p: number): FileItem {
  const pp = asPtr(p);
  return {
    relativePath: readCString(read.ptr(pp, FI_RELPATH)) ?? "",
    fileName: readCString(read.ptr(pp, FI_FNAME)) ?? "",
    gitStatus: readCString(read.ptr(pp, FI_GIT)) ?? "",
    size: Number(read.u64(pp, FI_SIZE)),
    modified: Number(read.u64(pp, FI_MODIFIED)),
    accessFrecencyScore: Number(read.i64(pp, FI_ACCESS)),
    modificationFrecencyScore: Number(read.i64(pp, FI_MODFR)),
    totalFrecencyScore: Number(read.i64(pp, FI_TOTAL_FR)),
  };
}

/**
 * Read an FffScore struct at the given raw address.
 */
function readScoreStruct(p: number): Score {
  const pp = asPtr(p);
  return {
    total: read.i32(pp, SC_TOTAL),
    baseScore: read.i32(pp, SC_BASE),
    filenameBonus: read.i32(pp, SC_FNAME),
    specialFilenameBonus: read.i32(pp, SC_SPECIAL),
    frecencyBoost: read.i32(pp, SC_FREC),
    distancePenalty: read.i32(pp, SC_DIST),
    currentFilePenalty: read.i32(pp, SC_CURFILE),
    comboMatchBoost: read.i32(pp, SC_COMBO),
    exactMatch: read.u8(pp, SC_EXACT) !== 0,
    matchType: readCString(read.ptr(pp, SC_MTYPE)) ?? "",
  };
}

/**
 * Parse an FffSearchResult from a raw pointer, then free native memory.
 */
function parseSearchResult(resultPtr: Pointer | null): Result<SearchResult> {
  if (resultPtr === null) {
    return err("FFI returned null pointer");
  }

  const envelope = readResultEnvelope(resultPtr);
  if (!("success" in envelope)) return envelope;

  if (envelope.handlePtr === 0) {
    return err("fff_search returned null search result");
  }

  const hp = asPtr(envelope.handlePtr);
  const count = read.u32(hp, SR_COUNT);
  const totalMatched = read.u32(hp, SR_MATCHED);
  const totalFiles = read.u32(hp, SR_TOTAL);
  const itemsBase = read.ptr(hp, SR_ITEMS);
  const scoresBase = read.ptr(hp, SR_SCORES);

  // Read location
  const locTag = read.u8(hp, SR_LOC_TAG);
  let location: Location | undefined;
  if (locTag === 1) {
    location = { type: "line", line: read.i32(hp, SR_LOC_LINE) };
  } else if (locTag === 2) {
    location = {
      type: "position",
      line: read.i32(hp, SR_LOC_LINE),
      col: read.i32(hp, SR_LOC_COL),
    };
  } else if (locTag === 3) {
    location = {
      type: "range",
      start: { line: read.i32(hp, SR_LOC_LINE), col: read.i32(hp, SR_LOC_COL) },
      end: {
        line: read.i32(hp, SR_LOC_END_LINE),
        col: read.i32(hp, SR_LOC_END_COL),
      },
    };
  }

  // Read items and scores arrays using pointer arithmetic
  const items: FileItem[] = [];
  const scores: Score[] = [];

  for (let i = 0; i < count; i++) {
    items.push(readFileItemStruct(itemsBase + i * FI_SIZE_OF));
    scores.push(readScoreStruct(scoresBase + i * SC_SIZE_OF));
  }

  // Free native search result
  loadLibrary().symbols.fff_free_search_result(hp);

  const result: SearchResult = { items, scores, totalMatched, totalFiles };
  if (location) {
    result.location = location;
  }
  return { ok: true, value: result };
}

// ---------------------------------------------------------------------------
// FffDirSearchResult byte offsets (must match #[repr(C)] layout on 64-bit)
// { items: *mut, scores: *mut, count: u32, total_matched: u32, total_dirs: u32 }
// ---------------------------------------------------------------------------
const DSR_COUNT = 16; // u32              (4)
const DSR_MATCHED = 20; // u32              (4)
const DSR_TOTAL_DIRS = 24; // u32              (4)

// FffDirItem (24 bytes: 8 + 8 + 4 + 4pad)
const DI_RELPATH = 0; // *mut c_char (8)
const DI_DIRNAME = 8; // *mut c_char (8)
const DI_MAX_FRECENCY = 16; // i32         (4)

/**
 * Read an FffDirItem struct at the given raw address.
 */
function readDirItemStruct(p: number): DirItem {
  const pp = asPtr(p);
  return {
    relativePath: readCString(read.ptr(pp, DI_RELPATH)) ?? "",
    dirName: readCString(read.ptr(pp, DI_DIRNAME)) ?? "",
    maxAccessFrecency: read.i32(pp, DI_MAX_FRECENCY),
  };
}

/**
 * Parse an FffDirSearchResult from a raw FffResult pointer, then free native memory.
 */
function parseDirSearchResult(resultPtr: Pointer | null): Result<DirSearchResult> {
  if (resultPtr === null) {
    return err("FFI returned null pointer");
  }

  const envelope = readResultEnvelope(resultPtr);
  if (!("success" in envelope)) return envelope;

  if (envelope.handlePtr === 0) {
    return err("fff_search_directories returned null search result");
  }

  const hp = asPtr(envelope.handlePtr);
  const count = read.u32(hp, DSR_COUNT);
  const totalMatched = read.u32(hp, DSR_MATCHED);
  const totalDirs = read.u32(hp, DSR_TOTAL_DIRS);

  const library = loadLibrary();

  const items: DirItem[] = [];
  const scores: Score[] = [];

  for (let i = 0; i < count; i++) {
    const itemPtr = library.symbols.fff_dir_search_result_get_item(hp, i);
    if (itemPtr !== null && (itemPtr as unknown as number) !== 0) {
      items.push(readDirItemStruct(itemPtr as unknown as number));
    }
    const scorePtr = library.symbols.fff_dir_search_result_get_score(hp, i);
    if (scorePtr !== null && (scorePtr as unknown as number) !== 0) {
      scores.push(readScoreStruct(scorePtr as unknown as number));
    }
  }

  // Free native dir search result
  library.symbols.fff_free_dir_search_result(hp);

  return { ok: true, value: { items, scores, totalMatched, totalDirs } };
}

// ---------------------------------------------------------------------------
// FffMixedSearchResult byte offsets (must match #[repr(C)] layout on 64-bit)
// { items: *mut, scores: *mut, count: u32, total_matched: u32, total_files: u32, total_dirs: u32, location: FffLocation }
// ---------------------------------------------------------------------------
const MSR_COUNT = 16; // u32               (4)
const MSR_MATCHED = 20; // u32               (4)
const MSR_TOTAL_FILES = 24; // u32               (4)
const MSR_TOTAL_DIRS = 28; // u32               (4)
// FffLocation is inlined at offset 32
const MSR_LOC_TAG = 32; // u8                (1 + 3 padding)
const MSR_LOC_LINE = 36; // i32               (4)
const MSR_LOC_COL = 40; // i32               (4)
const MSR_LOC_END_LINE = 44; // i32               (4)
const MSR_LOC_END_COL = 48; // i32               (4)

// FffMixedItem (80 bytes)
const MI_TYPE = 0; // u8          (1 + 7 pad)
const MI_RELPATH = 8; // *mut c_char (8)
const MI_DISPLAY = 16; // *mut c_char (8)
const MI_GIT = 24; // *mut c_char (8)
const MI_SIZE = 32; // u64         (8)
const MI_MODIFIED = 40; // u64         (8)
const MI_ACCESS = 48; // i64         (8)
const MI_MODFR = 56; // i64         (8)
const MI_TOTAL_FR = 64; // i64         (8)

/**
 * Read an FffMixedItem struct at the given raw address and return a MixedItem.
 */
function readMixedItemStruct(p: number): MixedItem {
  const pp = asPtr(p);
  const itemType = read.u8(pp, MI_TYPE);

  if (itemType === 1) {
    // Directory
    return {
      type: "directory",
      item: {
        relativePath: readCString(read.ptr(pp, MI_RELPATH)) ?? "",
        dirName: readCString(read.ptr(pp, MI_DISPLAY)) ?? "",
        maxAccessFrecency: Number(read.i64(pp, MI_ACCESS)),
      },
    };
  }

  // File (itemType === 0)
  return {
    type: "file",
    item: {
      relativePath: readCString(read.ptr(pp, MI_RELPATH)) ?? "",
      fileName: readCString(read.ptr(pp, MI_DISPLAY)) ?? "",
      gitStatus: readCString(read.ptr(pp, MI_GIT)) ?? "",
      size: Number(read.u64(pp, MI_SIZE)),
      modified: Number(read.u64(pp, MI_MODIFIED)),
      accessFrecencyScore: Number(read.i64(pp, MI_ACCESS)),
      modificationFrecencyScore: Number(read.i64(pp, MI_MODFR)),
      totalFrecencyScore: Number(read.i64(pp, MI_TOTAL_FR)),
    },
  };
}

/**
 * Parse an FffMixedSearchResult from a raw FffResult pointer, then free native memory.
 */
function parseMixedSearchResult(resultPtr: Pointer | null): Result<MixedSearchResult> {
  if (resultPtr === null) {
    return err("FFI returned null pointer");
  }

  const envelope = readResultEnvelope(resultPtr);
  if (!("success" in envelope)) return envelope;

  if (envelope.handlePtr === 0) {
    return err("fff_search_mixed returned null search result");
  }

  const hp = asPtr(envelope.handlePtr);
  const count = read.u32(hp, MSR_COUNT);
  const totalMatched = read.u32(hp, MSR_MATCHED);
  const totalFiles = read.u32(hp, MSR_TOTAL_FILES);
  const totalDirs = read.u32(hp, MSR_TOTAL_DIRS);

  // Read location
  const locTag = read.u8(hp, MSR_LOC_TAG);
  let location: Location | undefined;
  if (locTag === 1) {
    location = { type: "line", line: read.i32(hp, MSR_LOC_LINE) };
  } else if (locTag === 2) {
    location = {
      type: "position",
      line: read.i32(hp, MSR_LOC_LINE),
      col: read.i32(hp, MSR_LOC_COL),
    };
  } else if (locTag === 3) {
    location = {
      type: "range",
      start: {
        line: read.i32(hp, MSR_LOC_LINE),
        col: read.i32(hp, MSR_LOC_COL),
      },
      end: {
        line: read.i32(hp, MSR_LOC_END_LINE),
        col: read.i32(hp, MSR_LOC_END_COL),
      },
    };
  }

  const library = loadLibrary();

  const items: MixedItem[] = [];
  const scores: Score[] = [];

  for (let i = 0; i < count; i++) {
    const itemPtr = library.symbols.fff_mixed_search_result_get_item(hp, i);
    if (itemPtr !== null && (itemPtr as unknown as number) !== 0) {
      items.push(readMixedItemStruct(itemPtr as unknown as number));
    }
    const scorePtr = library.symbols.fff_mixed_search_result_get_score(hp, i);
    if (scorePtr !== null && (scorePtr as unknown as number) !== 0) {
      scores.push(readScoreStruct(scorePtr as unknown as number));
    }
  }

  // Free native mixed search result
  library.symbols.fff_free_mixed_search_result(hp);

  const result: MixedSearchResult = {
    items,
    scores,
    totalMatched,
    totalFiles,
    totalDirs,
  };
  if (location) {
    result.location = location;
  }
  return { ok: true, value: result };
}

// ---------------------------------------------------------------------------
// FffGrepMatch byte offsets (must match #[repr(C)] layout on 64-bit)
// ---------------------------------------------------------------------------

// Pointers (8 bytes each)
const GM_RELPATH = 0;
const GM_FNAME = 8;
const GM_GIT = 16;
const GM_LINE_CONTENT = 24;
const GM_MATCH_RANGES = 32;
const GM_CTX_BEFORE = 40;
const GM_CTX_AFTER = 48;

// 8-byte numeric fields
const GM_SIZE = 56;
const GM_MODIFIED = 64;
const GM_TOTAL_FR = 72;
const GM_ACCESS_FR = 80;
const GM_MOD_FR = 88;
const GM_LINE_NUM = 96;
const GM_BYTE_OFF = 104;

// 4-byte fields
const GM_COL = 112;
const GM_MR_COUNT = 116;
const GM_CTX_B_COUNT = 120;
const GM_CTX_A_COUNT = 124;

// 2-byte
const GM_FUZZY_SCORE = 128;
// 1-byte
const GM_HAS_FUZZY = 130;
const GM_IS_BINARY = 131;
const GM_IS_DEFINITION = 132;

// struct size: pad to 8-byte alignment → 136
const GM_SIZE_OF = 136;

// FffGrepResult
const GR_ITEMS = 0; // *mut FffGrepMatch (8)
const GR_COUNT = 8; // u32 (4)
const GR_MATCHED = 12; // u32 (4)
const GR_FILES_SEARCHED = 16; // u32 (4)
const GR_TOTAL_FILES = 20; // u32 (4)
const GR_FILTERED = 24; // u32 (4)
const GR_NEXT_OFFSET = 28; // u32 (4)
const GR_REGEX_ERR = 32; // *mut c_char (8)

// FffMatchRange (8 bytes)
const MR_START = 0;
const MR_END = 4;
const MR_SIZE = 8;

/**
 * Read a C string array (char**) at the given pointer, with `count` elements.
 */
function readCStringArray(base: number, count: number): string[] {
  if (count === 0 || base === 0) return [];
  const result: string[] = [];
  const bp = asPtr(base);
  for (let i = 0; i < count; i++) {
    const strPtr = read.ptr(bp, i * 8); // each pointer is 8 bytes
    result.push(readCString(strPtr) ?? "");
  }
  return result;
}

/**
 * Read an FffGrepMatch struct at the given raw address.
 */
function readGrepMatchStruct(p: number): GrepMatch {
  const pp = asPtr(p);
  const matchRangesPtr = read.ptr(pp, GM_MATCH_RANGES);
  const matchRangesCount = read.u32(pp, GM_MR_COUNT);
  const matchRanges: [number, number][] = [];
  for (let i = 0; i < matchRangesCount; i++) {
    const base = matchRangesPtr + i * MR_SIZE;
    const bp = asPtr(base);
    matchRanges.push([read.u32(bp, MR_START), read.u32(bp, MR_END)]);
  }

  const hasFuzzy = read.u8(pp, GM_HAS_FUZZY) !== 0;
  const ctxBeforeCount = read.u32(pp, GM_CTX_B_COUNT);
  const ctxAfterCount = read.u32(pp, GM_CTX_A_COUNT);

  const match: GrepMatch = {
    relativePath: readCString(read.ptr(pp, GM_RELPATH)) ?? "",
    fileName: readCString(read.ptr(pp, GM_FNAME)) ?? "",
    gitStatus: readCString(read.ptr(pp, GM_GIT)) ?? "",
    lineContent: readCString(read.ptr(pp, GM_LINE_CONTENT)) ?? "",
    size: Number(read.u64(pp, GM_SIZE)),
    modified: Number(read.u64(pp, GM_MODIFIED)),
    totalFrecencyScore: Number(read.i64(pp, GM_TOTAL_FR)),
    accessFrecencyScore: Number(read.i64(pp, GM_ACCESS_FR)),
    modificationFrecencyScore: Number(read.i64(pp, GM_MOD_FR)),
    isBinary: read.u8(pp, GM_IS_BINARY) !== 0,
    lineNumber: Number(read.u64(pp, GM_LINE_NUM)),
    col: read.u32(pp, GM_COL),
    byteOffset: Number(read.u64(pp, GM_BYTE_OFF)),
    matchRanges,
  };

  if (hasFuzzy) {
    match.fuzzyScore = read.u16(pp, GM_FUZZY_SCORE);
  }
  if (ctxBeforeCount > 0) {
    match.contextBefore = readCStringArray(read.ptr(pp, GM_CTX_BEFORE), ctxBeforeCount);
  }
  if (ctxAfterCount > 0) {
    match.contextAfter = readCStringArray(read.ptr(pp, GM_CTX_AFTER), ctxAfterCount);
  }
  if (read.u8(pp, GM_IS_DEFINITION) !== 0) {
    match.isDefinition = true;
  }

  return match;
}

/**
 * Parse an FffGrepResult from a raw FffResult pointer, then free native memory.
 */
function parseGrepResult(resultPtr: Pointer | null): Result<GrepResult> {
  const envelope = readResultEnvelope(resultPtr);
  if (!("success" in envelope)) return envelope;

  if (envelope.handlePtr === 0) {
    return err("grep returned null result");
  }

  const hp = asPtr(envelope.handlePtr);
  const count = read.u32(hp, GR_COUNT);
  const totalMatched = read.u32(hp, GR_MATCHED);
  const totalFilesSearched = read.u32(hp, GR_FILES_SEARCHED);
  const totalFiles = read.u32(hp, GR_TOTAL_FILES);
  const filteredFileCount = read.u32(hp, GR_FILTERED);
  const nextFileOffset = read.u32(hp, GR_NEXT_OFFSET);
  const regexErrPtr = read.ptr(hp, GR_REGEX_ERR);
  const regexFallbackError = readCString(regexErrPtr) ?? undefined;
  const itemsBase = read.ptr(hp, GR_ITEMS);

  const items: GrepMatch[] = [];
  for (let i = 0; i < count; i++) {
    items.push(readGrepMatchStruct(itemsBase + i * GM_SIZE_OF));
  }

  loadLibrary().symbols.fff_free_grep_result(hp);

  const grepResult: GrepResult = {
    items,
    totalMatched,
    totalFilesSearched,
    totalFiles,
    filteredFileCount,
    nextCursor: nextFileOffset > 0 ? createGrepCursor(nextFileOffset) : null,
  };
  if (regexFallbackError) {
    grepResult.regexFallbackError = regexFallbackError;
  }
  return { ok: true, value: grepResult };
}

/**
 * Perform fuzzy search.
 */
export function ffiSearch(
  handle: NativeHandle,
  query: string,
  currentFile: string,
  maxThreads: number,
  pageIndex: number,
  pageSize: number,
  comboBoostMultiplier: number,
  minComboCount: number,
): Result<SearchResult> {
  const library = loadLibrary();
  const resultPtr = library.symbols.fff_search(
    handle,
    ptr(encodeString(query)),
    ptr(encodeString(currentFile)),
    maxThreads,
    pageIndex,
    pageSize,
    comboBoostMultiplier,
    minComboCount,
  );
  return parseSearchResult(resultPtr);
}

/**
 * Glob-only search. Bypasses the regular query parser, applies the pattern
 * as a single `Constraint::Glob`, ranks by frecency, paginates.
 */
export function ffiGlob(
  handle: NativeHandle,
  pattern: string,
  currentFile: string,
  maxThreads: number,
  pageIndex: number,
  pageSize: number,
): Result<SearchResult> {
  const library = loadLibrary();
  const resultPtr = library.symbols.fff_glob(
    handle,
    ptr(encodeString(pattern)),
    ptr(encodeString(currentFile)),
    maxThreads,
    pageIndex,
    pageSize,
  );
  return parseSearchResult(resultPtr);
}

/**
 * Perform fuzzy directory search.
 */
export function ffiSearchDirectories(
  handle: NativeHandle,
  query: string,
  currentFile: string | null,
  maxThreads: number,
  pageIndex: number,
  pageSize: number,
): Result<DirSearchResult> {
  const library = loadLibrary();
  const resultPtr = library.symbols.fff_search_directories(
    handle,
    ptr(encodeString(query)),
    ptr(encodeString(currentFile ?? "")),
    maxThreads,
    pageIndex,
    pageSize,
  );
  return parseDirSearchResult(resultPtr);
}

/**
 * Perform mixed (files + directories) fuzzy search.
 */
export function ffiSearchMixed(
  handle: NativeHandle,
  query: string,
  currentFile: string,
  maxThreads: number,
  pageIndex: number,
  pageSize: number,
  comboBoostMultiplier: number,
  minComboCount: number,
): Result<MixedSearchResult> {
  const library = loadLibrary();
  const resultPtr = library.symbols.fff_search_mixed(
    handle,
    ptr(encodeString(query)),
    ptr(encodeString(currentFile)),
    maxThreads,
    pageIndex,
    pageSize,
    comboBoostMultiplier,
    minComboCount,
  );
  return parseMixedSearchResult(resultPtr);
}

/**
 * Live grep - search file contents.
 */
export function ffiLiveGrep(
  handle: NativeHandle,
  query: string,
  mode: string,
  maxFileSize: number,
  maxMatchesPerFile: number,
  smartCase: boolean,
  fileOffset: number,
  pageLimit: number,
  timeBudgetMs: number,
  beforeContext: number,
  afterContext: number,
  classifyDefinitions: boolean,
): Result<GrepResult> {
  const library = loadLibrary();
  const resultPtr = library.symbols.fff_live_grep(
    handle,
    ptr(encodeString(query)),
    grepModeToU8(mode),
    BigInt(maxFileSize),
    maxMatchesPerFile,
    smartCase,
    fileOffset,
    pageLimit,
    BigInt(timeBudgetMs),
    beforeContext,
    afterContext,
    classifyDefinitions,
  );
  return parseGrepResult(resultPtr);
}

/**
 * Multi-pattern grep - Aho-Corasick multi-needle search.
 */
export function ffiMultiGrep(
  handle: NativeHandle,
  patternsJoined: string,
  constraints: string,
  maxFileSize: number,
  maxMatchesPerFile: number,
  smartCase: boolean,
  fileOffset: number,
  pageLimit: number,
  timeBudgetMs: number,
  beforeContext: number,
  afterContext: number,
  classifyDefinitions: boolean,
): Result<GrepResult> {
  const library = loadLibrary();
  const resultPtr = library.symbols.fff_multi_grep(
    handle,
    ptr(encodeString(patternsJoined)),
    ptr(encodeString(constraints)),
    BigInt(maxFileSize),
    maxMatchesPerFile,
    smartCase,
    fileOffset,
    pageLimit,
    BigInt(timeBudgetMs),
    beforeContext,
    afterContext,
    classifyDefinitions,
  );
  return parseGrepResult(resultPtr);
}

/**
 * Trigger file scan.
 */
export function ffiScanFiles(handle: NativeHandle): Result<void> {
  const library = loadLibrary();
  const resultPtr = library.symbols.fff_scan_files(handle);
  return parseVoidResult(resultPtr);
}

/**
 * Check if scanning.
 */
export function ffiIsScanning(handle: NativeHandle): boolean {
  const library = loadLibrary();
  return library.symbols.fff_is_scanning(handle) as boolean;
}

/**
 * Get the base path of the file picker.
 */
export function ffiGetBasePath(handle: NativeHandle): Result<string | null> {
  const library = loadLibrary();
  const resultPtr = library.symbols.fff_get_base_path(handle);
  return parseStringResult(resultPtr);
}

// FffScanProgress { scanned_files_count: u64(8), is_scanning: bool(1), is_watcher_ready: bool(1), is_warmup_complete: bool(1) + pad }
const SP_COUNT = 0; // u64 (8)
const SP_SCANNING = 8; // bool (1)
const SP_WATCHER_READY = 9; // bool (1)
const SP_WARMUP_COMPLETE = 10; // bool (1)

/**
 * Get scan progress.
 */
export function ffiGetScanProgress(handle: NativeHandle): Result<ScanProgress> {
  const library = loadLibrary();
  const resultPtr = library.symbols.fff_get_scan_progress(handle);
  const envelope = readResultEnvelope(resultPtr);
  if (!("success" in envelope)) return envelope;

  if (envelope.handlePtr === 0) {
    return err("scan progress returned null");
  }

  const hp = asPtr(envelope.handlePtr);
  const result: ScanProgress = {
    scannedFilesCount: Number(read.u64(hp, SP_COUNT)),
    isScanning: read.u8(hp, SP_SCANNING) !== 0,
    isWatcherReady: read.u8(hp, SP_WATCHER_READY) !== 0,
    isWarmupComplete: read.u8(hp, SP_WARMUP_COMPLETE) !== 0,
  };
  library.symbols.fff_free_scan_progress(hp);
  return { ok: true, value: result };
}

/**
 * Wait for scan to complete.
 */
export function ffiWaitForScan(handle: NativeHandle, timeoutMs: number): Result<boolean> {
  const library = loadLibrary();
  const resultPtr = library.symbols.fff_wait_for_scan(handle, BigInt(timeoutMs));
  return parseBoolResult(resultPtr);
}

/**
 * Restart index in new path.
 */
export function ffiRestartIndex(handle: NativeHandle, newPath: string): Result<void> {
  const library = loadLibrary();
  const resultPtr = library.symbols.fff_restart_index(handle, ptr(encodeString(newPath)));
  return parseVoidResult(resultPtr);
}

// ---------------------------------------------------------------------------
// Watch struct byte offsets (must match #[repr(C)] layout on 64-bit)
// ---------------------------------------------------------------------------

// MUST match `crates/fff-c`::FffWatchOptions
const FFF_WATCH_OPTIONS_VERSION = 1;
const FFF_WATCH_OPTIONS_SIZE = 24;
const FWO_VERSION = 0; // u32 (4 + 4 pad)
const FWO_IGNORE = 8; // *const *const c_char (8)
const FWO_IGNORE_COUNT = 16; // u32 (4 + 4 pad)

/** kind u8 -> WatchEventKind. Unknown values degrade to "rescan". */
const WATCH_EVENT_KINDS: readonly WatchEventKind[] = [
  "created",
  "modified",
  "removed",
  "rescan",
  "renamed",
];

/**
 * Parse an FffWatchEventBatch delivered to a watch callback, then free it.
 * Ownership of the batch transfers to JS at callback time, so this MUST be
 * called exactly once per delivered batch pointer.
 */
export function readWatchEventBatch(batchPtr: Pointer | number | null): WatchEvent[] {
  if (batchPtr === null || (batchPtr as unknown as number) === 0) {
    return [];
  }

  const bp = batchPtr as unknown as Pointer;
  const symbols = loadLibrary().symbols;
  const count = symbols.fff_watch_events_count(bp);

  const events: WatchEvent[] = [];
  for (let i = 0; i < count; i++) {
    const path = symbols.fff_watch_events_get_path(bp, i) as Pointer | null;
    const kind = symbols.fff_watch_events_get_kind(bp, i) as number;
    const event: WatchEvent = {
      path: readCString(path) ?? "",
      kind: WATCH_EVENT_KINDS[kind] ?? "rescan",
    };
    if (event.kind === "renamed") {
      const from = symbols.fff_watch_events_get_from_path(bp, i) as Pointer | null;
      const decoded = readCString(from);
      if (decoded) event.from = decoded;
    }
    events.push(event);
  }

  symbols.fff_free_watch_events(bp);
  return events;
}

/**
 * Register the instance-wide watch callback. Must be called before the
 * first `ffiWatch`. The caller owns `callback` (a threadsafe `JSCallback`
 * built in finder.ts) and must keep it alive until after `ffiDestroy`
 * returns for this handle — that call is the delivery quiescence barrier.
 */
export function ffiSetWatchCallback(
  handle: NativeHandle,
  callback: JSCallback,
): Result<void> {
  const library = loadLibrary();
  if (callback.ptr === null) {
    return err("watch callback has been closed");
  }
  const resultPtr = library.symbols.fff_set_watch_callback(handle, callback.ptr, null);
  return parseVoidResult(resultPtr);
}

/**
 * Subscribe to filesystem changes; batches are delivered through the
 * instance callback registered with `ffiSetWatchCallback`, tagged with the
 * watch id this function returns.
 *
 * @returns The native watch id carried in `FffResult.int_value`.
 */
export function ffiWatch(
  handle: NativeHandle,
  pattern: string,
  ignore: string[] = [],
): Result<number> {
  const library = loadLibrary();

  // Keep every buffer referenced until the FFI call returns: the options
  // struct, the pointer array, and each encoded ignore string.
  const opts = Buffer.alloc(FFF_WATCH_OPTIONS_SIZE);
  opts.writeUInt32LE(FFF_WATCH_OPTIONS_VERSION, FWO_VERSION);

  const ignoreBuffers = ignore.map((entry) => encodeString(entry));
  const ignorePtrs = Buffer.alloc(Math.max(ignoreBuffers.length, 1) * 8);
  for (let i = 0; i < ignoreBuffers.length; i++) {
    ignorePtrs.writeBigUInt64LE(BigInt(ptr(ignoreBuffers[i] as Uint8Array)), i * 8);
  }
  opts.writeBigUInt64LE(
    ignoreBuffers.length > 0 ? BigInt(ptr(ignorePtrs)) : 0n,
    FWO_IGNORE,
  );
  opts.writeUInt32LE(ignoreBuffers.length, FWO_IGNORE_COUNT);

  const resultPtr = library.symbols.fff_watch(
    handle,
    ptr(encodeString(pattern)),
    ptr(opts),
  );
  return parseIntResult(resultPtr);
}

/**
 * Remove a watch subscription. Returns true if the id was found.
 */
export function ffiUnwatch(handle: NativeHandle, watchId: number): Result<boolean> {
  const library = loadLibrary();
  const resultPtr = library.symbols.fff_unwatch(handle, BigInt(watchId));
  return parseBoolResult(resultPtr);
}

/**
 * Refresh git status.
 */
export function ffiRefreshGitStatus(handle: NativeHandle): Result<number> {
  const library = loadLibrary();
  const resultPtr = library.symbols.fff_refresh_git_status(handle);
  return parseIntResult(resultPtr);
}

/**
 * Track query completion.
 */
export function ffiTrackQuery(
  handle: NativeHandle,
  query: string,
  filePath: string,
): Result<boolean> {
  const library = loadLibrary();
  const resultPtr = library.symbols.fff_track_query(
    handle,
    ptr(encodeString(query)),
    ptr(encodeString(filePath)),
  );
  return parseBoolResult(resultPtr);
}

/**
 * Get historical query.
 */
export function ffiGetHistoricalQuery(
  handle: NativeHandle,
  offset: number,
): Result<string | null> {
  const library = loadLibrary();
  const resultPtr = library.symbols.fff_get_historical_query(handle, BigInt(offset));
  return parseStringResult(resultPtr);
}

/**
 * Health check.
 *
 * `handle` can be null for a limited check (version + git only).
 */
export function ffiHealthCheck(
  handle: NativeHandle | null,
  testPath: string,
): Result<unknown> {
  const library = loadLibrary();
  const resultPtr = library.symbols.fff_health_check(
    handle ?? (0 as unknown as Pointer),
    ptr(encodeString(testPath)),
  );
  return parseJsonResult<unknown>(resultPtr);
}

/**
 * Ensure the library is loaded.
 *
 * Loads the native library from the platform-specific npm package
 * or a local dev build. Throws if the binary is not found.
 */
export function ensureLoaded(): void {
  loadLibrary();
}

/**
 * Check if the library is available.
 */
export function isAvailable(): boolean {
  try {
    loadLibrary();
    return true;
  } catch {
    return false;
  }
}
