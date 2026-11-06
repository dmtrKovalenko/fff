import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import { isGitRepo, resolveDbPaths } from "../src/paths";

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

describe("isGitRepo", () => {
  let tmpRoot: string;

  beforeEach(() => {
    tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), "fff-git-repo-"));
  });

  afterEach(() => {
    fs.rmSync(tmpRoot, { recursive: true, force: true });
  });

  test("returns true when the directory or an ancestor has a .git directory", () => {
    const repo = path.join(tmpRoot, "repo");
    const nested = path.join(repo, "packages", "pi-fff", "src");
    fs.mkdirSync(path.join(repo, ".git"), { recursive: true });
    fs.writeFileSync(path.join(repo, ".git", "HEAD"), "ref: refs/heads/main\n");
    fs.mkdirSync(nested, { recursive: true });

    expect(isGitRepo(repo)).toBe(true);
    expect(isGitRepo(nested)).toBe(true);
  });

  test("returns true for git worktrees and submodules where .git is a file", () => {
    const worktree = path.join(tmpRoot, "worktree");
    const nested = path.join(worktree, "src");
    fs.mkdirSync(nested, { recursive: true });
    fs.writeFileSync(
      path.join(worktree, ".git"),
      "gitdir: /tmp/main/.git/worktrees/wt\n",
    );

    expect(isGitRepo(worktree)).toBe(true);
    expect(isGitRepo(nested)).toBe(true);
  });

  test("rejects an empty .git directory or a non-gitdir .git file", () => {
    const emptyGitDir = path.join(tmpRoot, "empty-git-dir");
    fs.mkdirSync(path.join(emptyGitDir, ".git"), { recursive: true });

    const invalidGitFile = path.join(tmpRoot, "invalid-git-file");
    fs.mkdirSync(invalidGitFile, { recursive: true });
    fs.writeFileSync(path.join(invalidGitFile, ".git"), "not a gitfile\n");

    expect(isGitRepo(emptyGitDir)).toBe(false);
    expect(isGitRepo(invalidGitFile)).toBe(false);
  });

  test("returns false when no ancestor up to the root has .git", () => {
    const plain = path.join(tmpRoot, "plain", "nested");
    fs.mkdirSync(plain, { recursive: true });

    expect(isGitRepo(plain)).toBe(false);
  });
});
