/**
 * FileFinder - High-level API for the fff file finder
 *
 * This class provides a type-safe, ergonomic API for file finding operations.
 * Each instance owns an independent native file picker that can be created
 * and destroyed independently. Multiple instances can coexist.
 *
 * All methods return Result types for explicit error handling.
 */

import { FFIType, JSCallback, type Pointer } from "bun:ffi";

import {
  ensureLoaded,
  ffiCreate,
  ffiDestroy,
  ffiGetBasePath,
  ffiGetHistoricalQuery,
  ffiGetScanProgress,
  ffiGlob,
  ffiHealthCheck,
  ffiIsScanning,
  ffiLiveGrep,
  ffiMultiGrep,
  ffiRefreshGitStatus,
  ffiRestartIndex,
  ffiScanFiles,
  ffiSearch,
  ffiSearchDirectories,
  ffiSearchMixed,
  ffiSetWatchCallback,
  ffiTrackQuery,
  ffiUnwatch,
  ffiWaitForScan,
  ffiWatch,
  isAvailable,
  type NativeHandle,
  readWatchEventBatch,
} from "./ffi";

import type {
  DirSearchOptions,
  DirSearchResult,
  InitOptions as FFFInitOptions,
  FileFinderApi,
  GlobOptions,
  GrepOptions,
  GrepResult,
  HealthCheck,
  MixedSearchResult,
  MultiGrepOptions,
  Result,
  ScanProgress,
  SearchOptions,
  SearchResult,
  WatchBatchCallback,
  WatchOptions,
  WatchUnsubscribe,
} from "./fff-api";

import { err } from "./fff-api";

/**
 * FileFinder - Fast file finder with fuzzy search
 *
 * Each instance is backed by an independent native file picker. Create as many
 * as you need and destroy them when done.
 *
 * @example
 * ```typescript
 * import { FileFinder } from "fff";
 *
 * // Create an instance
 * const finder = FileFinder.create({ basePath: "/path/to/project" });
 * if (!finder.ok) {
 *   console.error(finder.error);
 *   process.exit(1);
 * }
 *
 * // Wait for initial scan
 * await finder.value.waitForScan(5000);
 *
 * // Search for files
 * const search = finder.value.search("main.ts");
 * if (search.ok) {
 *   for (const item of search.value.items) {
 *     console.log(item.relativePath);
 *   }
 * }
 *
 * // Cleanup
 * finder.value.destroy();
 * ```
 */
export class FileFinder implements FileFinderApi {
  private handle: NativeHandle | null;
  /** Active watch subscriptions: native watch id -> JS batch handler. */
  private watchHandlers = new Map<number, WatchBatchCallback>();
  /**
   * ONE threadsafe JSCallback per instance, registered lazily with
   * `fff_set_watch_callback` on the first subscription and closed in
   * `destroy()` after `fff_destroy` returns (the quiescence barrier).
   */
  private watchJsCallback: JSCallback | null = null;

  private constructor(handle: NativeHandle) {
    this.handle = handle;
  }

  /**
   * Create a new file finder instance.
   *
   * @param options - Initialization options
   * @returns Result containing the new FileFinder instance or an error
   *
   * @example
   * ```typescript
   * // Basic initialization
   * const finder = FileFinder.create({ basePath: "/path/to/project" });
   *
   * // With custom database paths
   * const finder = FileFinder.create({
   *   basePath: "/path/to/project",
   *   frecencyDbPath: "/custom/frecency.mdb",
   *   historyDbPath: "/custom/history.mdb",
   * });
   * ```
   */
  static create(options: FFFInitOptions): Result<FileFinder> {
    const result = ffiCreate(
      options.basePath,
      options.frecencyDbPath ?? "",
      options.historyDbPath ?? "",
      options.useUnsafeNoLock ?? false,
      !(options.disableMmapCache ?? false),
      !(options.disableContentIndexing ?? options.disableMmapCache ?? false),
      !(options.disableWatch ?? false),
      options.aiMode ?? false,
      options.logFilePath ?? "",
      options.logLevel ?? "",
      BigInt(options.cacheBudgetMaxFiles ?? 0),
      BigInt(options.cacheBudgetMaxBytes ?? 0),
      BigInt(options.cacheBudgetMaxFileSize ?? 0),
      options.enableFsRootScanning ?? false,
      options.enableHomeDirScanning ?? false,
    );

    if (!result.ok) {
      return result;
    }

    return { ok: true, value: new FileFinder(result.value) };
  }

  /**
   * Destroy and clean up all resources.
   *
   * Frees the native instance (unsubscribing all watches), then closes the
   * instance watch trampoline. After calling this, the instance must not be
   * used again.
   */
  destroy(): void {
    if (this.handle !== null) {
      this.watchHandlers.clear();
      ffiDestroy(this.handle);
      this.handle = null;
      // Handlers were cleared first, so a delivery racing the destroy is a
      // benign id-map miss before the trampoline is closed.
      this.watchJsCallback?.close();
      this.watchJsCallback = null;
    }
  }

  /**
   * Check if this instance has been destroyed.
   */
  get isDestroyed(): boolean {
    return this.handle === null;
  }

  /**
   * Guard that returns an error if the instance has been destroyed.
   */
  private ensureAlive(): Result<NativeHandle> {
    if (this.handle === null) {
      return err("FileFinder instance has been destroyed.");
    }
    return { ok: true, value: this.handle };
  }

  /**
   * Search for files matching the query.
   *
   * The query supports fuzzy matching and special syntax:
   * - `foo bar` - Match files containing "foo" and "bar"
   * - `src/` - Match files in src directory
   * - `file.ts:42` - Match file.ts with line 42
   * - `file.ts:42:10` - Match file.ts with line 42, column 10
   *
   * @param query - Search query string
   * @param options - Search options
   * @returns Search results with matched files and scores
   *
   * @example
   * ```typescript
   * const result = finder.search("main.ts", { pageSize: 10 });
   * if (result.ok) {
   *   console.log(`Found ${result.value.totalMatched} files`);
   *   for (const item of result.value.items) {
   *     console.log(item.relativePath);
   *   }
   * }
   * ```
   */
  fileSearch(query: string, options?: SearchOptions): Result<SearchResult> {
    const guard = this.ensureAlive();
    if (!guard.ok) return guard;

    return ffiSearch(
      guard.value,
      query,
      options?.currentFile ?? "",
      options?.maxThreads ?? 0,
      options?.pageIndex ?? 0,
      options?.pageSize ?? 0,
      options?.comboBoostMultiplier ?? 0,
      options?.minComboCount ?? 0,
    );
  }

  /**
   * Glob-only search.
   *
   * The pattern is applied as a single pass SIMD optimized prefiltering
   * without any fuzzy matching involved. Faster and 100% compatible to npm `glob`.
   *
   * @param pattern - Glob pattern (required, non-empty)
   * @param options - Glob search options (pagination, max threads, current file)
   * @returns Search results with files matching the glob
   */
  glob(pattern: string, options?: GlobOptions): Result<SearchResult> {
    const guard = this.ensureAlive();
    if (!guard.ok) return guard;

    return ffiGlob(
      guard.value,
      pattern,
      options?.currentFile ?? "",
      options?.maxThreads ?? 0,
      options?.pageIndex ?? 0,
      options?.pageSize ?? 0,
    );
  }

  /**
   * Search for directories matching the query.
   *
   * @param query - Search query string
   * @param options - Directory search options
   * @returns Search results with matched directories and scores
   *
   * @example
   * ```typescript
   * const result = finder.directorySearch("components", { pageSize: 10 });
   * if (result.ok) {
   *   console.log(`Found ${result.value.totalMatched} directories`);
   *   for (const item of result.value.items) {
   *     console.log(item.relativePath);
   *   }
   * }
   * ```
   */
  directorySearch(query: string, options?: DirSearchOptions): Result<DirSearchResult> {
    const guard = this.ensureAlive();
    if (!guard.ok) return guard;

    return ffiSearchDirectories(
      guard.value,
      query,
      options?.currentFile ?? null,
      options?.maxThreads ?? 0,
      options?.pageIndex ?? 0,
      options?.pageSize ?? 0,
    );
  }

  /**
   * Search for files and directories together (mixed search).
   *
   * Results are interleaved by total score in descending order.
   *
   * @param query - Search query string
   * @param options - Search options
   * @returns Mixed search results with files and directories interleaved by score
   *
   * @example
   * ```typescript
   * const result = finder.mixedSearch("main", { pageSize: 20 });
   * if (result.ok) {
   *   for (const entry of result.value.items) {
   *     if (entry.type === "file") {
   *       console.log(`File: ${entry.item.relativePath}`);
   *     } else {
   *       console.log(`Dir: ${entry.item.relativePath}`);
   *     }
   *   }
   * }
   * ```
   */
  mixedSearch(query: string, options?: SearchOptions): Result<MixedSearchResult> {
    const guard = this.ensureAlive();
    if (!guard.ok) return guard;

    return ffiSearchMixed(
      guard.value,
      query,
      options?.currentFile ?? "",
      options?.maxThreads ?? 0,
      options?.pageIndex ?? 0,
      options?.pageSize ?? 0,
      options?.comboBoostMultiplier ?? 0,
      options?.minComboCount ?? 0,
    );
  }

  /**
   * Search file contents (live grep).
   *
   * Searches through the contents of indexed files using the specified mode:
   * - `"plain"` (default): SIMD-accelerated literal text matching
   * - `"regex"`: Regular expression matching
   * - `"fuzzy"`: Smith-Waterman fuzzy matching per line
   *
   * Supports pagination for large result sets. The result includes a `nextCursor`
   * that can be passed back to fetch the next page.
   *
   * The query also supports constraint syntax:
   * - `*.ts pattern` - Only search in TypeScript files
   * - `src/ pattern` - Only search in the src directory
   *
   * @param query - Search query string
   * @param options - Grep options (mode, pagination, limits)
   * @returns Grep results with matched lines and file metadata
   *
   * @example
   * ```typescript
   * // First page
   * const result = finder.grep("TODO", { mode: "plain" });
   * if (result.ok) {
   *   for (const match of result.value.items) {
   *     console.log(`${match.relativePath}:${match.lineNumber}: ${match.lineContent}`);
   *   }
   *   // Fetch next page
   *   if (result.value.nextCursor) {
   *     const page2 = finder.grep("TODO", {
   *       cursor: result.value.nextCursor,
   *     });
   *   }
   * }
   * ```
   */
  grep(query: string, options?: GrepOptions): Result<GrepResult> {
    const guard = this.ensureAlive();
    if (!guard.ok) return guard;

    return ffiLiveGrep(
      guard.value,
      query,
      options?.mode ?? "plain",
      options?.maxFileSize ?? 0,
      options?.maxMatchesPerFile ?? 0,
      options?.smartCase ?? true,
      options?.cursor?._offset ?? 0,
      options?.pageSize ?? 0,
      options?.timeBudgetMs ?? 0,
      options?.enforceTimeBudget ?? false,
      options?.beforeContext ?? 0,
      options?.afterContext ?? 0,
      options?.classifyDefinitions ?? false,
    );
  }

  /**
   * Multi-pattern OR search using Aho-Corasick.
   *
   * Searches for lines matching ANY of the provided patterns using
   * SIMD-accelerated multi-needle matching. Faster than regex alternation
   * for literal text searches.
   *
   * Supports pagination. The result includes a `nextCursor` that can be
   * passed back to fetch the next page.
   *
   * @param options - Multi-grep options including patterns and optional constraints
   * @returns Grep results with matched lines and file metadata
   *
   * @example
   * ```typescript
   * const result = finder.multiGrep({
   *   patterns: ["VideoFrame", "video_frame", "PreloadedImage"],
   * });
   * if (result.ok) {
   *   for (const match of result.value.items) {
   *     console.log(`${match.relativePath}:${match.lineNumber}: ${match.lineContent}`);
   *   }
   * }
   * ```
   */
  multiGrep(options: MultiGrepOptions): Result<GrepResult> {
    const guard = this.ensureAlive();
    if (!guard.ok) return guard;

    if (!options.patterns || options.patterns.length === 0) {
      return err("patterns array must have at least 1 element");
    }

    return ffiMultiGrep(
      guard.value,
      options.patterns.join("\n"),
      options.constraints ?? "",
      options.maxFileSize ?? 0,
      options.maxMatchesPerFile ?? 0,
      options.smartCase ?? true,
      options.cursor?._offset ?? 0,
      options.pageSize ?? 0,
      options.timeBudgetMs ?? 0,
      options.enforceTimeBudget ?? false,
      options.beforeContext ?? 0,
      options.afterContext ?? 0,
      options.classifyDefinitions ?? false,
    );
  }

  /**
   * Trigger a rescan of the indexed directory.
   *
   * This is useful after major file system changes that the
   * background watcher might have missed.
   */
  scanFiles(): Result<void> {
    const guard = this.ensureAlive();
    if (!guard.ok) return guard;
    return ffiScanFiles(guard.value);
  }

  /**
   * Check if a scan is currently in progress.
   */
  isScanning(): boolean {
    if (this.handle === null) return false;
    return ffiIsScanning(this.handle);
  }

  /**
   * Get the base path of the file picker (the root directory being indexed).
   */
  getBasePath(): Result<string | null> {
    const guard = this.ensureAlive();
    if (!guard.ok) return guard;
    return ffiGetBasePath(guard.value);
  }

  /**
   * Get the current scan progress.
   */
  getScanProgress(): Result<ScanProgress> {
    const guard = this.ensureAlive();
    if (!guard.ok) return guard;
    return ffiGetScanProgress(guard.value) as Result<ScanProgress>;
  }

  /**
   * Wait for the initial file scan to complete.
   *
   * Non-blocking: polls `isScanning` and yields to the event loop between
   * checks, so other async work keeps running while waiting.
   *
   * @param timeoutMs - Maximum time to wait in milliseconds (default: 5000)
   * @returns true if scan completed, false if timed out
   *
   * @example
   * ```typescript
   * const finder = FileFinder.create({ basePath: "/path/to/project" });
   * if (finder.ok) {
   *   const completed = await finder.value.waitForScan(10000);
   *   if (!completed.ok || !completed.value) {
   *     console.warn("Scan did not complete in time");
   *   }
   * }
   * ```
   */
  async waitForScan(timeoutMs: number = 5000): Promise<Result<boolean>> {
    const guard = this.ensureAlive();
    if (!guard.ok) return guard;

    const deadline = Date.now() + timeoutMs;
    while (this.isScanning()) {
      if (Date.now() >= deadline) {
        return { ok: true, value: false };
      }
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
    return { ok: true, value: true };
  }

  /**
   * Wait for the initial file scan to complete, blocking the calling thread.
   *
   * Backed by the native `fff_wait_for_scan` call. Prefer {@link waitForScan}
   * unless you specifically need synchronous blocking behaviour.
   *
   * @param timeoutMs - Maximum time to wait in milliseconds (default: 5000)
   * @returns true if scan completed, false if timed out
   */
  waitForScanBlocking(timeoutMs: number = 5000): Result<boolean> {
    const guard = this.ensureAlive();
    if (!guard.ok) return guard;
    return ffiWaitForScan(guard.value, timeoutMs);
  }

  /**
   * Wait until the index is fully ready: the scan has finished and the warmup
   * (content indexing / bigram) phase has completed.
   *
   * Non-blocking — polls `getScanProgress` and yields to the event loop.
   *
   * @param timeoutMs - Maximum time to wait in milliseconds (default: 5000)
   * @returns true if the index became ready, false if timed out
   */
  async waitForIndexReady(timeoutMs: number = 5000): Promise<Result<boolean>> {
    const guard = this.ensureAlive();
    if (!guard.ok) return guard;

    const deadline = Date.now() + timeoutMs;
    while (true) {
      const progress = this.getScanProgress();
      if (!progress.ok) return progress;
      if (!progress.value.isScanning && progress.value.isWarmupComplete) {
        return { ok: true, value: true };
      }
      if (Date.now() >= deadline) {
        return { ok: true, value: false };
      }
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
  }

  /**
   * Change the indexed directory to a new path.
   *
   * This stops the current file watcher and starts indexing the new directory.
   *
   * @param newPath - New directory path to index
   */
  reindex(newPath: string): Result<void> {
    const guard = this.ensureAlive();
    if (!guard.ok) return guard;
    return ffiRestartIndex(guard.value, newPath);
  }

  /**
   * Refresh the git status cache.
   *
   * @returns Number of files with updated git status
   */
  refreshGitStatus(): Result<number> {
    const guard = this.ensureAlive();
    if (!guard.ok) return guard;
    return ffiRefreshGitStatus(guard.value);
  }

  /**
   * Track query completion for smart suggestions.
   *
   * Call this when a user selects a file from search results.
   * This helps improve future search rankings for similar queries.
   *
   * @param query - The search query that was used
   * @param selectedFilePath - The file path that was selected
   */
  trackQuery(query: string, selectedFilePath: string): Result<boolean> {
    const guard = this.ensureAlive();
    if (!guard.ok) return guard;
    return ffiTrackQuery(guard.value, query, selectedFilePath);
  }

  /**
   * Get a historical query by offset.
   *
   * @param offset - Offset from most recent (0 = most recent)
   * @returns The historical query string, or null if not found
   */
  getHistoricalQuery(offset: number): Result<string | null> {
    const guard = this.ensureAlive();
    if (!guard.ok) return guard;
    return ffiGetHistoricalQuery(guard.value, offset);
  }

  /**
   * Lazily create + register the instance-wide watch trampoline. Routes
   * every delivered batch to the handler registered for its watch id;
   * unknown ids (unsubscribe races) are benign — the batch is just freed.
   */
  private ensureWatchTrampoline(handle: NativeHandle): Result<void> {
    if (this.watchJsCallback !== null) return { ok: true, value: undefined };

    // Threadsafe: the native callback thread enqueues the invocation onto the
    // JS event loop; the batch stays valid because JS owns it until it frees
    // it inside readWatchEventBatch.
    const jsCallback = new JSCallback(
      (watchId: bigint | number, batchPtr: Pointer, _userData: Pointer) => {
        const events = readWatchEventBatch(batchPtr);
        const handler = this.watchHandlers.get(Number(watchId));
        if (handler !== undefined && events.length > 0) {
          handler(events);
        }
      },
      {
        // an attempt to fix the bug that is kept unfixed in the zig version of bun :()
        // https://github.com/oven-sh/bun/issues/33840:
        //
        // watch_id is declared `ptr`, not `u64`: ABI-identical (one 64-bit
        // register), but u64 args make bun allocate a JSBigInt on the CALLING
        // (non-JS) thread, corrupting the JS heap
        args: [FFIType.ptr, FFIType.ptr, FFIType.ptr],
        returns: FFIType.void,
        threadsafe: true,
      },
    );

    const registered = ffiSetWatchCallback(handle, jsCallback);
    if (!registered.ok) {
      jsCallback.close();
      return registered;
    }
    this.watchJsCallback = jsCallback;
    return { ok: true, value: undefined };
  }

  /**
   * Subscribe to filesystem changes matching `pattern` (glob, exact file,
   * or directory subtree). Omit the pattern to watch the entire indexed
   * tree. Normalized batches of up to 128 events are delivered on the JS event
   * loop, with each path appearing at most once. See `FileFinderApi.watch`.
   *
   * @example
   * ```typescript
   * const sub = finder.watch("**\/*.ts", (events) => {
   *   for (const e of events) console.log(e.kind, e.path);
   * });
   * if (sub.ok) sub.value(); // unsubscribe
   *
   * // no pattern: everything under the indexed base path
   * const all = finder.watch((events) => console.log(events.length));
   * ```
   */
  watch(callback: WatchBatchCallback, options?: WatchOptions): Result<WatchUnsubscribe>;
  watch(
    pattern: string,
    callback: WatchBatchCallback,
    options?: WatchOptions,
  ): Result<WatchUnsubscribe>;
  watch(
    patternOrCallback: string | WatchBatchCallback,
    callbackOrOptions?: WatchBatchCallback | WatchOptions,
    maybeOptions?: WatchOptions,
  ): Result<WatchUnsubscribe> {
    // Overload shift: watch(cb, opts?) -> empty pattern = whole tree.
    const noPattern = typeof patternOrCallback === "function";
    const pattern = noPattern ? "" : patternOrCallback;
    const callback = noPattern
      ? patternOrCallback
      : (callbackOrOptions as WatchBatchCallback);
    const options = noPattern
      ? (callbackOrOptions as WatchOptions | undefined)
      : maybeOptions;

    if (typeof callback !== "function") {
      return err("watch callback must be a function");
    }

    const guard = this.ensureAlive();
    if (!guard.ok) return guard;

    const trampoline = this.ensureWatchTrampoline(guard.value);
    if (!trampoline.ok) return trampoline;

    const result = ffiWatch(guard.value, pattern, options?.ignore ?? []);
    if (!result.ok) return result;

    // No startup race: the threadsafe trampoline only runs on the JS event
    // loop, so this synchronous set always precedes the first routing lookup.
    const watchId = result.value;
    this.watchHandlers.set(watchId, callback);

    return { ok: true, value: () => this.unwatchById(watchId) };
  }

  /**
   * Remove a subscription from the routing map, then from the native side.
   * Map removal is synchronous on the JS thread, so once this returns the
   * handler can never run again (late native batches miss the lookup).
   * Idempotent.
   */
  private unwatchById(watchId: number): void {
    if (!this.watchHandlers.delete(watchId)) return;
    if (this.handle !== null) {
      ffiUnwatch(this.handle, watchId);
    }
  }

  /**
   * Get health check information.
   *
   * Useful for debugging and verifying the file finder is working correctly.
   *
   * @param testPath - Optional path to test git repository detection
   */
  healthCheck(testPath?: string): Result<HealthCheck> {
    return ffiHealthCheck(this.handle, testPath || "") as Result<HealthCheck>;
  }

  /**
   * Check if the native library is available.
   */
  static isAvailable(): boolean {
    return isAvailable();
  }

  /** Ensure the native library is loaded. */
  static ensureLoaded(): void {
    ensureLoaded();
  }

  /**
   * Get a health check without requiring an instance.
   *
   * Returns limited info (version + git only, no picker/frecency/query data).
   *
   * @param testPath - Optional path to test git repository detection
   */
  static healthCheckStatic(testPath?: string): Result<HealthCheck> {
    return ffiHealthCheck(null, testPath || "") as Result<HealthCheck>;
  }
}
