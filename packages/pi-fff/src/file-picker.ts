import type { FileFinderApi, InitOptions, Result } from "@ff-labs/fff-node";
import { type FileFinderStatic, loadSdk, SCAN_TIMEOUT_MS } from "./sdk";

export interface PickerOptions {
  basePath: string;
  enableHomeDirScanning?: boolean;
  enableFsRootScanning?: boolean;
  followSymlinks?: boolean;
}

/** Opens every picker in this pi process — the cwd picker and the aux pickers —
 * on the same frecency/history databases. */
export class FilePickerFactory {
  private dbDisabled = false;
  private readonly frecencyDbPath: string;
  private readonly historyDbPath: string;
  private readonly onDbFailure?: (error: string) => void;

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
    const result = this.openWithDbFallback(FileFinder, options);

    if (!result.ok) {
      throw new Error(
        `Failed to create FFF file picker for ${options.basePath}: ${result.error}`,
      );
    }

    // waitForScan() also resolves on timeout, so this bounds startup rather
    // than guaranteeing a complete index.
    await result.value.waitForScan(SCAN_TIMEOUT_MS);
    return result.value;
  }

  private openWithDbFallback(
    FileFinder: FileFinderStatic,
    options: PickerOptions,
  ): Result<FileFinderApi> {
    const init: InitOptions = { ...options, aiMode: true };
    if (this.dbDisabled) return FileFinder.create(init);

    const result = FileFinder.create({
      ...init,
      frecencyDbPath: this.frecencyDbPath,
      historyDbPath: this.historyDbPath,
    });
    if (result.ok) return result;

    // A failure here is usually transient (broken lock, corruption) and self-heals
    // on restart, so drop the databases instead of leaving pi without a picker
    const dbLess = FileFinder.create(init);
    if (!dbLess.ok) return result; // db error is the more useful one to report

    this.dbDisabled = true;
    this.onDbFailure?.(result.error);
    return dbLess;
  }
}

export async function destroyPickers(
  pickers: readonly { destroy(): void; destroyAsync?(): Promise<void> }[],
): Promise<void> {
  const results = await Promise.allSettled(
    pickers.map(async (picker) => {
      if (picker.destroyAsync) await picker.destroyAsync();
      else picker.destroy();
    }),
  );
  const errors = results.flatMap((result) =>
    result.status === "rejected" ? [result.reason] : [],
  );
  if (errors.length) throw new AggregateError(errors, "Failed to destroy FFF pickers");
}
