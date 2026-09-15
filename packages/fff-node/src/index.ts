/**
 * fff - Fast File Finder
 *
 * High-performance fuzzy file finder for Node.js, powered by Rust.
 * Perfect for LLM agent tools that need to search through codebases.
 *
 * Each `FileFinder` instance is backed by an independent native file picker.
 * Create as many as you need and destroy them when done.
 *
 * Uses ffi-rs to load the same native libfff_c binary used by @ff-labs/fff-bun.
 *
 * @example
 * ```typescript
 * import { FileFinder } from "@ff-labs/fff-node";
 *
 * // Create a file finder instance
 * const result = FileFinder.create({ basePath: "/path/to/project" });
 * if (!result.ok) {
 *   console.error(result.error);
 *   process.exit(1);
 * }
 * const finder = result.value;
 *
 * // Wait for initial scan
 * await finder.waitForScan(5000);
 *
 * // Search for files
 * const search = finder.fileSearch("main.ts");
 * if (search.ok) {
 *   for (const item of search.value.items) {
 *     console.log(item.relativePath);
 *   }
 * }
 *
 * // Cleanup when done
 * finder.destroy();
 * ```
 *
 * @packageDocumentation
 */

export { binaryExists, findBinary } from "./binary.js";
export { closeLibrary } from "./ffi.js";
export type {
  DbHealth,
  DirItem,
  DirSearchOptions,
  DirSearchResult,
  FileFinderApi,
  FileItem,
  GrepCursor,
  GrepMatch,
  GrepMode,
  GrepOptions,
  GrepResult,
  HealthCheck,
  InitOptions,
  LayerEntry,
  LayerInfo,
  Location,
  MixedItem,
  MixedSearchResult,
  MultiGrepOptions,
  Result,
  ScanProgress,
  Score,
  SearchOptions,
  SearchResult,
  WatchBatchCallback,
  WatchEvent,
  WatchEventKind,
  WatchOptions,
  WatchUnsubscribe,
} from "./fff-api.js";
// Result helpers
export { err, ok } from "./fff-api.js";
export { FileFinder } from "./finder.js";
export {
  getLibExtension,
  getLibFilename,
  getNpmPackageName,
  getTriple,
} from "./platform.js";
