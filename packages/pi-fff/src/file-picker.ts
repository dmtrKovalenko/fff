import path from "node:path";
import type { FileFinderApi, InitOptions, Result } from "@ff-labs/fff-node";
import { type FileFinderStatic, loadSdk, SCAN_TIMEOUT_MS } from "./sdk";

export interface PickerOptions {
  basePath: string;
  enableHomeDirScanning?: boolean;
  enableFsRootScanning?: boolean;
  followSymlinks?: boolean;
}

interface SharedFinder {
  finder: FileFinderApi;
  refs: number;
}

const SHARED_FINDERS = Symbol.for("@ff-labs/pi-fff:shared-finders");

function sharedFinders(): Map<string, SharedFinder> {
  const global = globalThis as typeof globalThis & {
    [SHARED_FINDERS]?: Map<string, SharedFinder>;
  };
  return (global[SHARED_FINDERS] ??= new Map());
}

function finderKey(options: InitOptions): string {
  return JSON.stringify({
    basePath: path.resolve(options.basePath),
    frecencyDbPath:
      options.frecencyDbPath !== undefined
        ? path.resolve(options.frecencyDbPath)
        : undefined,
    historyDbPath:
      options.historyDbPath !== undefined
        ? path.resolve(options.historyDbPath)
        : undefined,
    enableHomeDirScanning: options.enableHomeDirScanning ?? false,
    enableFsRootScanning: options.enableFsRootScanning ?? false,
    aiMode: true,
  });
}

/** Opens every picker in this pi process — the cwd picker and the aux pickers —
 * on the same frecency/history databases. Identical pickers are shared across
 * in-process pi sessions, including retained subagent sessions. */
export class FilePickerFactory {
  private dbDisabled = false;
  private readonly frecencyDbPath: string;
  private readonly historyDbPath: string;
  private readonly onDbFailure?: (error: string) => void;
  private readonly owned = new Map<FileFinderApi, { key: string; refs: number }>();

  constructor(opts: {
    frecencyDbPath: string;
    historyDbPath: string;
    onDbFailure?: (error: string) => void;
  }) {
    this.frecencyDbPath = opts.frecencyDbPath;
    this.historyDbPath = opts.historyDbPath;
    this.onDbFailure = opts.onDbFailure;
  }

  /** True once the databases were given up on, so pickers open without them. */
  get databasesDisabled(): boolean {
    return this.dbDisabled;
  }

  /** Opens a scanned, ready-to-use picker. Throws if it cannot be created. */
  async create(options: PickerOptions): Promise<FileFinderApi> {
    const { FileFinder } = await loadSdk();
    const init: InitOptions = { ...options, aiMode: true };

    if (!this.dbDisabled) {
      const withDbs = {
        ...init,
        frecencyDbPath: this.frecencyDbPath,
        historyDbPath: this.historyDbPath,
      };
      const result = this.acquire(FileFinder, withDbs);
      if (result.ok) return this.waitForScan(result.value);

      const dbLess = this.acquire(FileFinder, init);
      if (!dbLess.ok) {
        throw this.createError(options.basePath, result.error);
      }
      this.dbDisabled = true;
      this.onDbFailure?.(result.error);
      return this.waitForScan(dbLess.value);
    }

    const result = this.acquire(FileFinder, init);
    if (!result.ok) throw this.createError(options.basePath, result.error);
    return this.waitForScan(result.value);
  }

  /** Releases this factory's ownership without disrupting other sessions. */
  release(finder: FileFinderApi): void {
    const ownership = this.owned.get(finder);
    if (!ownership) return;

    if (--ownership.refs === 0) this.owned.delete(finder);

    const shared = sharedFinders().get(ownership.key);
    if (!shared || shared.finder !== finder || --shared.refs > 0) return;
    sharedFinders().delete(ownership.key);
    if (!finder.isDestroyed) finder.destroy();
  }

  private acquire(
    FileFinder: FileFinderStatic,
    options: InitOptions,
  ): Result<FileFinderApi> {
    const key = finderKey(options);
    const existing = sharedFinders().get(key);
    if (existing && !existing.finder.isDestroyed) {
      existing.refs++;
      this.own(existing.finder, key);
      return { ok: true, value: existing.finder };
    }
    if (existing) sharedFinders().delete(key);

    const result = FileFinder.create(options);
    if (!result.ok) return result;

    sharedFinders().set(key, { finder: result.value, refs: 1 });
    this.own(result.value, key);
    return result;
  }

  private own(finder: FileFinderApi, key: string): void {
    const existing = this.owned.get(finder);
    if (existing) existing.refs++;
    else this.owned.set(finder, { key, refs: 1 });
  }

  private async waitForScan(finder: FileFinderApi): Promise<FileFinderApi> {
    try {
      // waitForScan() also resolves on timeout, so this bounds startup rather
      // than guaranteeing a complete index.
      await finder.waitForScan(SCAN_TIMEOUT_MS);
      return finder;
    } catch (error) {
      this.release(finder);
      throw error;
    }
  }

  private createError(basePath: string, error: string): Error {
    return new Error(`Failed to create FFF file picker for ${basePath}: ${error}`);
  }
}
