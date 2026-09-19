import { afterEach, describe, expect, mock, test } from "bun:test";
import os from "node:os";
import path from "node:path";

interface MockFinder {
  isDestroyed: boolean;
  basePath: string;
  waitForScan: (ms: number) => Promise<void>;
  destroy: () => void;
}

const created: MockFinder[] = [];
const createOptions: Record<string, unknown>[] = [];

function createMockFinder(basePath: string): MockFinder {
  const finder: MockFinder = {
    isDestroyed: false,
    basePath,
    waitForScan: async () => {},
    destroy: () => {
      finder.isDestroyed = true;
    },
  };
  created.push(finder);
  return finder;
}

// Set to make db-backed creates fail, mimicking a corrupt/locked LMDB.
let failDbCreates = false;
// Set to make every create fail, db-backed or not.
let failAllCreates = false;

const finderModule = {
  FileFinder: {
    create: (options: Record<string, unknown>) => {
      createOptions.push(options);
      if (failAllCreates || (failDbCreates && options.frecencyDbPath !== undefined)) {
        return { ok: false as const, error: "db locked" };
      }

      return {
        ok: true,
        value: createMockFinder(options.basePath as string),
      };
    },
  },
};

mock.module("@ff-labs/fff-node", () => finderModule);
mock.module("@ff-labs/fff-bun", () => finderModule);

const { AuxFinderPool } = await import("../src/aux-finders");
const { FilePickerFactory } = await import("../src/file-picker");

const activePools: Array<InstanceType<typeof AuxFinderPool>> = [];

function makePool(opts: Record<string, unknown> = {}) {
  created.length = 0;
  createOptions.length = 0;
  failDbCreates = false;
  failAllCreates = false;
  const pool = new AuxFinderPool({
    enableFsRootScanning: false,
    pickers: makePickers(),
    ...opts,
  });
  activePools.push(pool);
  return pool;
}

function makePickers(onDbFailure?: (error: string) => void) {
  return new FilePickerFactory({
    frecencyDbPath: "/dbs/frecency",
    historyDbPath: "/dbs/history",
    onDbFailure,
  });
}

afterEach(() => {
  for (const pool of activePools.splice(0)) pool.destroy();
});

describe("AuxFinderPool covering reuse", () => {
  test("reuses a picker rooted at an ancestor of the requested path", async () => {
    const pool = makePool();
    const a = await pool.acquire("/a/b/c");
    expect(a.root).toBe("/a/b/c");

    const b = await pool.acquire("/a/b/c/d");
    expect(b.finder).toBe(a.finder);
    expect(b.root).toBe("/a/b/c");
    expect(created.length).toBe(1);
  });

  test("passes followSymlinks through to the aux picker", async () => {
    const pool = makePool({ followSymlinks: true });
    await pool.acquire("/a/b/c");

    expect(createOptions[0]?.followSymlinks).toBe(true);
  });

  test("does not reuse a picker rooted deeper than the requested path", async () => {
    const pool = makePool();
    await pool.acquire("/a/b/c");
    const broad = await pool.acquire("/a/b");
    expect(broad.root).toBe("/a/b");
    expect(created.length).toBe(2);
  });

  test("prefers the deepest covering picker", async () => {
    const pool = makePool();
    const narrow = await pool.acquire("/a/b/c");
    await pool.acquire("/a/b");

    const again = await pool.acquire("/a/b/c/src");
    expect(again.finder).toBe(narrow.finder);
    expect(again.root).toBe("/a/b/c");
  });

  test("exact mode skips ancestor reuse", async () => {
    const pool = makePool();
    await pool.acquire("/a/b");
    const exact = await pool.acquire("/a/b/c", { exact: true });
    expect(exact.root).toBe("/a/b/c");
    expect(created.length).toBe(2);
  });

  test("does not treat sibling prefixes as covering", async () => {
    const pool = makePool();
    await pool.acquire("/a/bc");
    const other = await pool.acquire("/a/b");
    expect(other.root).toBe("/a/b");
    expect(created.length).toBe(2);
  });

  // Regression for #743: the agent spawning an aux picker over $HOME must warn
  // the user every time, not silently walk the home tree.
  test("notifies on every aux picker that covers $HOME", async () => {
    const onHomeDirScan = mock((_root: string) => undefined);
    const pool = makePool({ onHomeDirScan });
    const home = os.homedir();

    await pool.acquire(home);
    await pool.acquire(path.dirname(home));
    expect(onHomeDirScan.mock.calls).toEqual([[home], [path.dirname(home)]]);

    // Project-scoped roots below $HOME do not walk the whole home tree.
    await pool.acquire(path.join(home, "dev", "some-project"));
    expect(onHomeDirScan).toHaveBeenCalledTimes(2);
  });

  test("no aux notification when home scanning is disabled", async () => {
    const onHomeDirScan = mock(() => undefined);
    const pool = makePool({ enableHomeDirScanning: false, onHomeDirScan });

    await pool.acquire(os.homedir());
    expect(onHomeDirScan).not.toHaveBeenCalled();
    expect(createOptions[0].enableHomeDirScanning).toBe(false);
  });

  // #700 is fixed by the process-wide LMDB env pool: same-path opens share one
  // env, so aux finders now reuse the session's frecency/history DBs.
  test("aux finders receive the pool's frecency/history db paths", async () => {
    const pool = makePool();
    await pool.acquire("/a/b/c");
    await pool.acquire("/x/y");
    expect(createOptions.length).toBe(2);
    for (const opts of createOptions) {
      expect(opts.frecencyDbPath).toBe("/dbs/frecency");
      expect(opts.historyDbPath).toBe("/dbs/history");
    }
  });

  test("aux finder falls back to no dbs when opening them fails", async () => {
    const failures: string[] = [];
    const pool = makePool({ pickers: makePickers((e) => failures.push(e)) });
    failDbCreates = true;

    const entry = await pool.acquire("/a/b/c");

    expect(entry.root).toBe("/a/b/c");
    expect(createOptions.length).toBe(2);
    expect(createOptions[0].frecencyDbPath).toBe("/dbs/frecency");
    expect(createOptions[1].frecencyDbPath).toBeUndefined();
    expect(failures).toEqual(["db locked"]);
  });

  test("a db failure on the main finder keeps later aux finders db-less", async () => {
    const failures: string[] = [];
    const pickers = makePickers((e) => failures.push(e));
    const pool = makePool({ pickers });
    failDbCreates = true;

    // Stands in for the main cwd picker hitting the broken db first.
    // The SDK is mocked, so create() hands back a MockFinder, not a real finder.
    const main = (await pickers.create({
      basePath: "/workspace",
    })) as unknown as MockFinder;
    expect(main.basePath).toBe("/workspace");
    expect(pickers.databasesDisabled).toBe(true);

    createOptions.length = 0;
    await pool.acquire("/a/b/c");

    // No retry: the factory already gave up on the dbs, so one db-less create.
    expect(createOptions.length).toBe(1);
    expect(createOptions[0].frecencyDbPath).toBeUndefined();
    expect(createOptions[0].historyDbPath).toBeUndefined();
    expect(failures).toEqual(["db locked"]);
    pickers.release(main as any);
  });

  test("create throws when the picker cannot be opened at all", async () => {
    makePool();
    failAllCreates = true;

    expect(makePickers().create({ basePath: "/nope" })).rejects.toThrow(
      "Failed to create FFF file picker for /nope: db locked",
    );
  });
});
