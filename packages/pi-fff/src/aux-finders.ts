import fs from "node:fs";
import path from "node:path";
import type { FileFinderApi } from "@ff-labs/fff-node";
import type { FilePickerFactory } from "./file-picker";
import { HOME_DIR } from "./paths";

export const MAX_AUX = 3;
export const IDLE_TTL_MS = 5 * 60 * 1000;

interface AuxPicker {
  root: string;
  finder: FileFinderApi;
  lastUsed: number;
}

export interface AuxOpts {
  enableFsRootScanning: boolean;
  enableHomeDirScanning?: boolean;
  followSymlinks?: boolean;
  pickers: FilePickerFactory;
  // Called before a newly spawned aux picker starts a scan that covers $HOME.
  onHomeDirScan?: (root: string) => void;
}

export class AuxFinderPool {
  private entries: AuxPicker[] = [];
  private destroyed = false;
  // In-flight creations keyed by root. Concurrent acquire() calls for the same
  // (or a covering) root share one finder/scan instead of each starting a full
  // duplicate traversal — issue #746. Mirrors the main finder's finderPromise.
  private pending = new Map<string, Promise<AuxPicker>>();
  constructor(private opts: AuxOpts) {}

  destroy(): void {
    this.destroyed = true;
    for (const e of this.entries) {
      this.opts.pickers.release(e.finder);
    }

    this.entries = [];
    this.pending.clear();
  }

  private sweepIdle(now = Date.now()): void {
    const kept: AuxPicker[] = [];
    for (const e of this.entries) {
      if (now - e.lastUsed > IDLE_TTL_MS) {
        this.opts.pickers.release(e.finder);
      } else {
        kept.push(e);
      }
    }
    this.entries = kept;
  }

  async acquire(
    maybeRoot: string,
    opts?: { exact?: boolean },
  ): Promise<{ finder: FileFinderApi; root: string }> {
    if (this.destroyed) throw new Error("FFF auxiliary finder pool is destroyed");
    this.sweepIdle();
    let covering: AuxPicker | null = null;
    for (const e of this.entries) {
      if (e.finder.isDestroyed) continue;
      if (opts?.exact ? e.root !== maybeRoot : !rootCovers(e.root, maybeRoot)) continue;
      if (!covering || e.root.length > covering.root.length) covering = e;
    }

    if (covering) {
      covering.lastUsed = Date.now();
      return { finder: covering.finder, root: covering.root };
    }

    // Coalesce concurrent creations for the same root so we scan once. A slow
    // full-home scan started by one call is awaited by the others instead of
    // each spawning its own traversal (#746).
    const inflight = this.pending.get(maybeRoot);
    if (inflight) {
      const e = await inflight;
      e.lastUsed = Date.now();
      return { finder: e.finder, root: e.root };
    }

    const creation = this.create(maybeRoot).finally(() => {
      this.pending.delete(maybeRoot);
    });
    this.pending.set(maybeRoot, creation);
    const entry = await creation;
    return { finder: entry.finder, root: entry.root };
  }

  private async create(root: string): Promise<AuxPicker> {
    if (this.entries.length >= MAX_AUX) {
      let oldest = this.entries[0];
      for (const e of this.entries) if (e.lastUsed < oldest.lastUsed) oldest = e;
      this.opts.pickers.release(oldest.finder);
      this.entries = this.entries.filter((e) => e !== oldest);
    }

    const enableHomeDirScanning = this.opts.enableHomeDirScanning ?? true;
    // A fresh picker rooted at (or above) $HOME walks the whole home tree, so
    // the user gets told every time the agent spawns one — see issue #743.
    if (enableHomeDirScanning && rootCovers(root, HOME_DIR)) {
      this.opts.onHomeDirScan?.(root);
    }

    const finder = await this.opts.pickers.create({
      basePath: root,
      enableHomeDirScanning,
      enableFsRootScanning: this.opts.enableFsRootScanning,
      followSymlinks: this.opts.followSymlinks,
    });
    if (this.destroyed) {
      this.opts.pickers.release(finder);
      throw new Error("FFF auxiliary finder pool was destroyed during initialization");
    }

    const entry: AuxPicker = { root, finder, lastUsed: Date.now() };
    this.entries.push(entry);
    return entry;
  }

  size(): number {
    this.sweepIdle();
    return this.entries.length;
  }
}

// Split an absolute path into an existing directory (the aux root) and a
// remainder usable as a fuzzy path constraint relative to that root. Glob and
// nonexistent segments both go into the suffix: we walk up to the nearest
// existing ancestor so partially-wrong paths still resolve to a search root.
export function resolveAuxRoot(absPath: string): { root: string; suffix: string } | null {
  const trimmed = path.normalize(absPath.trim()).replace(/\/+$/, "") || "/";
  if (!path.isAbsolute(trimmed)) return null;
  if (trimmed === path.sep) return { root: path.sep, suffix: "" };

  const parts = trimmed.split(path.sep);
  const firstGlob = parts.findIndex((p) => /[*?[{]/.test(p));
  const boundary = firstGlob === -1 ? parts.length : firstGlob;

  // Deepest existing non-glob prefix wins; everything below it is suffix.
  for (let i = boundary; i > 0; i--) {
    const candidate = parts.slice(0, i).join(path.sep) || path.sep;
    let stat: fs.Stats;
    try {
      stat = fs.statSync(candidate);
    } catch {
      continue;
    }
    if (stat.isFile()) {
      return {
        root: parts.slice(0, i - 1).join(path.sep) || path.sep,
        suffix: parts.slice(i - 1).join("/"),
      };
    }
    return { root: candidate, suffix: parts.slice(i).join("/") };
  }
  return null;
}

export function isOutsideWorkspaceRelativePath(relativePath: string): boolean {
  return (
    path.isAbsolute(relativePath) ||
    relativePath === ".." ||
    relativePath.startsWith(`..${path.sep}`)
  );
}

// Decide whether a `path` parameter should route to the workspace finder or
// to an aux finder. Accepts absolute paths, `~`-prefixed paths, and relative
// paths escaping the workspace (`../other-project`); everything is resolved
// against cwd first. Returns null to signal "no rerouting" (workspace finder).
export function routePathConstraint(
  pathConstraint: string | undefined,
  cwd: string,
): { root: string; suffix: string } | null {
  if (!pathConstraint) return null;
  let candidate = pathConstraint.trim();
  if (!candidate) return null;
  if (candidate === "~" || candidate.startsWith("~/"))
    candidate = path.join(HOME_DIR, candidate.slice(1));
  if (!path.isAbsolute(candidate)) {
    // Plain workspace-relative constraints stay on the workspace finder.
    if (candidate !== ".." && !candidate.startsWith("../")) return null;
    candidate = path.resolve(cwd, candidate);
  }
  const rel = path.relative(cwd, candidate);
  if (!isOutsideWorkspaceRelativePath(rel)) return null;
  return resolveAuxRoot(candidate);
}

export function rootCovers(root: string, target: string): boolean {
  if (root === target) return true;
  const prefix = root.endsWith(path.sep) ? root : root + path.sep;
  return target.startsWith(prefix);
}
