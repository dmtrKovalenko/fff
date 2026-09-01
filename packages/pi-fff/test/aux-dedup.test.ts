import { describe, expect, mock, test } from "bun:test";

interface MockFinder {
  isDestroyed: boolean;
  basePath: string;
  waitForScan: (ms: number) => Promise<void>;
  destroy: () => void;
}

const created: MockFinder[] = [];

function createMockFinder(basePath: string): MockFinder {
  const finder: MockFinder = {
    isDestroyed: false,
    basePath,
    // simulate a slow scan so concurrent acquires overlap
    waitForScan: () => new Promise((r) => setTimeout(r, 50)),
    destroy: () => {
      finder.isDestroyed = true;
    },
  };
  created.push(finder);
  return finder;
}

const finderModule = {
  FileFinder: {
    create: (options: Record<string, unknown>) => ({
      ok: true,
      value: createMockFinder(options.basePath as string),
    }),
  },
};

mock.module("@ff-labs/fff-node", () => finderModule);
mock.module("@ff-labs/fff-bun", () => finderModule);

const { AuxFinderPool } = await import("../src/aux-finders");
const { FilePickerFactory } = await import("../src/file-picker");

function makePickers() {
  return new FilePickerFactory({
    frecencyDbPath: "/dbs/frecency",
    historyDbPath: "/dbs/history",
  });
}

describe("process-wide finder sharing", () => {
  test("one factory releases each acquisition independently", async () => {
    created.length = 0;
    const pickers = makePickers();
    const [first, second] = await Promise.all([
      pickers.create({ basePath: "/workspace" }),
      pickers.create({ basePath: "/workspace" }),
    ]);

    expect(created).toHaveLength(1);
    expect(first).toBe(second);

    pickers.release(first);
    expect(created[0].isDestroyed).toBe(false);
    pickers.release(second);
    expect(created[0].isDestroyed).toBe(true);
  });
});

describe("AuxFinderPool concurrent dedup (#746)", () => {
  test("two concurrent acquires for same root share one finder", async () => {
    created.length = 0;
    const pool = new AuxFinderPool({
      enableFsRootScanning: false,
      pickers: makePickers(),
    });
    const [a, b] = await Promise.all([
      pool.acquire("/Users/x"),
      pool.acquire("/Users/x"),
    ]);
    expect(created.length).toBe(1);
    expect(a.finder).toBe(b.finder);
    pool.destroy();
  });

  test("destroying a pool releases a finder whose scan is still starting", async () => {
    created.length = 0;
    const pool = new AuxFinderPool({
      enableFsRootScanning: false,
      pickers: makePickers(),
    });
    const pending = pool.acquire("/Users/pending");

    pool.destroy();
    expect(pending).rejects.toThrow("destroyed during initialization");
    await pending.catch(() => undefined);
    expect(created[0].isDestroyed).toBe(true);
  });

  test("sequential acquire after in-flight one resolves still reuses", async () => {
    created.length = 0;
    const pool = new AuxFinderPool({
      enableFsRootScanning: false,
      pickers: makePickers(),
    });
    const first = pool.acquire("/Users/x");
    const second = pool.acquire("/Users/x");
    await Promise.all([first, second]);
    const third = await pool.acquire("/Users/x");
    expect(created.length).toBe(1);
    expect(third.root).toBe("/Users/x");
    pool.destroy();
  });
});
