import type { FileFinderApi, InitOptions, Result } from "@ff-labs/fff-node";

export const SCAN_TIMEOUT_MS = 15_000;

/** pi can be run either under node or sdk, we resolve correct SDK version at runtime */
export type FileFinderStatic = {
  create(options: InitOptions): Result<FileFinderApi>;
};

let sdkPromise: Promise<{ FileFinder: FileFinderStatic }> | null = null;

const SDK_ORDER: Record<"bun" | "node", readonly [string, string]> = {
  // fff-bun is TS-source only and cannot be imported by Bun-compiled hosts
  // (e.g. omp) whose module resolver rejects .ts under node_modules, so the
  // JS-compiled fff-node is kept as a fallback for every runtime.
  bun: ["@ff-labs/fff-bun", "@ff-labs/fff-node"],
  node: ["@ff-labs/fff-node", "@ff-labs/fff-bun"],
};

// Literal dynamic imports so hosts that statically scan extension graphs
// (omp's legacy-pi-compat loader) discover and hook both SDK packages;
// a variable `import(pkg)` would bypass that scan and fail at runtime.
const SDK_IMPORTS = {
  "@ff-labs/fff-bun": () => import("@ff-labs/fff-bun"),
  "@ff-labs/fff-node": () => import("@ff-labs/fff-node"),
} as const;

function detectRuntime(): "bun" | "node" {
  if (typeof (globalThis as { Bun?: unknown }).Bun !== "undefined") return "bun";
  if (
    typeof process !== "undefined" &&
    (process as { versions?: { bun?: string } }).versions?.bun
  )
    return "bun";
  return "node";
}

/** Preferred SDK order for the detected runtime. */
export function sdkCandidates(): readonly [string, string] {
  return SDK_ORDER[detectRuntime()];
}

export async function loadFirst(
  candidates: readonly [string, string],
  loaders: Record<string, () => Promise<unknown>> = SDK_IMPORTS,
): Promise<{ FileFinder: FileFinderStatic }> {
  let lastError: unknown;
  for (const pkg of candidates) {
    try {
      return (await loaders[pkg]()) as { FileFinder: FileFinderStatic };
    } catch (error) {
      lastError = error;
    }
  }
  throw lastError;
}

export function loadSdk(): Promise<{ FileFinder: FileFinderStatic }> {
  if (sdkPromise) return sdkPromise;

  // Pi reloads extension modules with jiti moduleCache:false, so this module
  // is re-executed on every /reload. Re-importing the fff-bun module graph
  // (which top-level awaits a `type: "file"` import of the native .so) hangs
  // forever inside the Bun-compiled pi binary. Cache the first import on
  // globalThis so reloads reuse the resolved module instead of re-importing.
  const g = globalThis as Record<string, unknown>;
  if (g.__fffSdkPromiseGlobal) {
    sdkPromise = g.__fffSdkPromiseGlobal as Promise<{ FileFinder: FileFinderStatic }>;
    return sdkPromise;
  }

  // default to node as it seems like default option
  const p = loadFirst(sdkCandidates());
  sdkPromise = p;
  (globalThis as Record<string, unknown>).__fffSdkPromiseGlobal = p;
  return p;
}
