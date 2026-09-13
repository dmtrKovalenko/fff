import fs from "node:fs";
import os from "node:os";
import path from "node:path";

// Resolved once per process: os.homedir() hits the env/passwd on every call.
export const HOME_DIR = path.resolve(os.homedir());

// fff.nvim db dir names (`frecency.db_path` / `history.db_path` in lua/fff/conf.lua).
const NVIM_FRECENCY_DIR = "fff_nvim";
const NVIM_HISTORY_DIR = "fff_queries";

export interface DbPaths {
  frecency: string;
  history: string;
}

export function isHomeDir(dir: string): boolean {
  return path.resolve(dir) === HOME_DIR;
}

// Mirrors the native `path.parent().is_none()` check: a path that is its own parent.
export function isFsRoot(dir: string): boolean {
  const resolved = path.resolve(dir);
  return path.dirname(resolved) === resolved;
}

// Resolution order: explicit override > existing fff.nvim db > pi-local data dir.
// Reusing the nvim db lets pi rank files by the frecency the user built in their editor.
export function resolveDbPaths(overrides: {
  frecency?: string;
  history?: string;
}): DbPaths {
  return {
    frecency:
      overrides.frecency ??
      existingDir(nvimCacheDir(), NVIM_FRECENCY_DIR) ??
      path.join(piDataDir(), "fff", "frecency"),
    history:
      overrides.history ??
      existingDir(nvimDataDir(), NVIM_HISTORY_DIR) ??
      path.join(piDataDir(), "fff", "history"),
  };
}

function nvimCacheDir(): string {
  const xdg = process.env.XDG_CACHE_HOME;
  if (xdg) return path.join(xdg, "nvim");
  if (process.platform === "win32" && process.env.LOCALAPPDATA)
    return path.join(process.env.LOCALAPPDATA, "nvim-data", "cache");
  return path.join(HOME_DIR, ".cache", "nvim");
}

function nvimDataDir(): string {
  const xdg = process.env.XDG_DATA_HOME;
  if (xdg) return path.join(xdg, "nvim");
  if (process.platform === "win32" && process.env.LOCALAPPDATA)
    return path.join(process.env.LOCALAPPDATA, "nvim-data");
  return path.join(HOME_DIR, ".local", "share", "nvim");
}

export function piDataDir(): string {
  return process.env.PI_CODING_AGENT_DIR ?? path.join(HOME_DIR, ".pi", "agent");
}

// LMDB environments are directories, so a stray file at the same path is not a db.
function existingDir(parent: string, name: string): string | undefined {
  const candidate = path.join(parent, name);
  try {
    return fs.statSync(candidate).isDirectory() ? candidate : undefined;
  } catch {
    return undefined;
  }
}
