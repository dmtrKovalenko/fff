/**
 * Node.js FFI bindings for the fff-c native library using ffi-rs
 *
 * This module uses ffi-rs to call into the Rust C library.
 * All functions follow the Result pattern for error handling.
 *
 * The API is instance-based: `ffiCreate` returns an opaque handle that must
 * be passed to all subsequent calls and freed with `ffiDestroy`.
 *
 * ## Memory management
 *
 * Every `fff_*` function returning `*mut FffResult` allocates with Rust's Box.
 * We MUST call `fff_free_result` to properly deallocate (not libc::free).
 *
 * ## FffResult struct reading
 *
 * The FffResult struct layout (#[repr(C)]):
 *   offset  0: success (bool, 1 byte + 7 padding)
 *   offset  8: data pointer (8 bytes) - *mut c_char (JSON string or null)
 *   offset 16: error pointer (8 bytes) - *mut c_char (error message or null)
 *   offset 24: handle pointer (8 bytes) - *mut c_void (instance handle or null)
 *
 * ## Two-step approach for reading + freeing
 *
 * ffi-rs auto-dereferences struct retType pointers, losing the original pointer.
 * We solve this by:
 * 1. Calling the C function with `retType: DataType.External` to get the raw pointer
 * 2. Using `restorePointer` to read the struct fields from the raw pointer
 * 3. Calling `fff_free_result` with the original raw pointer
 *
 * ## Null pointer detection
 *
 * `isNullPointer` from ffi-rs correctly detects null C pointers wrapped as
 * V8 External objects. We use this instead of truthy checks.
 */

import {
  close,
  createPointer,
  DataType,
  type FieldType,
  freePointer,
  funcConstructor,
  isNullPointer,
  type JsExternal,
  load,
  open,
  PointerType,
  restorePointer,
  unwrapPointer,
  wrapPointer,
} from "ffi-rs";
import { findBinary } from "./binary.js";
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
  Score,
  SearchResult,
  WatchEvent,
  WatchEventKind,
} from "./fff-api.js";
import { createGrepCursor, err } from "./fff-api.js";

const LIBRARY_KEY = "fff_c";

const FFF_CREATE_OPTIONS_STRUCT = {
  version: DataType.U32,
  base_path: DataType.String,
  frecency_db_path: DataType.String,
  history_db_path: DataType.String,
  enable_mmap_cache: DataType.U8,
  enable_content_indexing: DataType.U8,
  watch: DataType.U8,
  ai_mode: DataType.U8,
  log_file_path: DataType.String,
  log_level: DataType.String,
  cache_budget_max_files: DataType.U64,
  cache_budget_max_bytes: DataType.U64,
  cache_budget_max_file_size: DataType.U64,
  enable_fs_root_scanning: DataType.U8,
  enable_home_dir_scanning: DataType.U8,
  follow_symlinks: DataType.U8,
};

// ALWAYS KEEP IN SYNC WITH fff.h
const FFF_CREATE_OPTIONS_VERSION = 2;

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

// Track whether the library is loaded
let isLoaded = false;

/**
 * Struct type definition for FffResult used with restorePointer.
 *
 * Uses U8 for the bool success field (correct alignment with ffi-rs).
 * Uses External for ALL pointer fields to avoid hangs on null char* pointers
 * (ffi-rs hangs when trying to read DataType.String from null char*).
 */
const FFF_RESULT_STRUCT = {
  success: DataType.U8,
  error: DataType.External,
  handle: DataType.External,
  int_value: DataType.I64,
};

interface FffResultRaw {
  success: number;
  error: JsExternal;
  handle: JsExternal;
  int_value: number;
}

/**
 * Load the native library using ffi-rs
 */
function loadLibrary(): void {
  if (isLoaded) return;

  const binaryPath = findBinary();
  if (!binaryPath) {
    throw new Error(
      "fff native library not found. Run `npx @ff-labs/fff-node download` or build from source with `cargo build --release -p fff-c`",
    );
  }

  open({ library: LIBRARY_KEY, path: binaryPath });
  isLoaded = true;
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
    const camelKey = key.replace(/_([a-z])/g, (_, letter: string) =>
      letter.toUpperCase(),
    );
    result[camelKey] = snakeToCamel(value);
  }
  return result;
}

/**
 * Read a C string (char*) from an ffi-rs External pointer.
 *
 * Uses restorePointer + wrapPointer to dereference the char* and read the
 * null-terminated string. Returns null if the pointer is null.
 */
function readCString(ptr: JsExternal): string | null {
  if (isNullPointer(ptr)) return null;
  try {
    const [str] = restorePointer({
      retType: [DataType.String],
      paramsValue: wrapPointer([ptr]),
    });
    return str as string;
  } catch {
    return null;
  }
}

/**
 * Call a C function that returns `*mut FffResult` and get both the raw pointer
 * (for freeing) and the parsed struct fields.
 *
 * Step 1: Call function with `DataType.External` retType → raw pointer
 * Step 2: Use `restorePointer` to read struct fields from the raw pointer
 */
function callRaw(
  funcName: string,
  paramsType: FieldType[],
  paramsValue: unknown[],
): { rawPtr: JsExternal; struct: FffResultRaw } {
  const rawPtr = load({
    library: LIBRARY_KEY,
    funcName,
    retType: DataType.External,
    paramsType,
    paramsValue,
    freeResultMemory: false,
  }) as JsExternal;

  const [structData] = restorePointer({
    retType: [FFF_RESULT_STRUCT],
    paramsValue: wrapPointer([rawPtr]),
  }) as unknown as [FffResultRaw];

  return { rawPtr, struct: structData };
}

/**
 * Free a FffResult pointer by calling fff_free_result.
 *
 * This frees the FffResult struct and its data/error strings using Rust's
 * Box::from_raw and CString::from_raw. The handle field is NOT freed.
 */
function freeResult(resultPtr: JsExternal): void {
  try {
    load({
      library: LIBRARY_KEY,
      funcName: "fff_free_result",
      retType: DataType.Void,
      paramsType: [DataType.External],
      paramsValue: [resultPtr],
    });
  } catch {
    // Ignore cleanup errors
  }
}

/**
 * Read the FffResult envelope from a raw call. Returns the parsed struct + raw pointer.
 * On error, frees the result and returns a Result error.
 */
function readResultEnvelope(
  funcName: string,
  paramsType: FieldType[],
  paramsValue: unknown[],
): { rawPtr: JsExternal; struct: FffResultRaw } | Result<never> {
  loadLibrary();
  const { rawPtr, struct: structData } = callRaw(funcName, paramsType, paramsValue);

  if (structData.success === 0) {
    const errorStr = readCString(structData.error);
    freeResult(rawPtr);
    return err(errorStr || "Unknown error");
  }

  return { rawPtr, struct: structData };
}

/** Call a function returning FffResult with void payload. */
function callVoidResult(
  funcName: string,
  paramsType: FieldType[],
  paramsValue: unknown[],
): Result<void> {
  const res = readResultEnvelope(funcName, paramsType, paramsValue);
  if ("ok" in res) return res;
  freeResult(res.rawPtr);
  return { ok: true, value: undefined };
}

/** Call a function returning FffResult with int_value payload. */
function callIntResult(
  funcName: string,
  paramsType: FieldType[],
  paramsValue: unknown[],
): Result<number> {
  const res = readResultEnvelope(funcName, paramsType, paramsValue);
  if ("ok" in res) return res;
  const value = Number(res.struct.int_value);
  freeResult(res.rawPtr);
  return { ok: true, value };
}

/** Call a function returning FffResult with bool in int_value. */
function callBoolResult(
  funcName: string,
  paramsType: FieldType[],
  paramsValue: unknown[],
): Result<boolean> {
  const res = readResultEnvelope(funcName, paramsType, paramsValue);
  if ("ok" in res) return res;
  const value = Number(res.struct.int_value) !== 0;
  freeResult(res.rawPtr);
  return { ok: true, value };
}

/** Call a function returning FffResult with a C string in handle. */
function callStringResult(
  funcName: string,
  paramsType: FieldType[],
  paramsValue: unknown[],
): Result<string | null> {
  const res = readResultEnvelope(funcName, paramsType, paramsValue);
  if ("ok" in res) return res;
  const handlePtr = res.struct.handle;
  freeResult(res.rawPtr);
  if (isNullPointer(handlePtr)) return { ok: true, value: null };
  const str = readCString(handlePtr);
  freeString(handlePtr);
  return { ok: true, value: str };
}

/** Call a function returning FffResult with a JSON string in handle. */
function callJsonResult<T>(
  funcName: string,
  paramsType: FieldType[],
  paramsValue: unknown[],
): Result<T> {
  const res = readResultEnvelope(funcName, paramsType, paramsValue);
  if ("ok" in res) return res;
  const handlePtr = res.struct.handle;
  freeResult(res.rawPtr);
  if (isNullPointer(handlePtr)) return { ok: true, value: undefined as T };
  const jsonStr = readCString(handlePtr);
  freeString(handlePtr);
  if (jsonStr === null || jsonStr === "") return { ok: true, value: undefined as T };
  try {
    return { ok: true, value: snakeToCamel(JSON.parse(jsonStr)) as T };
  } catch {
    return { ok: true, value: jsonStr as T };
  }
}

/** Free a C string via fff_free_string. */
function freeString(ptr: JsExternal): void {
  try {
    load({
      library: LIBRARY_KEY,
      funcName: "fff_free_string",
      retType: DataType.Void,
      paramsType: [DataType.External],
      paramsValue: [ptr],
    });
  } catch {
    // Ignore
  }
}

/**
 * Opaque native handle type. Callers must not inspect or modify this value.
 */
export type NativeHandle = JsExternal;

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
  cacheBudgetMaxFiles: number,
  cacheBudgetMaxBytes: number,
  cacheBudgetMaxFileSize: number,
  enableFsRootScanning: boolean,
  enableHomeDirScanning: boolean,
  followSymlinks: boolean,
): Result<NativeHandle> {
  loadLibrary();

  const optsValue = {
    version: FFF_CREATE_OPTIONS_VERSION,
    base_path: basePath,
    frecency_db_path: frecencyDbPath,
    history_db_path: historyDbPath,
    enable_mmap_cache: enableMmapCache ? 1 : 0,
    enable_content_indexing: enableContentIndexing ? 1 : 0,
    watch: watch ? 1 : 0,
    ai_mode: aiMode ? 1 : 0,
    log_file_path: logFilePath,
    log_level: logLevel,
    cache_budget_max_files: cacheBudgetMaxFiles,
    cache_budget_max_bytes: cacheBudgetMaxBytes,
    cache_budget_max_file_size: cacheBudgetMaxFileSize,
    enable_fs_root_scanning: enableFsRootScanning ? 1 : 0,
    enable_home_dir_scanning: enableHomeDirScanning ? 1 : 0,
    follow_symlinks: followSymlinks ? 1 : 0,
  };

  const rawPtr = load({
    library: LIBRARY_KEY,
    funcName: "fff_create_instance_with",
    retType: DataType.External,
    paramsType: [FFF_CREATE_OPTIONS_STRUCT],
    paramsValue: [optsValue],
    freeResultMemory: false,
  }) as JsExternal;

  const [structData] = restorePointer({
    retType: [FFF_RESULT_STRUCT],
    paramsValue: wrapPointer([rawPtr]),
  }) as unknown as [FffResultRaw];

  const success = structData.success !== 0;

  try {
    if (success) {
      const handle = structData.handle;
      if (isNullPointer(handle)) {
        return err("fff_create_instance_with returned null handle");
      }
      return { ok: true, value: handle };
    } else {
      const errorStr = readCString(structData.error);
      return err(errorStr || "Unknown error");
    }
  } finally {
    freeResult(rawPtr);
  }
}

/**
 * Destroy and clean up an instance.
 */
export function ffiDestroy(handle: NativeHandle): void {
  loadLibrary();
  load({
    library: LIBRARY_KEY,
    funcName: "fff_destroy",
    retType: DataType.Void,
    paramsType: [DataType.External],
    paramsValue: [handle],
  });
}

// ---------------------------------------------------------------------------
// Struct type definitions for restorePointer (must match #[repr(C)] layout)
// ---------------------------------------------------------------------------

const FFF_FILE_ITEM_STRUCT = {
  relative_path: DataType.External,
  file_name: DataType.External,
  git_status: DataType.External,
  size: DataType.U64,
  modified: DataType.U64,
  access_frecency_score: DataType.I64,
  modification_frecency_score: DataType.I64,
  total_frecency_score: DataType.I64,
  is_binary: DataType.U8,
};

interface FffFileItemRaw {
  relative_path: JsExternal;
  file_name: JsExternal;
  git_status: JsExternal;
  size: number;
  modified: number;
  access_frecency_score: number;
  modification_frecency_score: number;
  total_frecency_score: number;
  is_binary: number;
}

const FFF_SCORE_STRUCT = {
  total: DataType.I32,
  base_score: DataType.I32,
  filename_bonus: DataType.I32,
  special_filename_bonus: DataType.I32,
  frecency_boost: DataType.I32,
  distance_penalty: DataType.I32,
  current_file_penalty: DataType.I32,
  combo_match_boost: DataType.I32,
  exact_match: DataType.U8,
  match_type: DataType.External,
};

interface FffScoreRaw {
  total: number;
  base_score: number;
  filename_bonus: number;
  special_filename_bonus: number;
  frecency_boost: number;
  distance_penalty: number;
  current_file_penalty: number;
  combo_match_boost: number;
  exact_match: number;
  match_type: JsExternal;
}

const FFF_SEARCH_RESULT_STRUCT = {
  items: DataType.External,
  scores: DataType.External,
  count: DataType.U32,
  total_matched: DataType.U32,
  total_files: DataType.U32,
  // FffLocation inlined (flattened)
  location_tag: DataType.U8,
  location_line: DataType.I32,
  location_col: DataType.I32,
  location_end_line: DataType.I32,
  location_end_col: DataType.I32,
};

interface FffSearchResultRaw {
  items: JsExternal;
  scores: JsExternal;
  count: number;
  total_matched: number;
  total_files: number;
  location_tag: number;
  location_line: number;
  location_col: number;
  location_end_line: number;
  location_end_col: number;
}

// FffDirItem struct (#[repr(C)]): char* (8) + char* (8) + i32 (4) + 4 padding = 24 bytes
const FFF_DIR_ITEM_STRUCT = {
  relative_path: DataType.External,
  dir_name: DataType.External,
  max_access_frecency: DataType.I32,
};

interface FffDirItemRaw {
  relative_path: JsExternal;
  dir_name: JsExternal;
  max_access_frecency: number;
}

const FFF_DIR_SEARCH_RESULT_STRUCT = {
  items: DataType.External,
  scores: DataType.External,
  count: DataType.U32,
  total_matched: DataType.U32,
  total_dirs: DataType.U32,
};

interface FffDirSearchResultRaw {
  items: JsExternal;
  scores: JsExternal;
  count: number;
  total_matched: number;
  total_dirs: number;
}

// FffMixedItem struct (#[repr(C)]): u8 (1) + 7 padding + char* (8) + char* (8) + char* (8)
//   + u64 (8) + u64 (8) + i64 (8) + i64 (8) + i64 (8) + bool (1) + 7 padding = 80 bytes
const FFF_MIXED_ITEM_STRUCT = {
  item_type: DataType.U8,
  relative_path: DataType.External,
  display_name: DataType.External,
  git_status: DataType.External,
  size: DataType.U64,
  modified: DataType.U64,
  access_frecency_score: DataType.I64,
  modification_frecency_score: DataType.I64,
  total_frecency_score: DataType.I64,
  is_binary: DataType.U8,
};

interface FffMixedItemRaw {
  item_type: number;
  relative_path: JsExternal;
  display_name: JsExternal;
  git_status: JsExternal;
  size: number;
  modified: number;
  access_frecency_score: number;
  modification_frecency_score: number;
  total_frecency_score: number;
  is_binary: number;
}

const FFF_MIXED_SEARCH_RESULT_STRUCT = {
  items: DataType.External,
  scores: DataType.External,
  count: DataType.U32,
  total_matched: DataType.U32,
  total_files: DataType.U32,
  total_dirs: DataType.U32,
  // FffLocation inlined (flattened)
  location_tag: DataType.U8,
  location_line: DataType.I32,
  location_col: DataType.I32,
  location_end_line: DataType.I32,
  location_end_col: DataType.I32,
};

interface FffMixedSearchResultRaw {
  items: JsExternal;
  scores: JsExternal;
  count: number;
  total_matched: number;
  total_files: number;
  total_dirs: number;
  location_tag: number;
  location_line: number;
  location_col: number;
  location_end_line: number;
  location_end_col: number;
}

const FFF_GREP_MATCH_STRUCT = {
  relative_path: DataType.External,
  file_name: DataType.External,
  git_status: DataType.External,
  line_content: DataType.External,
  match_ranges: DataType.External,
  context_before: DataType.External,
  context_after: DataType.External,
  size: DataType.U64,
  modified: DataType.U64,
  total_frecency_score: DataType.I64,
  access_frecency_score: DataType.I64,
  modification_frecency_score: DataType.I64,
  line_number: DataType.U64,
  byte_offset: DataType.U64,
  col: DataType.U32,
  match_ranges_count: DataType.U32,
  context_before_count: DataType.U32,
  context_after_count: DataType.U32,
  fuzzy_score: DataType.U32, // actually u16 in C, but ffi-rs doesn't so we read it as u32 with padding
  has_fuzzy_score: DataType.U8,
  is_binary: DataType.U8,
  is_definition: DataType.U8,
};

interface FffGrepMatchRaw {
  relative_path: JsExternal;
  file_name: JsExternal;
  git_status: JsExternal;
  line_content: JsExternal;
  match_ranges: JsExternal;
  context_before: JsExternal;
  context_after: JsExternal;
  size: number;
  modified: number;
  total_frecency_score: number;
  access_frecency_score: number;
  modification_frecency_score: number;
  line_number: number;
  byte_offset: number;
  col: number;
  match_ranges_count: number;
  context_before_count: number;
  context_after_count: number;
  fuzzy_score: number;
  has_fuzzy_score: number;
  is_binary: number;
  is_definition: number;
}

const FFF_GREP_RESULT_STRUCT = {
  items: DataType.External,
  count: DataType.U32,
  total_matched: DataType.U32,
  total_files_searched: DataType.U32,
  total_files: DataType.U32,
  filtered_file_count: DataType.U32,
  next_file_offset: DataType.U32,
  regex_fallback_error: DataType.External,
};

interface FffGrepResultRaw {
  items: JsExternal;
  count: number;
  total_matched: number;
  total_files_searched: number;
  total_files: number;
  filtered_file_count: number;
  next_file_offset: number;
  regex_fallback_error: JsExternal;
}

const FFF_MATCH_RANGE_STRUCT = {
  start: DataType.U32,
  end: DataType.U32,
};

interface FffMatchRangeRaw {
  start: number;
  end: number;
}

// ---------------------------------------------------------------------------
// Struct reading helpers
// ---------------------------------------------------------------------------

function readFileItemFromRaw(raw: FffFileItemRaw): FileItem {
  return {
    relativePath: readCString(raw.relative_path) ?? "",
    fileName: readCString(raw.file_name) ?? "",
    gitStatus: readCString(raw.git_status) ?? "",
    size: Number(raw.size),
    modified: Number(raw.modified),
    accessFrecencyScore: Number(raw.access_frecency_score),
    modificationFrecencyScore: Number(raw.modification_frecency_score),
    totalFrecencyScore: Number(raw.total_frecency_score),
  };
}

function readScoreFromRaw(raw: FffScoreRaw): Score {
  return {
    total: raw.total,
    baseScore: raw.base_score,
    filenameBonus: raw.filename_bonus,
    specialFilenameBonus: raw.special_filename_bonus,
    frecencyBoost: raw.frecency_boost,
    distancePenalty: raw.distance_penalty,
    currentFilePenalty: raw.current_file_penalty,
    comboMatchBoost: raw.combo_match_boost,
    exactMatch: raw.exact_match !== 0,
    matchType: readCString(raw.match_type) ?? "",
  };
}

function readDirItemFromRaw(raw: FffDirItemRaw): DirItem {
  return {
    relativePath: readCString(raw.relative_path) ?? "",
    dirName: readCString(raw.dir_name) ?? "",
    maxAccessFrecency: raw.max_access_frecency,
  };
}

function readMixedItemFromRaw(raw: FffMixedItemRaw): MixedItem {
  if (raw.item_type === 1) {
    // Directory
    return {
      type: "directory",
      item: {
        relativePath: readCString(raw.relative_path) ?? "",
        dirName: readCString(raw.display_name) ?? "",
        maxAccessFrecency: Number(raw.access_frecency_score),
      },
    };
  }
  // File (item_type === 0)
  return {
    type: "file",
    item: {
      relativePath: readCString(raw.relative_path) ?? "",
      fileName: readCString(raw.display_name) ?? "",
      gitStatus: readCString(raw.git_status) ?? "",
      size: Number(raw.size),
      modified: Number(raw.modified),
      accessFrecencyScore: Number(raw.access_frecency_score),
      modificationFrecencyScore: Number(raw.modification_frecency_score),
      totalFrecencyScore: Number(raw.total_frecency_score),
    },
  };
}

/**
 * Call an accessor function that returns a pointer to a struct element,
 * then read the struct from that pointer.
 */
function callAccessor<T>(
  funcName: string,
  resultPtr: JsExternal,
  index: number,
  structDef: Record<string, DataType>,
): T {
  loadLibrary();
  const elemPtr = load({
    library: LIBRARY_KEY,
    funcName,
    retType: DataType.External,
    paramsType: [DataType.External, DataType.U32],
    paramsValue: [resultPtr, index],
  }) as JsExternal;

  const [raw] = restorePointer({
    retType: [structDef],
    paramsValue: wrapPointer([elemPtr]),
  }) as unknown as [T];

  return raw;
}

/**
 * Offset a pointer by `bytes` using the C API helper.
 */
function ptrOffset(base: JsExternal, bytes: number): JsExternal {
  return load({
    library: LIBRARY_KEY,
    funcName: "fff_ptr_offset",
    retType: DataType.External,
    paramsType: [DataType.External, DataType.U64],
    paramsValue: [base, bytes],
  }) as JsExternal;
}

/**
 * Read a C string array (char**) of `count` elements.
 */
function readCStringArray(ptrArray: JsExternal, count: number): string[] {
  if (count === 0 || isNullPointer(ptrArray)) return [];
  const result: string[] = [];
  for (let i = 0; i < count; i++) {
    const elemPtr = ptrOffset(ptrArray, i * 8);
    const [charPtr] = restorePointer({
      retType: [DataType.External],
      paramsValue: [elemPtr],
    }) as unknown as [JsExternal];
    result.push(readCString(charPtr) ?? "");
  }
  return result;
}

function readGrepMatchFromRaw(raw: FffGrepMatchRaw): GrepMatch {
  // Read match_ranges array via pointer offsets
  const matchRanges: [number, number][] = [];
  for (let i = 0; i < raw.match_ranges_count; i++) {
    const rangePtr = ptrOffset(raw.match_ranges, i * 8); // FffMatchRange is 8 bytes
    const [rangeRaw] = restorePointer({
      retType: [FFF_MATCH_RANGE_STRUCT],
      paramsValue: wrapPointer([rangePtr]),
    }) as unknown as [FffMatchRangeRaw];
    matchRanges.push([rangeRaw.start, rangeRaw.end]);
  }

  const match: GrepMatch = {
    relativePath: readCString(raw.relative_path) ?? "",
    fileName: readCString(raw.file_name) ?? "",
    gitStatus: readCString(raw.git_status) ?? "",
    lineContent: readCString(raw.line_content) ?? "",
    size: Number(raw.size),
    modified: Number(raw.modified),
    totalFrecencyScore: Number(raw.total_frecency_score),
    accessFrecencyScore: Number(raw.access_frecency_score),
    modificationFrecencyScore: Number(raw.modification_frecency_score),
    isBinary: raw.is_binary !== 0,
    lineNumber: Number(raw.line_number),
    col: raw.col,
    byteOffset: Number(raw.byte_offset),
    matchRanges,
  };

  if (raw.has_fuzzy_score !== 0) {
    match.fuzzyScore = raw.fuzzy_score;
  }
  if (raw.context_before_count > 0) {
    match.contextBefore = readCStringArray(raw.context_before, raw.context_before_count);
  }
  if (raw.context_after_count > 0) {
    match.contextAfter = readCStringArray(raw.context_after, raw.context_after_count);
  }
  if (raw.is_definition !== 0) {
    match.isDefinition = true;
  }

  return match;
}

/**
 * Parse an FffGrepResult from `FffResult.handle`, then free native memory.
 */
function parseGrepResult(rawPtr: JsExternal): Result<GrepResult> {
  loadLibrary();

  const [envelope] = restorePointer({
    retType: [FFF_RESULT_STRUCT],
    paramsValue: wrapPointer([rawPtr]),
  }) as unknown as [FffResultRaw];

  const success = envelope.success !== 0;

  if (!success) {
    const errorMsg = readCString(envelope.error) || "Unknown error";
    freeResult(rawPtr);
    return err(errorMsg);
  }

  const handlePtr = envelope.handle;
  freeResult(rawPtr);

  if (isNullPointer(handlePtr)) {
    return err("grep returned null result");
  }

  const [gr] = restorePointer({
    retType: [FFF_GREP_RESULT_STRUCT],
    paramsValue: wrapPointer([handlePtr]),
  }) as unknown as [FffGrepResultRaw];

  const count = gr.count;
  const regexFallbackError = readCString(gr.regex_fallback_error) ?? undefined;

  const items: GrepMatch[] = [];
  for (let i = 0; i < count; i++) {
    const rawMatch = callAccessor<FffGrepMatchRaw>(
      "fff_grep_result_get_match",
      handlePtr,
      i,
      FFF_GREP_MATCH_STRUCT,
    );
    items.push(readGrepMatchFromRaw(rawMatch));
  }

  // Free native grep result
  load({
    library: LIBRARY_KEY,
    funcName: "fff_free_grep_result",
    retType: DataType.Void,
    paramsType: [DataType.External],
    paramsValue: [handlePtr],
  });

  const grepResult: GrepResult = {
    items,
    totalMatched: gr.total_matched,
    totalFilesSearched: gr.total_files_searched,
    totalFiles: gr.total_files,
    filteredFileCount: gr.filtered_file_count,
    nextCursor: gr.next_file_offset > 0 ? createGrepCursor(gr.next_file_offset) : null,
  };
  if (regexFallbackError) {
    grepResult.regexFallbackError = regexFallbackError;
  }
  return { ok: true, value: grepResult };
}

/**
 * Parse an FffSearchResult from `FffResult.handle`, then free native memory.
 */
function parseSearchResult(rawPtr: JsExternal): Result<SearchResult> {
  loadLibrary();

  // Read FffResult envelope
  const [envelope] = restorePointer({
    retType: [FFF_RESULT_STRUCT],
    paramsValue: wrapPointer([rawPtr]),
  }) as unknown as [FffResultRaw];

  const success = envelope.success !== 0;

  if (!success) {
    const errorMsg = readCString(envelope.error) || "Unknown error";
    freeResult(rawPtr);
    return err(errorMsg);
  }

  const handlePtr = envelope.handle;
  // Free the FffResult envelope (does NOT free handle)
  freeResult(rawPtr);

  if (isNullPointer(handlePtr)) {
    return err("fff_search returned null search result");
  }

  // Read FffSearchResult struct
  const [sr] = restorePointer({
    retType: [FFF_SEARCH_RESULT_STRUCT],
    paramsValue: wrapPointer([handlePtr]),
  }) as unknown as [FffSearchResultRaw];

  const count = sr.count;

  // Read location
  let location: Location | undefined;
  if (sr.location_tag === 1) {
    location = { type: "line", line: sr.location_line };
  } else if (sr.location_tag === 2) {
    location = {
      type: "position",
      line: sr.location_line,
      col: sr.location_col,
    };
  } else if (sr.location_tag === 3) {
    location = {
      type: "range",
      start: { line: sr.location_line, col: sr.location_col },
      end: { line: sr.location_end_line, col: sr.location_end_col },
    };
  }

  // Read items and scores via accessor functions
  const items: FileItem[] = [];
  const scores: Score[] = [];

  for (let i = 0; i < count; i++) {
    const rawItem = callAccessor<FffFileItemRaw>(
      "fff_search_result_get_item",
      handlePtr,
      i,
      FFF_FILE_ITEM_STRUCT,
    );
    items.push(readFileItemFromRaw(rawItem));

    const rawScore = callAccessor<FffScoreRaw>(
      "fff_search_result_get_score",
      handlePtr,
      i,
      FFF_SCORE_STRUCT,
    );
    scores.push(readScoreFromRaw(rawScore));
  }

  // Free native search result
  load({
    library: LIBRARY_KEY,
    funcName: "fff_free_search_result",
    retType: DataType.Void,
    paramsType: [DataType.External],
    paramsValue: [handlePtr],
  });

  const result: SearchResult = {
    items,
    scores,
    totalMatched: sr.total_matched,
    totalFiles: sr.total_files,
  };
  if (location) {
    result.location = location;
  }
  return { ok: true, value: result };
}

/**
 * Parse an FffDirSearchResult from `FffResult.handle`, then free native memory.
 */
function parseDirSearchResult(rawPtr: JsExternal): Result<DirSearchResult> {
  loadLibrary();

  // Read FffResult envelope
  const [envelope] = restorePointer({
    retType: [FFF_RESULT_STRUCT],
    paramsValue: wrapPointer([rawPtr]),
  }) as unknown as [FffResultRaw];

  const success = envelope.success !== 0;

  if (!success) {
    const errorMsg = readCString(envelope.error) || "Unknown error";
    freeResult(rawPtr);
    return err(errorMsg);
  }

  const handlePtr = envelope.handle;
  // Free the FffResult envelope (does NOT free handle)
  freeResult(rawPtr);

  if (isNullPointer(handlePtr)) {
    return err("fff_search_directories returned null search result");
  }

  // Read FffDirSearchResult struct
  const [sr] = restorePointer({
    retType: [FFF_DIR_SEARCH_RESULT_STRUCT],
    paramsValue: wrapPointer([handlePtr]),
  }) as unknown as [FffDirSearchResultRaw];

  const count = sr.count;

  // Read items and scores via accessor functions
  const items: DirItem[] = [];
  const scores: Score[] = [];

  for (let i = 0; i < count; i++) {
    const rawItem = callAccessor<FffDirItemRaw>(
      "fff_dir_search_result_get_item",
      handlePtr,
      i,
      FFF_DIR_ITEM_STRUCT,
    );
    items.push(readDirItemFromRaw(rawItem));

    const rawScore = callAccessor<FffScoreRaw>(
      "fff_dir_search_result_get_score",
      handlePtr,
      i,
      FFF_SCORE_STRUCT,
    );
    scores.push(readScoreFromRaw(rawScore));
  }

  // Free native dir search result
  load({
    library: LIBRARY_KEY,
    funcName: "fff_free_dir_search_result",
    retType: DataType.Void,
    paramsType: [DataType.External],
    paramsValue: [handlePtr],
  });

  return {
    ok: true,
    value: {
      items,
      scores,
      totalMatched: sr.total_matched,
      totalDirs: sr.total_dirs,
    },
  };
}

/**
 * Parse an FffMixedSearchResult from `FffResult.handle`, then free native memory.
 */
function parseMixedSearchResult(rawPtr: JsExternal): Result<MixedSearchResult> {
  loadLibrary();

  // Read FffResult envelope
  const [envelope] = restorePointer({
    retType: [FFF_RESULT_STRUCT],
    paramsValue: wrapPointer([rawPtr]),
  }) as unknown as [FffResultRaw];

  const success = envelope.success !== 0;

  if (!success) {
    const errorMsg = readCString(envelope.error) || "Unknown error";
    freeResult(rawPtr);
    return err(errorMsg);
  }

  const handlePtr = envelope.handle;
  // Free the FffResult envelope (does NOT free handle)
  freeResult(rawPtr);

  if (isNullPointer(handlePtr)) {
    return err("fff_search_mixed returned null search result");
  }

  // Read FffMixedSearchResult struct
  const [sr] = restorePointer({
    retType: [FFF_MIXED_SEARCH_RESULT_STRUCT],
    paramsValue: wrapPointer([handlePtr]),
  }) as unknown as [FffMixedSearchResultRaw];

  const count = sr.count;

  // Read location
  let location: Location | undefined;
  if (sr.location_tag === 1) {
    location = { type: "line", line: sr.location_line };
  } else if (sr.location_tag === 2) {
    location = {
      type: "position",
      line: sr.location_line,
      col: sr.location_col,
    };
  } else if (sr.location_tag === 3) {
    location = {
      type: "range",
      start: { line: sr.location_line, col: sr.location_col },
      end: { line: sr.location_end_line, col: sr.location_end_col },
    };
  }

  // Read items and scores via accessor functions
  const items: MixedItem[] = [];
  const scores: Score[] = [];

  for (let i = 0; i < count; i++) {
    const rawItem = callAccessor<FffMixedItemRaw>(
      "fff_mixed_search_result_get_item",
      handlePtr,
      i,
      FFF_MIXED_ITEM_STRUCT,
    );
    items.push(readMixedItemFromRaw(rawItem));

    const rawScore = callAccessor<FffScoreRaw>(
      "fff_mixed_search_result_get_score",
      handlePtr,
      i,
      FFF_SCORE_STRUCT,
    );
    scores.push(readScoreFromRaw(rawScore));
  }

  // Free native mixed search result
  load({
    library: LIBRARY_KEY,
    funcName: "fff_free_mixed_search_result",
    retType: DataType.Void,
    paramsType: [DataType.External],
    paramsValue: [handlePtr],
  });

  const result: MixedSearchResult = {
    items,
    scores,
    totalMatched: sr.total_matched,
    totalFiles: sr.total_files,
    totalDirs: sr.total_dirs,
  };
  if (location) {
    result.location = location;
  }
  return { ok: true, value: result };
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
  loadLibrary();

  const rawPtr = load({
    library: LIBRARY_KEY,
    funcName: "fff_search",
    retType: DataType.External,
    paramsType: [
      DataType.External, // handle
      DataType.String, // query
      DataType.String, // current_file
      DataType.U32, // max_threads
      DataType.U32, // page_index
      DataType.U32, // page_size
      DataType.I32, // combo_boost_multiplier
      DataType.U32, // min_combo_count
    ],
    paramsValue: [
      handle,
      query,
      currentFile,
      maxThreads,
      pageIndex,
      pageSize,
      comboBoostMultiplier,
      minComboCount,
    ],
    freeResultMemory: false,
  }) as JsExternal;

  return parseSearchResult(rawPtr);
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
  loadLibrary();

  const rawPtr = load({
    library: LIBRARY_KEY,
    funcName: "fff_glob",
    retType: DataType.External,
    paramsType: [
      DataType.External, // handle
      DataType.String, // pattern
      DataType.String, // current_file
      DataType.U32, // max_threads
      DataType.U32, // page_index
      DataType.U32, // page_size
    ],
    paramsValue: [handle, pattern, currentFile, maxThreads, pageIndex, pageSize],
    freeResultMemory: false,
  }) as JsExternal;

  return parseSearchResult(rawPtr);
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
  loadLibrary();

  const rawPtr = load({
    library: LIBRARY_KEY,
    funcName: "fff_search_directories",
    retType: DataType.External,
    paramsType: [
      DataType.External, // handle
      DataType.String, // query
      DataType.String, // current_file
      DataType.U32, // max_threads
      DataType.U32, // page_index
      DataType.U32, // page_size
    ],
    paramsValue: [handle, query, currentFile ?? "", maxThreads, pageIndex, pageSize],
    freeResultMemory: false,
  }) as JsExternal;

  return parseDirSearchResult(rawPtr);
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
  loadLibrary();

  const rawPtr = load({
    library: LIBRARY_KEY,
    funcName: "fff_search_mixed",
    retType: DataType.External,
    paramsType: [
      DataType.External, // handle
      DataType.String, // query
      DataType.String, // current_file
      DataType.U32, // max_threads
      DataType.U32, // page_index
      DataType.U32, // page_size
      DataType.I32, // combo_boost_multiplier
      DataType.U32, // min_combo_count
    ],
    paramsValue: [
      handle,
      query,
      currentFile,
      maxThreads,
      pageIndex,
      pageSize,
      comboBoostMultiplier,
      minComboCount,
    ],
    freeResultMemory: false,
  }) as JsExternal;

  return parseMixedSearchResult(rawPtr);
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
  enforceTimeBudget: boolean,
  beforeContext: number,
  afterContext: number,
  classifyDefinitions: boolean,
): Result<GrepResult> {
  loadLibrary();

  const rawPtr = load({
    library: LIBRARY_KEY,
    funcName: "fff_live_grep_ex",
    retType: DataType.External,
    paramsType: [
      DataType.External, // handle
      DataType.String, // query
      DataType.U8, // mode
      DataType.U64, // max_file_size
      DataType.U32, // max_matches_per_file
      DataType.Boolean, // smart_case
      DataType.U32, // file_offset
      DataType.U32, // page_limit
      DataType.U64, // time_budget_ms
      DataType.Boolean, // enforce_time_budget
      DataType.U32, // before_context
      DataType.U32, // after_context
      DataType.Boolean, // classify_definitions
    ],
    paramsValue: [
      handle,
      query,
      grepModeToU8(mode),
      maxFileSize,
      maxMatchesPerFile,
      smartCase,
      fileOffset,
      pageLimit,
      timeBudgetMs,
      enforceTimeBudget,
      beforeContext,
      afterContext,
      classifyDefinitions,
    ],
    freeResultMemory: false,
  }) as JsExternal;

  return parseGrepResult(rawPtr);
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
  enforceTimeBudget: boolean,
  beforeContext: number,
  afterContext: number,
  classifyDefinitions: boolean,
): Result<GrepResult> {
  loadLibrary();

  const rawPtr = load({
    library: LIBRARY_KEY,
    funcName: "fff_multi_grep_ex",
    retType: DataType.External,
    paramsType: [
      DataType.External, // handle
      DataType.String, // patterns_joined
      DataType.String, // constraints
      DataType.U64, // max_file_size
      DataType.U32, // max_matches_per_file
      DataType.Boolean, // smart_case
      DataType.U32, // file_offset
      DataType.U32, // page_limit
      DataType.U64, // time_budget_ms
      DataType.Boolean, // enforce_time_budget
      DataType.U32, // before_context
      DataType.U32, // after_context
      DataType.Boolean, // classify_definitions
    ],
    paramsValue: [
      handle,
      patternsJoined,
      constraints,
      maxFileSize,
      maxMatchesPerFile,
      smartCase,
      fileOffset,
      pageLimit,
      timeBudgetMs,
      enforceTimeBudget,
      beforeContext,
      afterContext,
      classifyDefinitions,
    ],
    freeResultMemory: false,
  }) as JsExternal;

  return parseGrepResult(rawPtr);
}

/**
 * Trigger file scan.
 */
export function ffiScanFiles(handle: NativeHandle): Result<void> {
  return callVoidResult("fff_scan_files", [DataType.External], [handle]);
}

/**
 * Check if scanning.
 */
export function ffiIsScanning(handle: NativeHandle): boolean {
  loadLibrary();
  return load({
    library: LIBRARY_KEY,
    funcName: "fff_is_scanning",
    retType: DataType.Boolean,
    paramsType: [DataType.External],
    paramsValue: [handle],
  }) as boolean;
}

/**
 * Get the base path of the file picker.
 */
export function ffiGetBasePath(handle: NativeHandle): Result<string | null> {
  return callStringResult("fff_get_base_path", [DataType.External], [handle]);
}

// FffScanProgress struct definition
const FFF_SCAN_PROGRESS_STRUCT = {
  scanned_files_count: DataType.U64,
  is_scanning: DataType.U8,
  is_watcher_ready: DataType.U8,
  is_warmup_complete: DataType.U8,
};

interface FffScanProgressRaw {
  scanned_files_count: number;
  is_scanning: number;
  is_watcher_ready: number;
  is_warmup_complete: number;
}

/**
 * Get scan progress.
 */
export function ffiGetScanProgress(handle: NativeHandle): Result<{
  scannedFilesCount: number;
  isScanning: boolean;
  isWatcherReady: boolean;
  isWarmupComplete: boolean;
}> {
  loadLibrary();
  const res = readResultEnvelope("fff_get_scan_progress", [DataType.External], [handle]);
  if ("ok" in res) return res;

  const handlePtr = res.struct.handle;
  freeResult(res.rawPtr);

  if (isNullPointer(handlePtr)) return err("scan progress returned null");

  const [sp] = restorePointer({
    retType: [FFF_SCAN_PROGRESS_STRUCT],
    paramsValue: wrapPointer([handlePtr]),
  }) as unknown as [FffScanProgressRaw];

  const result = {
    scannedFilesCount: Number(sp.scanned_files_count),
    isScanning: sp.is_scanning !== 0,
    isWatcherReady: sp.is_watcher_ready !== 0,
    isWarmupComplete: sp.is_warmup_complete !== 0,
  };

  // Free native scan progress
  load({
    library: LIBRARY_KEY,
    funcName: "fff_free_scan_progress",
    retType: DataType.Void,
    paramsType: [DataType.External],
    paramsValue: [handlePtr],
  });

  return { ok: true, value: result };
}

/**
 * Wait for a tree scan to complete.
 */
export function ffiWaitForScan(handle: NativeHandle, timeoutMs: number): Result<boolean> {
  return callBoolResult(
    "fff_wait_for_scan",
    [DataType.External, DataType.U64],
    [handle, timeoutMs],
  );
}

/**
 * Restart index in new path.
 */
export function ffiRestartIndex(handle: NativeHandle, newPath: string): Result<void> {
  return callVoidResult(
    "fff_restart_index",
    [DataType.External, DataType.String],
    [handle, newPath],
  );
}

/**
 * Refresh git status.
 */
export function ffiRefreshGitStatus(handle: NativeHandle): Result<number> {
  return callIntResult("fff_refresh_git_status", [DataType.External], [handle]);
}

/**
 * Track query completion.
 */
export function ffiTrackQuery(
  handle: NativeHandle,
  query: string,
  filePath: string,
): Result<boolean> {
  return callBoolResult(
    "fff_track_query",
    [DataType.External, DataType.String, DataType.String],
    [handle, query, filePath],
  );
}

/**
 * Get historical query.
 */
export function ffiGetHistoricalQuery(
  handle: NativeHandle,
  offset: number,
): Result<string | null> {
  return callStringResult(
    "fff_get_historical_query",
    [DataType.External, DataType.U64],
    [handle, offset],
  );
}

// ALWAYS KEEP IN SYNC WITH fff.h
//
// Note: node uses `fff_watch_args` (flattened options) because ffi-rs cannot
// marshal a `*const *const c_char` field inside a struct param — StringArray
// is only supported as a top-level parameter.
//
// Batch contents are read through the C accessors (fff_watch_events_count /
// fff_watch_events_get_path / fff_watch_events_get_kind), so no struct
// layout knowledge lives on this side.

/** Map the C kind byte to the public WatchEventKind. */
function watchKindFromU8(kind: number): WatchEventKind {
  switch (kind) {
    case 0:
      return "created";
    case 1:
      return "modified";
    case 2:
      return "removed";
    default:
      return "rescan";
  }
}

/** Trampoline signature: (watch id, batch address, user_data — unused). */
const WATCH_TRAMPOLINE_TYPE = funcConstructor({
  paramsType: [DataType.U64, DataType.U64, DataType.U64],
  retType: DataType.Void,
});

/** JS handlers keyed by process-unique native watch id. */
const watchHandlers = new Map<number, (events: WatchEvent[]) => void>();
/** Instances (by handle identity) that ever created a watch subscription. */
const watchInstances = new Set<unknown>();

/** Lazily created process-wide trampoline (createPointer result). */
let watchTrampoline: JsExternal[] | null = null;

/** Convert a raw u64 address delivered through the trampoline to a JsExternal. */
function addressToExternal(address: number): JsExternal {
  return load({
    library: LIBRARY_KEY,
    funcName: "fff_ptr_offset",
    retType: DataType.External,
    paramsType: [DataType.U64, DataType.U64],
    paramsValue: [address, 0],
  }) as unknown as JsExternal;
}

/** Parse an FffWatchEventBatch at `address` and free the native memory. */
function consumeWatchBatch(address: number): WatchEvent[] {
  const batchPtr = addressToExternal(address);
  const count = load({
    library: LIBRARY_KEY,
    funcName: "fff_watch_events_count",
    retType: DataType.U32,
    paramsType: [DataType.External],
    paramsValue: [batchPtr],
  }) as unknown as number;

  const events: WatchEvent[] = [];
  for (let i = 0; i < count; i++) {
    const path = load({
      library: LIBRARY_KEY,
      funcName: "fff_watch_events_get_path",
      retType: DataType.External,
      paramsType: [DataType.External, DataType.U32],
      paramsValue: [batchPtr, i],
    }) as unknown as JsExternal;
    const kind = load({
      library: LIBRARY_KEY,
      funcName: "fff_watch_events_get_kind",
      retType: DataType.U8,
      paramsType: [DataType.External, DataType.U32],
      paramsValue: [batchPtr, i],
    }) as unknown as number;
    events.push({
      path: readCString(path) ?? "",
      kind: watchKindFromU8(kind),
    });
  }

  load({
    library: LIBRARY_KEY,
    funcName: "fff_free_watch_events",
    retType: DataType.Void,
    paramsType: [DataType.U64],
    paramsValue: [address],
  });

  return events;
}

/**
 * The single native->JS entry point for all watch subscriptions. Runs on
 * the JS thread (threadsafe_function delivery); the batch is owned by us
 * and freed inside `consumeWatchBatch`. Unknown watch ids (unsubscribe
 * races) are benign: the batch is freed and dropped.
 */
function watchTrampolineImpl(
  watchId: number,
  batchAddress: number,
  _userData: number,
): void {
  const events = consumeWatchBatch(batchAddress);
  const handler = watchHandlers.get(Number(watchId));
  if (handler === undefined || events.length === 0) return;
  try {
    handler(events);
  } catch {
    // User callback errors must not propagate into the FFI layer
  }
}

function ensureWatchTrampoline(): JsExternal {
  if (watchTrampoline === null) {
    watchTrampoline = createPointer({
      paramsType: [WATCH_TRAMPOLINE_TYPE],
      paramsValue: [watchTrampolineImpl],
    });
  }
  return unwrapPointer(watchTrampoline)[0] as JsExternal;
}

// fff watcher uses a single cross-boundary FFI callback to deliver all events which we then manually
// mapping to the user's javascript functions
function ensureWatchCallbackRegistered(handle: NativeHandle): Result<void> {
  if (watchInstances.has(handle as unknown)) return { ok: true, value: undefined };
  const trampoline = ensureWatchTrampoline();
  const registered = callVoidResult(
    "fff_set_watch_callback",
    [DataType.External, DataType.External, DataType.U64],
    [handle, trampoline, 0],
  );
  if (registered.ok) watchInstances.add(handle as unknown);
  return registered;
}

function releaseWatchTrampolineIfIdle(): void {
  if (watchHandlers.size > 0 || watchInstances.size > 0 || watchTrampoline === null)
    return;
  freePointer({
    paramsType: [WATCH_TRAMPOLINE_TYPE],
    paramsValue: watchTrampoline,
    pointerType: PointerType.RsPointer,
  });
  watchTrampoline = null;
}

/**
 * Create a push-mode watch subscription. `callback` receives a normalized
 * batch of up to 128 events, delivered on the JS event loop.
 *
 * Returns the native watch id to pass to `ffiUnwatch`.
 */
export function ffiWatch(
  handle: NativeHandle,
  pattern: string,
  ignore: string[],
  callback: (events: WatchEvent[]) => void,
): Result<number> {
  loadLibrary();

  const registered = ensureWatchCallbackRegistered(handle);
  if (!registered.ok) return registered;

  const created = callIntResult(
    "fff_watch_args",
    [DataType.External, DataType.String, DataType.StringArray, DataType.U32],
    [handle, pattern, ignore, ignore.length],
  );

  if (!created.ok) return created;

  // No startup race: threadsafe delivery lands on the JS event loop, so this
  // synchronous set always precedes the first routing lookup for this id.
  watchHandlers.set(created.value, callback);
  return created;
}

/**
 * Remove a watch subscription. Drops the JS handler synchronously — once
 * this returns the callback can never run again (a late native tail batch
 * misses the map lookup and is dropped).
 */
export function ffiUnwatch(handle: NativeHandle, watchId: number): Result<boolean> {
  const result = callBoolResult(
    "fff_unwatch",
    [DataType.External, DataType.U64],
    [handle, watchId],
  );
  watchHandlers.delete(watchId);
  return result;
}

/**
 * Post-`ffiDestroy` cleanup for an instance's watch state: drops any
 * handlers that were never explicitly unwatched and releases the process
 * trampoline when this was the last watching instance.
 */
export function ffiWatchCleanupAfterDestroy(
  handle: NativeHandle,
  watchIds: Iterable<number>,
): void {
  for (const id of watchIds) {
    watchHandlers.delete(id);
  }
  watchInstances.delete(handle as unknown);
  releaseWatchTrampolineIfIdle();
}

/**
 * Health check.
 *
 * `handle` can be null for a limited check (version + git only).
 * When null, we pass DataType.U64 with value 0 as a null pointer workaround
 * since ffi-rs does not accept `null` for External parameters.
 */
export function ffiHealthCheck(
  handle: NativeHandle | null,
  testPath: string,
): Result<unknown> {
  if (handle === null) {
    // Use U64(0) as a null pointer since ffi-rs rejects null for External params
    return callJsonResult<unknown>(
      "fff_health_check",
      [DataType.U64, DataType.String],
      [0, testPath],
    );
  }

  return callJsonResult<unknown>(
    "fff_health_check",
    [DataType.External, DataType.String],
    [handle, testPath],
  );
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

/**
 * Close the library and release ffi-rs resources.
 * Call this when completely done with the library.
 */
export function closeLibrary(): void {
  if (isLoaded) {
    close(LIBRARY_KEY);
    isLoaded = false;
  }
}
