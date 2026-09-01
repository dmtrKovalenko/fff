import { afterEach, describe, expect, mock, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { FileFinder } from "../src/index";

// Real-native tests for #700/#760. Lives here, not in pi-fff/test: that suite
// mocks @ff-labs/fff-bun process-globally and bun module mocks can't be undone.
mock.module("@earendil-works/pi-tui", () => ({
  Text: class Text {
    text: string;
    constructor(text: string) {
      this.text = text;
    }
    setText(text: string) {
      this.text = text;
    }
  },
}));

const schema = (type: string) => (options?: unknown) => ({ type, options });
mock.module("@sinclair/typebox", () => ({
  Type: {
    Array: (items: unknown, options?: unknown) => ({ type: "array", items, options }),
    Boolean: schema("boolean"),
    Number: schema("number"),
    Object: (properties: unknown, options?: unknown) => ({
      type: "object",
      properties,
      options,
    }),
    Optional: (value: unknown) => ({ ...(value as object), optional: true }),
    String: schema("string"),
    Union: (items: unknown[], options?: unknown) => ({ type: "union", items, options }),
  },
}));

const { default: fffExtension } = await import("../../pi-fff/src/index");

// Inject this package as the extension's SDK through the cache hook sdk.ts
// already uses for reloads: CI has no node_modules to resolve "@ff-labs/fff-bun"
// from pi-fff, and the finder stays the real native one either way.
(globalThis as Record<string, unknown>).__fffSdkPromiseGlobal = Promise.resolve({
  FileFinder,
});

const cleanups: Array<() => void> = [];

afterEach(() => {
  while (cleanups.length) cleanups.pop()?.();
});

function makeWorkspace(name: string): string {
  const dir = mkdtempSync(join(tmpdir(), `pi-fff-native-${name}-`));
  cleanups.push(() => rmSync(dir, { recursive: true, force: true }));
  writeFileSync(join(dir, "alpha.ts"), "export const alpha = 1;\n");
  writeFileSync(join(dir, "beta.ts"), "export const beta = 2;\n");
  mkdirSync(join(dir, "src"));
  writeFileSync(join(dir, "src", "gamma.ts"), "export const gamma = 3;\n");
  return dir;
}

function makeDbPaths() {
  const root = mkdtempSync(join(tmpdir(), "pi-fff-native-dbs-"));
  cleanups.push(() => rmSync(root, { recursive: true, force: true }));
  return { frecencyDbPath: join(root, "frecency"), historyDbPath: join(root, "history") };
}

function createFinder(options: Parameters<typeof FileFinder.create>[0]) {
  const result = FileFinder.create(options);
  if (result.ok) {
    const finder = result.value;
    cleanups.push(() => {
      if (!finder.isDestroyed) finder.destroy();
    });
  }
  return result;
}

type SearchOk = { ok: true; value: { items: Array<{ fileName: string }> } };
type SearchResult = SearchOk | { ok: false; error: string };

function fileNames(
  finder: { fileSearch: (q: string, o: { pageSize: number }) => SearchResult },
  query: string,
): string[] {
  const search = finder.fileSearch(query, { pageSize: 10 });
  expect(search.ok).toBe(true);
  return search.ok ? search.value.items.map((i) => i.fileName) : [];
}

describe("fff-bun: many finders share one LMDB env per path (#700/#760)", () => {
  test("a second finder on the same db paths works and searches", async () => {
    const dbs = makeDbPaths();
    const main = createFinder({ basePath: makeWorkspace("main"), ...dbs });
    expect(main.ok).toBe(true);
    if (!main.ok) return;
    await main.value.waitForScan(15_000);
    expect(fileNames(main.value, "alpha")).toContain("alpha.ts");

    // The createAgentSession scenario: same process, same db paths.
    const sub = createFinder({ basePath: makeWorkspace("subagent"), ...dbs });
    expect(sub.ok).toBe(true);
    if (!sub.ok) return;
    await sub.value.waitForScan(15_000);
    expect(fileNames(sub.value, "gamma")).toContain("gamma.ts");
  }, 30_000);

  test("destroying one finder keeps the shared env alive for the other", async () => {
    const dbs = makeDbPaths();
    const first = createFinder({ basePath: makeWorkspace("first"), ...dbs });
    const second = createFinder({ basePath: makeWorkspace("second"), ...dbs });
    expect(first.ok).toBe(true);
    expect(second.ok).toBe(true);
    if (!first.ok || !second.ok) return;

    first.value.destroy();
    await second.value.waitForScan(15_000);
    expect(fileNames(second.value, "alpha")).toContain("alpha.ts");

    // And once the survivor is gone too, the paths are reusable.
    second.value.destroy();
    const third = createFinder({ basePath: makeWorkspace("third"), ...dbs });
    expect(third.ok).toBe(true);
  }, 30_000);

  test("a db-less aux finder coexists with the main finder (#700)", async () => {
    const main = createFinder({ basePath: makeWorkspace("main"), ...makeDbPaths() });
    expect(main.ok).toBe(true);

    const aux = createFinder({ basePath: makeWorkspace("aux") });
    expect(aux.ok).toBe(true);
    if (!aux.ok) return;
    await aux.value.waitForScan(15_000);
    expect(fileNames(aux.value, "gamma")).toContain("gamma.ts");
  }, 30_000);
});

type EventHandler = (...args: unknown[]) => unknown;
type RegisteredTool = {
  name: string;
  execute: (
    toolCallId: string,
    params: unknown,
    signal?: AbortSignal,
  ) => Promise<unknown>;
};

function startSession(
  cwd: string,
  dbs: { frecencyDbPath: string; historyDbPath: string },
) {
  const events = new Map<string, EventHandler>();
  const tools = new Map<string, RegisteredTool>();
  const notifications: Array<{ message: string; level?: string }> = [];
  let activeTools: string[] = [];

  const flags: Record<string, unknown> = {
    "fff-frecency-db": dbs.frecencyDbPath,
    "fff-history-db": dbs.historyDbPath,
  };

  const pi = {
    getFlag: (name: string) => flags[name],
    on: (event: string, handler: EventHandler) => events.set(event, handler),
    registerCommand: () => undefined,
    registerFlag: () => undefined,
    registerTool: (tool: RegisteredTool) => tools.set(tool.name, tool),
    getActiveTools: () => activeTools,
    setActiveTools: (names: string[]) => {
      activeTools = names;
    },
    appendEntry: () => undefined,
  };

  const ctx = {
    cwd,
    ui: {
      notify: (message: string, level?: string) => notifications.push({ message, level }),
      setStatus: () => undefined,
    },
  };

  fffExtension(pi as never);
  cleanups.push(() => {
    void events.get("session_shutdown")?.({}, undefined);
  });

  return {
    start: async () => events.get("session_start")?.({ reason: "startup" }, ctx),
    shutdown: async () => events.get("session_shutdown")?.({}, undefined),
    find: async (pattern: string, params?: Record<string, unknown>) =>
      JSON.stringify(
        await tools.get("fffind")?.execute("test-call", { pattern, ...params }),
      ),
    errors: () => notifications.filter((n) => n.level === "error").map((n) => n.message),
  };
}

describe("pi-fff: in-process double activation works (#760)", () => {
  test("same-workspace sessions share a finder across one shutdown", async () => {
    const dbs = makeDbPaths();
    const workspace = makeWorkspace("shared-session");
    const first = startSession(workspace, dbs);
    const second = startSession(workspace, dbs);

    await Promise.all([first.start(), second.start()]);
    expect(first.errors()).toEqual([]);
    expect(second.errors()).toEqual([]);

    await first.shutdown();
    expect(await second.find("gamma")).toContain("gamma.ts");
    await second.shutdown();
  }, 40_000);

  test("two sessions in one process both search against the same dbs", async () => {
    const dbs = makeDbPaths();
    const first = startSession(makeWorkspace("session1"), dbs);
    await first.start();
    expect(first.errors()).toEqual([]);
    expect(await first.find("alpha")).toContain("alpha.ts");

    // What createAgentSession does: activate the extension again in-process.
    const second = startSession(makeWorkspace("session2"), dbs);
    await second.start();
    expect(second.errors()).toEqual([]);
    expect(await second.find("gamma")).toContain("gamma.ts");

    // And the first session keeps working alongside it.
    expect(await first.find("beta")).toContain("beta.ts");

    await second.shutdown();
    await first.shutdown();
  }, 40_000);

  test("aux finder over an external root shares the session dbs (#700)", async () => {
    const dbs = makeDbPaths();
    const session = startSession(makeWorkspace("aux-session"), dbs);
    await session.start();
    expect(session.errors()).toEqual([]);

    // An absolute out-of-workspace path constraint routes to an aux finder,
    // which now opens the same frecency/history LMDB paths as the main finder.
    const external = makeWorkspace("aux-external");
    expect(await session.find("gamma", { path: external })).toContain("gamma.ts");
    expect(session.errors()).toEqual([]);

    await session.shutdown();
  }, 40_000);
});
