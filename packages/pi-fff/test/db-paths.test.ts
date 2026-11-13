import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import { resolveDbPaths } from "../src/paths";

const ENV_KEYS = ["XDG_CACHE_HOME", "XDG_DATA_HOME", "PI_CODING_AGENT_DIR"] as const;

describe("resolveDbPaths", () => {
  let tmpRoot: string;
  let piDir: string;
  let saved: Record<string, string | undefined>;

  beforeEach(() => {
    saved = {};
    for (const key of ENV_KEYS) saved[key] = process.env[key];

    tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), "fff-db-paths-"));
    piDir = path.join(tmpRoot, "pi-agent");

    process.env.XDG_CACHE_HOME = path.join(tmpRoot, "cache");
    process.env.XDG_DATA_HOME = path.join(tmpRoot, "data");
    process.env.PI_CODING_AGENT_DIR = piDir;
  });

  afterEach(() => {
    for (const key of ENV_KEYS) {
      const value = saved[key];
      if (value === undefined) delete process.env[key];
      else process.env[key] = value;
    }
    fs.rmSync(tmpRoot, { recursive: true, force: true });
  });

  test("overrides win over discovery and fallback", () => {
    mkNvimDir("cache", "fff_nvim");
    mkNvimDir("data", "fff_queries");

    const paths = resolveDbPaths({
      frecency: "/explicit/frecency",
      history: "/explicit/history",
    });

    expect(paths.frecency).toBe("/explicit/frecency");
    expect(paths.history).toBe("/explicit/history");
  });

  test("picks existing fff.nvim databases", () => {
    const frecency = mkNvimDir("cache", "fff_nvim");
    const history = mkNvimDir("data", "fff_queries");

    expect(resolveDbPaths({})).toEqual({ frecency, history });
  });

  test("uses the pi data dir when no nvim databases exist", () => {
    expect(resolveDbPaths({})).toEqual({
      frecency: path.join(piDir, "fff", "frecency"),
      history: path.join(piDir, "fff", "history"),
    });
  });

  test("ignores a plain file at the nvim candidate path", () => {
    const cacheDir = path.join(tmpRoot, "cache", "nvim");
    fs.mkdirSync(cacheDir, { recursive: true });
    fs.writeFileSync(path.join(cacheDir, "fff_nvim"), "not a db");

    expect(resolveDbPaths({}).frecency).toBe(path.join(piDir, "fff", "frecency"));
  });

  test("resolves each database independently", () => {
    const history = mkNvimDir("data", "fff_queries");
    const paths = resolveDbPaths({ frecency: "/explicit/frecency" });

    expect(paths.frecency).toBe("/explicit/frecency");
    expect(paths.history).toBe(history);
  });

  function mkNvimDir(kind: "cache" | "data", name: string): string {
    const dir = path.join(tmpRoot, kind, "nvim", name);
    fs.mkdirSync(dir, { recursive: true });
    return dir;
  }
});
