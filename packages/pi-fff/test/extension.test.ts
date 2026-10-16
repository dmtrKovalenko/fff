import { afterAll, beforeEach, describe, expect, mock, test } from "bun:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

type MockFinder = {
  isDestroyed: boolean;
  waitForScan: ReturnType<typeof mock>;
  mixedSearch: ReturnType<typeof mock>;
  fileSearch: ReturnType<typeof mock>;
  grep: ReturnType<typeof mock>;
  multiGrep: ReturnType<typeof mock>;
  getScanProgress: ReturnType<typeof mock>;
  destroy: ReturnType<typeof mock>;
};

const createCalls: unknown[] = [];
let finders: MockFinder[] = [];
let mixedSearchImpl: ((query: string, options: unknown) => unknown) | undefined;
let fileSearchImpl: ((query: string, options: unknown) => unknown) | undefined;
let grepImpl: ((query: string, options: unknown) => unknown) | undefined;
let multiGrepImpl: ((options: unknown) => unknown) | undefined;
let scanProgressImpl: (() => unknown) | undefined;

function createMockFinder(): MockFinder {
  return {
    isDestroyed: false,
    waitForScan: mock(async () => undefined),
    getScanProgress: mock(() => {
      if (scanProgressImpl) return scanProgressImpl();
      return {
        ok: true,
        value: {
          scannedFilesCount: 0,
          isScanning: false,
          isWatcherReady: true,
          isWarmupComplete: true,
        },
      };
    }),
    mixedSearch: mock((query: string, options: unknown) => {
      if (mixedSearchImpl) return mixedSearchImpl(query, options);
      return {
        ok: true,
        value: {
          items: [],
          scores: [],
          totalMatched: 0,
          totalFiles: 0,
          totalDirs: 0,
        },
      };
    }),
    fileSearch: mock((query: string, options: unknown) => {
      if (fileSearchImpl) return fileSearchImpl(query, options);
      return {
        ok: true,
        value: { items: [], scores: [], totalMatched: 0, totalFiles: 0 },
      };
    }),
    grep: mock((query: string, options: unknown) => {
      if (grepImpl) return grepImpl(query, options);
      return {
        ok: true,
        value: {
          items: [],
          totalMatched: 0,
          totalFiles: 0,
          totalFilesSearched: 0,
          filteredFileCount: 0,
          nextCursor: null,
        },
      };
    }),
    multiGrep: mock((options: unknown) => {
      if (multiGrepImpl) return multiGrepImpl(options);
      return grepPage();
    }),
    destroy: mock(function (this: MockFinder) {
      this.isDestroyed = true;
    }),
  };
}

const NATIVE_ROOT_REFUSAL =
  "Failed to init file picker: Can not run certain FFF features in a file system root or home directories. Consider smaller per-project directories.";

// Mirrors the native guard in crates/fff-core/src/file_picker.rs: a picker rooted
// at $HOME or `/` is refused unless the matching opt-in is set.
function nativeRefusal(options: {
  basePath: string;
  enableHomeDirScanning?: boolean;
  enableFsRootScanning?: boolean;
}): boolean {
  const base = path.resolve(options.basePath);
  if (options.enableHomeDirScanning === false && base === path.resolve(os.homedir()))
    return true;
  return options.enableFsRootScanning === false && path.dirname(base) === base;
}

const finderModule = {
  FileFinder: {
    create: mock((options: any) => {
      createCalls.push(options);
      if (nativeRefusal(options)) return { ok: false, error: NATIVE_ROOT_REFUSAL };
      const finder = createMockFinder();
      finders.push(finder);
      return { ok: true, value: finder };
    }),
  },
};

mock.module("@ff-labs/fff-node", () => finderModule);
mock.module("@ff-labs/fff-bun", () => finderModule);

mock.module("@earendil-works/pi-tui", () => ({
  MouseRegion: class MouseRegion {
    constructor(
      public component: any,
      public onMouse: (event: any) => unknown,
    ) {}
  },
  Text: class Text {
    text: string;
    constructor(text: string) {
      this.text = text;
    }
    setText(text: string) {
      this.text = text;
    }
  },
  sliceByColumn: (text: string, _start: number, end: number) => text.slice(0, end),
  visibleWidth: (text: string) => text.length,
}));

const schema = (type: string) => (options?: unknown) => ({ type, options });

mock.module("@sinclair/typebox", () => ({
  Type: {
    Array: (items: unknown, options?: unknown) => ({ type: "array", items, options }),
    Boolean: schema("boolean"),
    Literal: (value: unknown) => ({ const: value }),
    Number: schema("number"),
    Object: (properties: unknown, options?: unknown) => ({
      type: "object",
      properties,
      options,
    }),
    Optional: (value: Record<string, unknown>) => ({ ...value, optional: true }),
    String: schema("string"),
    Union: (items: unknown[], options?: unknown) => ({ type: "union", items, options }),
  },
}));

const { default: fffExtension } = await import("../src/index");

type EventHandler = (...args: any[]) => unknown;

function createPi(mode?: string, flags: Record<string, unknown> = {}) {
  const events = new Map<string, EventHandler>();
  const commands = new Map<string, any>();
  const registeredFlags = new Set<string>();
  let flagsReady = false;

  const pi = {
    getFlag: mock((name: string) => {
      if (!flagsReady || !registeredFlags.has(name)) return undefined;
      return name === "fff-mode" && mode !== undefined ? mode : flags[name];
    }),
    on: mock((event: string, handler: EventHandler) => {
      events.set(event, (...args) => {
        flagsReady = true;
        return handler(...args);
      });
    }),
    registerCommand: mock((name: string, command: any) => {
      commands.set(name, {
        ...command,
        handler: (...args: any[]) => {
          flagsReady = true;
          return command.handler(...args);
        },
      });
    }),
    registerFlag: mock((name: string) => {
      registeredFlags.add(name);
    }),
    registerTool: mock((_tool: any) => undefined),
    getActiveTools: mock(() => ["read"] as string[]),
    setActiveTools: mock((_names: string[]) => undefined),
    appendEntry: mock((_customType: string, _data: unknown) => undefined),
  };

  return { pi, events, commands };
}

function createContext(cwd = "/tmp/workspace") {
  return {
    cwd,
    sessionManager: {
      getEntries: mock(() => [] as any[]),
    },
    // Signatures mirror the real pi UI surface so mock.calls stays typed.
    ui: {
      addAutocompleteProvider: mock((_factory: (current: any) => any) => undefined),
      notify: mock((_message: string, _level?: string) => undefined),
      setEditorComponent: mock(() => undefined),
      setStatus: mock((_key: string, _text?: string) => undefined),
    },
  };
}

async function start(mode?: string, cwd?: string, flags: Record<string, unknown> = {}) {
  const setup = createPi(mode, flags);
  const ctx = createContext(cwd);
  fffExtension(setup.pi as any);

  const sessionStart = setup.events.get("session_start");
  expect(sessionStart).toBeDefined();
  await sessionStart?.({ reason: "startup" }, ctx);

  return { ...setup, ctx };
}

async function shutdown(setup: { events: Map<string, EventHandler> }) {
  await setup.events.get("session_shutdown")?.({}, undefined);
}

function currentProvider(
  result = { items: [{ value: "base", label: "base" }], prefix: "ba" },
) {
  return {
    getSuggestions: mock(async () => result),
    applyCompletion: mock(() => ({ lines: ["applied"], cursorLine: 0, cursorCol: 7 })),
    shouldTriggerFileCompletion: mock(() => false),
  };
}

function abortOptions() {
  return { signal: new AbortController().signal };
}

const CONFIG_ENV_KEYS = [
  "PI_CODING_AGENT_DIR",
  "PI_FFF_MODE",
  "PI_FFF_MULTIGREP",
  "FFF_FRECENCY_DB",
  "FFF_HISTORY_DB",
  "FFF_ENABLE_ROOT_SCAN",
  "FFF_ENABLE_HOME_SCAN",
  "FFF_WARN_HOME_SCAN",
  "FFF_FOLLOW_SYMLINKS",
] as const;

const savedEnv: Record<string, string | undefined> = {};
for (const key of CONFIG_ENV_KEYS) savedEnv[key] = process.env[key];

const agentDir = fs.mkdtempSync(path.join(os.tmpdir(), "pi-fff-extension-"));
const configPath = path.join(agentDir, "pi-fff.json");

beforeEach(() => {
  createCalls.length = 0;
  finders = [];
  mixedSearchImpl = undefined;
  fileSearchImpl = undefined;
  grepImpl = undefined;
  multiGrepImpl = undefined;
  scanProgressImpl = undefined;

  for (const key of CONFIG_ENV_KEYS) delete process.env[key];
  process.env.PI_CODING_AGENT_DIR = agentDir;
  fs.rmSync(configPath, { force: true });
});

afterAll(() => {
  for (const key of CONFIG_ENV_KEYS) {
    const value = savedEnv[key];
    if (value === undefined) delete process.env[key];
    else process.env[key] = value;
  }
  fs.rmSync(agentDir, { recursive: true, force: true });
});

describe("pi-fff global config", () => {
  test("applies every supported startup option", async () => {
    writeConfig({
      mode: "override",
      frecencyDbPath: "/config/frecency",
      historyDbPath: "/config/history",
      enableFsRootScanning: true,
      enableHomeDirScanning: false,
      followSymlinks: false,
    });

    const setup = await start();
    const toolNames = setup.pi.registerTool.mock.calls.map(([tool]) => tool.name);

    expect(toolNames).toEqual(
      expect.arrayContaining(["ffgrep", "fffind", "grep", "find"]),
    );
    expect(setup.pi.setActiveTools).toHaveBeenLastCalledWith(
      expect.not.arrayContaining(["ffgrep", "fffind"]),
    );
    expect(createCalls[0]).toEqual({
      basePath: "/tmp/workspace",
      frecencyDbPath: "/config/frecency",
      historyDbPath: "/config/history",
      aiMode: true,
      enableHomeDirScanning: false,
      enableFsRootScanning: true,
      followSymlinks: false,
    });
    await shutdown(setup);
  });

  test("keeps flag and environment precedence", async () => {
    writeConfig({
      mode: "tools-only",
      frecencyDbPath: "/config/frecency",
      historyDbPath: "/config/history",
      enableFsRootScanning: true,
      enableHomeDirScanning: false,
    });
    process.env.PI_FFF_MODE = "override";
    process.env.FFF_FRECENCY_DB = "/env/frecency";
    process.env.FFF_HISTORY_DB = "/env/history";
    process.env.FFF_ENABLE_ROOT_SCAN = "1";
    process.env.FFF_ENABLE_HOME_SCAN = "1";
    process.env.FFF_FOLLOW_SYMLINKS = "1";

    const setup = await start("tools-and-ui", undefined, {
      "fff-frecency-db": "/flag/frecency",
      "fff-enable-root-scan": false,
      "fff-follow-symlinks": false,
    });
    const toolNames = setup.pi.registerTool.mock.calls.map(([tool]) => tool.name);

    expect(toolNames).toContain("ffgrep");
    expect(toolNames).toContain("fffind");
    expect(createCalls[0]).toEqual({
      basePath: "/tmp/workspace",
      frecencyDbPath: "/flag/frecency",
      historyDbPath: "/env/history",
      aiMode: true,
      enableHomeDirScanning: true,
      enableFsRootScanning: false,
      followSymlinks: false,
    });
    await shutdown(setup);
  });

  // #627: worktree and stow layouts reach their files through symlinks, so following
  // them is the default and the environment is the way out.
  test("stops following symlinks when the environment disables them", async () => {
    process.env.FFF_FOLLOW_SYMLINKS = "0";

    const setup = await start();

    expect((createCalls[0] as { followSymlinks: boolean }).followSymlinks).toBe(false);
    await shutdown(setup);
  });

  test("falls through invalid flag and environment modes", async () => {
    writeConfig({ mode: "override" });
    process.env.PI_FFF_MODE = "invalid-env-mode";

    const setup = await start("invalid-flag-mode");
    const toolNames = setup.pi.registerTool.mock.calls.map(([tool]) => tool.name);

    expect(toolNames).toEqual(
      expect.arrayContaining(["ffgrep", "fffind", "grep", "find"]),
    );
    expect(setup.pi.setActiveTools).toHaveBeenLastCalledWith(
      expect.not.arrayContaining(["ffgrep", "fffind"]),
    );
    await shutdown(setup);
  });
});

function writeConfig(config: Record<string, unknown>): void {
  fs.writeFileSync(configPath, JSON.stringify(config));
}

describe("pi-fff session mode", () => {
  test("pre-registers FFF renderers before restoring the saved mode", async () => {
    const setup = createPi("tools-and-ui");
    const ctx = createContext();
    ctx.sessionManager.getEntries.mockReturnValue([
      { type: "custom", customType: "fff-mode", data: { mode: "override" } },
    ]);
    fffExtension(setup.pi as any);

    expect(setup.pi.registerTool.mock.calls.map(([tool]) => tool.name)).toEqual([
      "ffgrep",
      "fffind",
    ]);
    expect(setup.pi.setActiveTools).not.toHaveBeenCalled();
    await setup.events.get("session_start")?.({ reason: "startup" }, ctx);

    const tools = setup.pi.registerTool.mock.calls.map(([tool]) => tool);
    const toolNames = tools.map((tool) => tool.name);
    expect(toolNames).toEqual(
      expect.arrayContaining(["ffgrep", "fffind", "grep", "find"]),
    );
    const historicalGrep = tools.find((tool) => tool.name === "ffgrep");
    const activeGrep = tools.find((tool) => tool.name === "grep");
    const theme = {
      bold: (text: string) => text,
      fg: (_color: string, text: string) => text,
    };
    const historicalCall = historicalGrep.renderCall(
      { pattern: "TODO", path: "." },
      theme,
      { state: {}, invalidate: mock(() => undefined), isError: false },
    );
    const activeCall = activeGrep.renderCall({ pattern: "TODO", path: "." }, theme, {
      state: {},
      invalidate: mock(() => undefined),
      isError: false,
    });
    expect(historicalCall.render(80)).toEqual(["ffgrep /TODO/ in ."]);
    expect(activeCall.render(80)).toEqual(["grep /TODO/ in ."]);
    expect(activeGrep.promptGuidelines[0].startsWith("grep:")).toBe(true);
    expect(setup.pi.setActiveTools).toHaveBeenCalledWith(
      expect.arrayContaining(["read", "grep", "find"]),
    );
    expect(setup.pi.setActiveTools).toHaveBeenLastCalledWith(
      expect.not.arrayContaining(["ffgrep", "fffind"]),
    );

    await setup.commands.get("fff-mode").handler("", ctx);
    expect(ctx.ui.notify).toHaveBeenLastCalledWith(
      "Current mode: 'override' (flag: tools-and-ui)",
      "info",
    );
    await shutdown(setup);
  });

  test("prunes auto-activated FFF names when override is the final mode", async () => {
    const setup = createPi("tools-and-ui");
    const ctx = createContext();
    ctx.sessionManager.getEntries.mockReturnValue([
      { type: "custom", customType: "fff-mode", data: { mode: "override" } },
    ]);
    fffExtension(setup.pi as any);

    // Pi core activates every newly registered tool, so the early FFF
    // registrations are active by the time session_start runs.
    setup.pi.getActiveTools.mockReturnValue(["read", "ffgrep", "fffind"]);
    await setup.events.get("session_start")?.({ reason: "startup" }, ctx);

    expect(setup.pi.setActiveTools).toHaveBeenLastCalledWith(
      expect.arrayContaining(["read", "grep", "find"]),
    );
    expect(setup.pi.setActiveTools).toHaveBeenLastCalledWith(
      expect.not.arrayContaining(["ffgrep", "fffind"]),
    );
    await shutdown(setup);
  });

  test("registers tools before an unbound SDK session's first agent turn", async () => {
    const setup = createPi("override");
    const ctx = createContext();
    fffExtension(setup.pi as any);

    expect(setup.pi.registerTool.mock.calls.map(([tool]) => tool.name)).toEqual([
      "ffgrep",
      "fffind",
    ]);
    expect(setup.pi.setActiveTools).not.toHaveBeenCalled();
    await setup.events.get("before_agent_start")?.({}, ctx);

    const toolNames = setup.pi.registerTool.mock.calls.map(([tool]) => tool.name);
    expect(toolNames).toEqual(
      expect.arrayContaining(["ffgrep", "fffind", "grep", "find"]),
    );
    expect(setup.pi.setActiveTools).toHaveBeenLastCalledWith(
      expect.not.arrayContaining(["ffgrep", "fffind"]),
    );
    expect(createCalls).toHaveLength(0);
    await shutdown(setup);
  });

  test("keeps the active mode unchanged until a tool-name switch is reloaded", async () => {
    const setup = await start();

    await setup.commands.get("fff-mode").handler("override", setup.ctx);

    expect(setup.pi.appendEntry).toHaveBeenCalledWith("fff-mode", {
      mode: "override",
    });
    expect(setup.ctx.ui.notify).toHaveBeenLastCalledWith(
      "Mode 'override' saved. Run /reload to apply the tool name change.",
      "info",
    );

    await setup.commands.get("fff-mode").handler("", setup.ctx);
    expect(setup.ctx.ui.notify).toHaveBeenLastCalledWith(
      "Current mode: 'tools-and-ui' (flag: unset)",
      "info",
    );
    await shutdown(setup);
  });
});

// Regression for #855: pi carries the active tool list and the session entries across
// /reload, but drops the extension instance, so a mode switch must not leave the
// previous mode's tool names active.
describe("pi-fff mode switch across /reload", () => {
  // Pi's own default active set: builtin grep/find ship inactive (dist/core/sdk.js).
  const PI_DEFAULT_ACTIVE = ["read", "bash", "edit", "write"];

  function createReloadableSession(
    startupMode?: string,
    active = PI_DEFAULT_ACTIVE,
    cwd?: string,
  ) {
    let activeTools = [...active];
    const entries: unknown[] = [];

    async function load() {
      const setup = createPi(startupMode);
      setup.pi.getActiveTools.mockImplementation(() => [...activeTools]);
      setup.pi.setActiveTools.mockImplementation((names: string[]) => {
        activeTools = [...names];
      });
      setup.pi.appendEntry.mockImplementation((customType: string, data: unknown) => {
        entries.push({ type: "custom", customType, data });
      });

      const ctx = createContext(cwd);
      ctx.sessionManager.getEntries.mockReturnValue(entries as any[]);
      fffExtension(setup.pi as any);
      await setup.events.get("session_start")?.({ reason: "startup" }, ctx);
      return { ...setup, ctx };
    }

    return { load, getActiveTools: () => activeTools };
  }

  test("drops override tool names when switching back to a FFF-named mode", async () => {
    const session = createReloadableSession("override");

    const first = await session.load();
    expect(session.getActiveTools()).toEqual([...PI_DEFAULT_ACTIVE, "grep", "find"]);
    await first.commands.get("fff-mode").handler("tools-and-ui", first.ctx);
    await shutdown(first);

    const second = await session.load();
    expect(session.getActiveTools()).toEqual([...PI_DEFAULT_ACTIVE, "ffgrep", "fffind"]);
    await shutdown(second);
  });

  test("drops FFF tool names when switching to override", async () => {
    const session = createReloadableSession("tools-and-ui");

    const first = await session.load();
    expect(session.getActiveTools()).toEqual([...PI_DEFAULT_ACTIVE, "ffgrep", "fffind"]);
    await first.commands.get("fff-mode").handler("override", first.ctx);
    await shutdown(first);

    const second = await session.load();
    expect(session.getActiveTools()).toEqual([...PI_DEFAULT_ACTIVE, "grep", "find"]);
    await shutdown(second);
  });

  test("keeps user-enabled builtin grep and find when override was never used", async () => {
    const session = createReloadableSession("tools-and-ui", [
      ...PI_DEFAULT_ACTIVE,
      "grep",
      "find",
    ]);

    const setup = await session.load();

    expect(session.getActiveTools()).toEqual([
      ...PI_DEFAULT_ACTIVE,
      "grep",
      "find",
      "ffgrep",
      "fffind",
    ]);
    await shutdown(setup);
  });

  // #857 keeps pi's built-ins when the cwd is opted out of indexing, so the
  // override prune must not deactivate them behind its back.
  test("keeps builtin grep and find when override falls back on an opted-out cwd", async () => {
    process.env.FFF_ENABLE_HOME_SCAN = "0";
    const session = createReloadableSession(
      "override",
      [...PI_DEFAULT_ACTIVE, "grep", "find"],
      os.homedir(),
    );

    const setup = await session.load();

    expect(session.getActiveTools()).toEqual([
      ...PI_DEFAULT_ACTIVE,
      "grep",
      "find",
      "ffgrep",
      "fffind",
    ]);
    await shutdown(setup);
  });
});

// Regression for #743: launching from $HOME must be visible and interruptible.
describe("pi-fff $HOME scan warning", () => {
  test("warns and pins a status when cwd is $HOME", async () => {
    const setup = await start(undefined, os.homedir());

    expect(setup.ctx.ui.notify).toHaveBeenCalledTimes(1);
    const [message, level] = setup.ctx.ui.notify.mock.calls[0];
    expect(message).toContain(os.homedir());
    expect(level).toBe("warning");
    expect(setup.ctx.ui.setStatus).toHaveBeenCalledWith(
      "fff",
      "Agent is indexing $HOME, this can lead to high CPU",
    );
    await shutdown(setup);
  });

  test("stays silent outside $HOME", async () => {
    const { ctx } = await start();

    expect(ctx.ui.notify).not.toHaveBeenCalled();
    expect(ctx.ui.setStatus).not.toHaveBeenCalled();
  });

  test("clears the status once the scan settles", async () => {
    const setup = await start(undefined, os.homedir());

    expect(setup.ctx.ui.setStatus).toHaveBeenLastCalledWith("fff", undefined);
    await shutdown(setup);
  });

  // waitForScan resolves on timeout, so a slow $HOME walk keeps the footer up.
  test("keeps reporting live progress while the scan is still running", async () => {
    scanProgressImpl = () => ({
      ok: true,
      value: {
        scannedFilesCount: 12345,
        isScanning: true,
        isWatcherReady: false,
        isWarmupComplete: false,
      },
    });
    const setup = await start(undefined, os.homedir());

    const lastStatus = setup.ctx.ui.setStatus.mock.calls.at(-1);
    expect(lastStatus?.[0]).toBe("fff");
    expect(lastStatus?.[1]).toContain("12345 files");

    // session_shutdown must stop the poller and clear the footer.
    await shutdown(setup);
    expect(setup.ctx.ui.setStatus).toHaveBeenLastCalledWith("fff", undefined);
  });

  // #857: the opt-out must skip the picker, not surface the native refusal as an
  // init error, and must not shadow pi's built-in search tools in override mode.
  test("skips the picker and stays out of the way when home scanning is off", async () => {
    process.env.FFF_ENABLE_HOME_SCAN = "0";
    const setup = await start("override", os.homedir());

    expect(createCalls).toHaveLength(0);
    expect(setup.ctx.ui.setStatus).not.toHaveBeenCalled();

    const [message, level] = setup.ctx.ui.notify.mock.calls[0];
    expect(setup.ctx.ui.notify).toHaveBeenCalledTimes(1);
    expect(level).toBe("warning");
    expect(message).not.toContain("FFF init failed");
    expect(message).toContain("enableHomeDirScanning");

    const toolNames = setup.pi.registerTool.mock.calls.map(([tool]) => tool.name);
    expect(toolNames).toEqual(["ffgrep", "fffind"]);
    await shutdown(setup);
  });

  test("skips the picker at the filesystem root without root scanning", async () => {
    const setup = await start(undefined, path.parse(process.cwd()).root);

    expect(createCalls).toHaveLength(0);
    const [message, level] = setup.ctx.ui.notify.mock.calls[0];
    expect(level).toBe("warning");
    expect(message).toContain("enableFsRootScanning");
    await shutdown(setup);
  });

  test("still indexes $HOME when the opt-out is not set", async () => {
    const setup = await start(undefined, os.homedir());

    expect(createCalls).toHaveLength(1);
    expect((createCalls[0] as { basePath: string }).basePath).toBe(os.homedir());
    await shutdown(setup);
  });

  // #806: muting the warning must not turn the scan or the footer off.
  test("FFF_WARN_HOME_SCAN=0 mutes the warning but keeps indexing", async () => {
    process.env.FFF_WARN_HOME_SCAN = "0";
    const setup = await start(undefined, os.homedir());

    expect(setup.ctx.ui.notify).not.toHaveBeenCalled();
    expect(setup.ctx.ui.setStatus).toHaveBeenCalledWith(
      "fff",
      "Agent is indexing $HOME, this can lead to high CPU",
    );
    expect(
      (createCalls[0] as { enableHomeDirScanning: boolean }).enableHomeDirScanning,
    ).toBe(true);
    await shutdown(setup);
  });

  test("--fff-warn-home-scan=false mutes the warning", async () => {
    const setup = await start(undefined, os.homedir(), {
      "fff-warn-home-scan": false,
    });

    expect(setup.ctx.ui.notify).not.toHaveBeenCalled();
    await shutdown(setup);
  });

  test("warnOnHomeDirScan in the config file mutes the warning", async () => {
    writeConfig({ warnOnHomeDirScan: false });
    const setup = await start(undefined, os.homedir());

    expect(setup.ctx.ui.notify).not.toHaveBeenCalled();
    await shutdown(setup);
  });
});

describe("pi-fff autocomplete registration", () => {
  test("session_start registers a provider without replacing the editor", async () => {
    const { ctx } = await start();

    expect(ctx.ui.addAutocompleteProvider).toHaveBeenCalledTimes(1);
    expect(ctx.ui.setEditorComponent).not.toHaveBeenCalled();
    expect(createCalls).toEqual([
      {
        basePath: "/tmp/workspace",
        // Resolved defaults are host-dependent; covered by test/db-paths.test.ts.
        frecencyDbPath: expect.any(String),
        historyDbPath: expect.any(String),
        aiMode: true,
        enableHomeDirScanning: true,
        enableFsRootScanning: false,
        followSymlinks: true,
      },
    ]);
  });

  test("FFF_ENABLE_HOME_SCAN=0 disables home dir scanning", async () => {
    process.env.FFF_ENABLE_HOME_SCAN = "0";
    await start();

    const opts = createCalls[0] as { enableHomeDirScanning: boolean };
    expect(opts.enableHomeDirScanning).toBe(false);
  });

  test("session_start survives hosts without addAutocompleteProvider", async () => {
    const setup = createPi();
    const ctx = {
      cwd: "/tmp/workspace",
      ui: {
        notify: mock(() => undefined),
        setEditorComponent: mock(() => undefined),
      },
    };
    fffExtension(setup.pi as any);

    const sessionStart = setup.events.get("session_start");
    await sessionStart?.({ reason: "startup" }, ctx);

    expect(ctx.ui.notify).not.toHaveBeenCalled();
    expect(createCalls).toHaveLength(1);
  });

  test("delegates non-@ completions to the current provider", async () => {
    const { ctx } = await start();
    const factory = ctx.ui.addAutocompleteProvider.mock.calls[0][0];
    const current = currentProvider();
    const provider = factory(current);

    const result = await provider.getSuggestions(["hello"], 0, 5, abortOptions());

    expect(result).toEqual({ items: [{ value: "base", label: "base" }], prefix: "ba" });
    expect(current.getSuggestions).toHaveBeenCalledTimes(1);
    expect(finders[0].mixedSearch).not.toHaveBeenCalled();
  });

  test("returns FFF-backed @ mention suggestions", async () => {
    mixedSearchImpl = (query, options) => {
      expect(query).toBe("src");
      expect(options).toEqual({ pageSize: 20 });
      return {
        ok: true,
        value: {
          items: [
            {
              type: "file",
              item: {
                relativePath: "src/index.ts",
                fileName: "index.ts",
                size: 1,
                modified: 1,
                accessFrecencyScore: 0,
                modificationFrecencyScore: 0,
                totalFrecencyScore: 0,
                gitStatus: "clean",
              },
            },
            {
              type: "directory",
              item: {
                relativePath: "src/components/",
                dirName: "components/",
                maxAccessFrecency: 0,
              },
            },
          ],
          scores: [],
          totalMatched: 2,
          totalFiles: 1,
          totalDirs: 1,
        },
      };
    };

    const { ctx } = await start();
    const factory = ctx.ui.addAutocompleteProvider.mock.calls[0][0];
    const current = currentProvider();
    const provider = factory(current);

    const result = await provider.getSuggestions(["open @src"], 0, 9, abortOptions());

    expect(result).toEqual({
      prefix: "@src",
      items: [
        {
          value: "@src/index.ts",
          label: "index.ts",
          description: "src/index.ts",
        },
        {
          value: "@src/components/",
          label: "components/",
          description: "src/components/",
        },
      ],
    });
    expect(current.getSuggestions).not.toHaveBeenCalled();
  });

  test("delegates when FFF lookup fails", async () => {
    mixedSearchImpl = () => {
      throw new Error("native lookup failed");
    };

    const { ctx } = await start();
    const factory = ctx.ui.addAutocompleteProvider.mock.calls[0][0];
    const current = currentProvider();
    const provider = factory(current);

    const result = await provider.getSuggestions(["@src"], 0, 4, abortOptions());

    expect(result).toEqual({ items: [{ value: "base", label: "base" }], prefix: "ba" });
    expect(current.getSuggestions).toHaveBeenCalledTimes(1);
  });

  test("tools-only mode bypasses FFF mentions and delegates", async () => {
    const { ctx } = await start("tools-only");
    const factory = ctx.ui.addAutocompleteProvider.mock.calls[0][0];
    const current = currentProvider();
    const provider = factory(current);

    const result = await provider.getSuggestions(["@src"], 0, 4, abortOptions());

    expect(result).toEqual({ items: [{ value: "base", label: "base" }], prefix: "ba" });
    expect(current.getSuggestions).toHaveBeenCalledTimes(1);
    expect(finders[0].mixedSearch).not.toHaveBeenCalled();
  });

  test("/fff-mode changes mention behavior without touching the editor", async () => {
    const { commands, ctx, pi } = await start();
    const factory = ctx.ui.addAutocompleteProvider.mock.calls[0][0];
    const current = currentProvider();
    const provider = factory(current);

    await commands.get("fff-mode").handler("tools-only", ctx);
    await provider.getSuggestions(["@src"], 0, 4, abortOptions());

    expect(pi.appendEntry).toHaveBeenCalledWith("fff-mode", { mode: "tools-only" });
    expect(current.getSuggestions).toHaveBeenCalledTimes(1);
    expect(finders[0].mixedSearch).not.toHaveBeenCalled();
    expect(ctx.ui.setEditorComponent).not.toHaveBeenCalled();
  });

  test("completion application and file-completion trigger delegate to current provider", async () => {
    const { ctx } = await start();
    const factory = ctx.ui.addAutocompleteProvider.mock.calls[0][0];
    const current = currentProvider();
    const provider = factory(current);

    const applied = provider.applyCompletion(
      ["@src"],
      0,
      4,
      { value: "@src/index.ts", label: "index.ts" },
      "@src",
    );
    const shouldTrigger = provider.shouldTriggerFileCompletion(["@src"], 0, 4);

    expect(applied).toEqual({ lines: ["applied"], cursorLine: 0, cursorCol: 7 });
    expect(shouldTrigger).toBe(false);
    expect(current.applyCompletion).toHaveBeenCalledTimes(1);
    expect(current.shouldTriggerFileCompletion).toHaveBeenCalledTimes(1);
  });
});

describe("compact tool rendering", () => {
  const theme = {
    bold: (text: string) => text,
    fg: (_color: string, text: string) => text,
  };

  function toolByName(
    setup: { pi: { registerTool: ReturnType<typeof mock> } },
    name: string,
  ) {
    const tool = setup.pi.registerTool.mock.calls
      .map(([tool]) => tool)
      .find((tool) => tool.name === name);
    expect(tool).toBeDefined();
    return tool;
  }

  test("ffgrep renders collapsed by default and full when pi reports expanded", async () => {
    const setup = await start("tools-and-ui");
    const tool = toolByName(setup, "ffgrep");
    const context = { state: {}, invalidate: mock(() => undefined), isError: false };

    const call = tool.renderCall(
      { pattern: "TODO", path: ".", limit: 3, context: 2 },
      theme,
      context,
    );
    expect(call.render(80)).toEqual(["ffgrep /TODO/ in . (limit 3, context 2)"]);

    const defaultCall = tool.renderCall({ pattern: "TODO", path: "." }, theme, context);
    expect(defaultCall.render(80)).toEqual(["ffgrep /TODO/ in ."]);

    const content = [{ type: "text", text: "first\nsecond\nthird" }];
    const collapsed = tool.renderResult({ content }, { expanded: false }, theme, context);
    expect(collapsed.render(80)).toEqual(["first ... (2 more lines)"]);

    const expanded = tool.renderResult({ content }, { expanded: true }, theme, context);
    expect(expanded.text).toBe("first\nsecond\nthird");
  });

  test("fffind result follows the expanded option", async () => {
    const setup = await start("tools-and-ui");
    const tool = toolByName(setup, "fffind");
    const context = { state: {}, invalidate: mock(() => undefined), isError: false };

    const call = tool.renderCall({ pattern: "index", path: "src" }, theme, context);
    expect(call.render(80)).toEqual(["fffind index in src"]);

    const content = [{ type: "text", text: "src/index.ts\nsrc/main.ts" }];
    const collapsed = tool.renderResult({ content }, { expanded: false }, theme, context);
    expect(collapsed.render(80)).toEqual(["src/index.ts ... (1 more lines)"]);

    const expanded = tool.renderResult({ content }, { expanded: true }, theme, context);
    expect(expanded.text).toBe("src/index.ts\nsrc/main.ts");
  });
});

describe("ffgrep per-file cap (#825)", () => {
  function grepTool(setup: { pi: { registerTool: ReturnType<typeof mock> } }) {
    const tool = setup.pi.registerTool.mock.calls
      .map(([t]) => t)
      .find((t) => t.name === "ffgrep" || t.name === "grep");
    expect(tool).toBeDefined();
    return tool;
  }

  // Grep cursors advance by file offset, so maxMatchesPerFile must NOT be clamped
  // to pageSize — otherwise same-file overflow is unreachable on later pages.
  test("passes a per-file cap decoupled from pageSize", async () => {
    let captured: any;
    grepImpl = (_query, options) => {
      captured = options;
      return {
        ok: true,
        value: {
          items: [],
          totalMatched: 0,
          totalFiles: 0,
          totalFilesSearched: 0,
          filteredFileCount: 0,
          nextCursor: null,
        },
      };
    };

    const setup = await start("tools-and-ui");
    const tool = grepTool(setup);
    await tool.execute("call-1", { pattern: "TODO", limit: 20 }, abortOptions().signal);

    expect(captured).toBeDefined();
    expect(captured.pageSize).toBe(20);
    expect(captured.maxMatchesPerFile).toBe(200);
    expect(captured.maxMatchesPerFile).toBeGreaterThan(captured.pageSize);
  });
});

for (const name of ["ffgrep", "fff-multi-grep"]) {
  describe(`${name} definitions_only`, () => {
    const normal = name === "ffgrep" ? "classify" : "off";
    const fallbackMode = name === "ffgrep" ? "classify" : "off";
    const params =
      name === "ffgrep"
        ? { pattern: "Needle", path: "src/" }
        : { patterns: ["Needle"], constraints: "src/" };

    async function prepare(impl: (options: any) => unknown) {
      process.env.PI_FFF_MULTIGREP = "1";
      grepImpl = (_query, options) => impl(options);
      multiGrepImpl = impl;
      const setup = await start("tools-and-ui");
      const tool = setup.pi.registerTool.mock.calls
        .map(([tool]) => tool)
        .find((tool) => tool.name === name);
      expect(tool).toBeDefined();
      return { setup, tool };
    }

    test("exposes an optional boolean and returns definitions without retrying", async () => {
      const calls: any[] = [];
      const { setup, tool } = await prepare((options) => {
        calls.push(options);
        return grepPage("fn Needle() {}", null, true);
      });
      expect(tool.parameters.properties.definitions_only).toMatchObject({
        type: "boolean",
        optional: true,
      });
      expect(tool.parameters.properties.classifyDefinitions).toBeUndefined();
      const result = await tool.execute(
        "defs",
        { ...params, definitions_only: true },
        abortOptions().signal,
      );
      expect(calls).toHaveLength(1);
      expect(calls[0].classifyDefinitions).toBe("definition_only");
      expect(result.content[0].text).toContain("fn Needle");
      expect(result.content[0].text).not.toContain("No matching definitions");
      await shutdown(setup);
    });

    test("lists files that define the exact name first on a page", async () => {
      const definition = (relativePath: string, lineContent: string, start: number) => ({
        relativePath,
        fileName: relativePath.split("/").pop(),
        lineNumber: 1,
        lineContent,
        matchRanges: [[start, start + 5]],
        isDefinition: true,
        totalFrecencyScore: 0,
      });
      const { setup, tool } = await prepare(() => {
        const page = grepPage();
        page.value.items = [
          definition("src/a.rs", "pub struct ArgValueCompletion {}", 14),
          definition("src/c.rs", "pub struct DebuggableValue(pub Value);", 31),
          definition("src/b.rs", "pub enum Value {", 9),
        ] as any;
        return page;
      });
      const result = await tool.execute(
        "exact",
        { ...params, pattern: "Value", definitions_only: true },
        abortOptions().signal,
      );
      const text = result.content[0].text;
      expect(text.indexOf("src/b.rs")).toBeGreaterThanOrEqual(0);
      expect(text.indexOf("src/b.rs")).toBeLessThan(text.indexOf("src/a.rs"));
      // A whole word inside a field type does not count as the declared name.
      expect(text.indexOf("src/a.rs")).toBeLessThan(text.indexOf("src/c.rs"));
      await shutdown(setup);
    });

    test("falls back with a notice and preserves the ordinary cursor", async () => {
      const calls: any[] = [];
      const next = { _offset: 7 };
      const { setup, tool } = await prepare((options) => {
        calls.push(options);
        if (calls.length === 1) return grepPage();
        return grepPage("Needle();", calls.length === 2 ? next : null);
      });
      const first = await tool.execute(
        "fallback",
        { ...params, definitions_only: true },
        abortOptions().signal,
      );
      expect(first.content[0].text).toContain(
        "No matching definitions found. Showing other matches:",
      );
      expect(first.content[0].text).toContain("Needle();");
      const cursor = first.content[0].text.match(/cursor="([^"]+)"/)[1];
      const second = await tool.execute(
        "next",
        { ...params, definitions_only: true, cursor },
        abortOptions().signal,
      );
      expect(calls.map((call) => call.classifyDefinitions)).toEqual([
        "definition_only",
        fallbackMode,
        fallbackMode,
      ]);
      expect(calls[2].cursor).toEqual(next);
      expect(second.content[0].text).not.toContain("No matching definitions");
      await shutdown(setup);
    });

    test("waits for a complete empty definition search before falling back", async () => {
      const calls: any[] = [];
      const { setup, tool } = await prepare((options) => {
        calls.push(options);
        if (calls.length === 1) return grepPage(undefined, { _offset: 3 });
        return calls.length === 2 ? grepPage() : grepPage("Needle();");
      });
      const first = await tool.execute(
        "partial",
        { ...params, definitions_only: true },
        abortOptions().signal,
      );
      expect(calls).toHaveLength(1);
      expect(first.content[0].text).not.toContain("No matching definitions");
      const cursor = first.content[0].text.match(/cursor="([^"]+)"/)[1];
      const second = await tool.execute(
        "finish",
        { ...params, cursor },
        abortOptions().signal,
      );
      expect(calls.map((call) => call.classifyDefinitions)).toEqual([
        "definition_only",
        "definition_only",
        fallbackMode,
      ]);
      expect(calls[2].cursor).toBeNull();
      expect(second.content[0].text).toContain("No matching definitions found");
      await shutdown(setup);
    });

    test("does not fall back at the end of previously returned definitions", async () => {
      const calls: any[] = [];
      const { setup, tool } = await prepare((options) => {
        calls.push(options);
        return calls.length === 1
          ? grepPage("fn Needle() {}", { _offset: 3 }, true)
          : grepPage();
      });
      const first = await tool.execute(
        "defs",
        { ...params, definitions_only: true },
        abortOptions().signal,
      );
      const cursor = first.content[0].text.match(/cursor="([^"]+)"/)[1];
      const second = await tool.execute(
        "last",
        { ...params, cursor },
        abortOptions().signal,
      );
      expect(calls.map((call) => call.classifyDefinitions)).toEqual([
        "definition_only",
        "definition_only",
      ]);
      expect(second.content[0].text).not.toContain("No matching definitions");
      await shutdown(setup);
    });

    test("keeps the ordinary behavior when the flag is omitted", async () => {
      const calls: any[] = [];
      const { setup, tool } = await prepare((options) => {
        calls.push(options);
        return grepPage("Needle();");
      });
      await tool.execute("ordinary", params, abortOptions().signal);
      expect(calls).toHaveLength(1);
      expect(calls[0].classifyDefinitions).toBe(normal);
      await shutdown(setup);
    });
  });
}

for (const fuzzyDefinitions of [true, false]) {
  test(`definition fallback preserves fuzzy ${fuzzyDefinitions ? "definitions" : "matches"} across pages`, async () => {
    const calls: { query: string; options: any }[] = [];
    const firstHit = fuzzyDefinitions ? 3 : 4;
    grepImpl = (query, options) => {
      calls.push({ query, options });
      if (calls.length < firstHit) return grepPage();
      return grepPage(
        fuzzyDefinitions ? "fn Needle() {}" : "Needle();",
        calls.length === firstHit ? { _offset: 9 } : null,
        fuzzyDefinitions,
      );
    };
    const setup = await start("tools-and-ui");
    const tool = setup.pi.registerTool.mock.calls
      .map(([tool]) => tool)
      .find((tool) => tool.name === "ffgrep");
    const params = {
      pattern: "Nedle", // spellchecker:disable-line
      path: "src/",
      exclude: "test/",
      definitions_only: true,
    };
    const first = await tool.execute("typo", params, abortOptions().signal);
    expect(first.content[0].text).toContain(
      fuzzyDefinitions
        ? "Maybe you meant these definitions:"
        : "Maybe you meant these other matches:",
    );
    const cursor = first.content[0].text.match(/cursor="([^"]+)"/)[1];
    const second = await tool.execute(
      "next",
      { pattern: params.pattern, cursor },
      abortOptions().signal,
    );
    expect(calls.map((call) => call.options.mode)).toEqual([
      "plain",
      "plain",
      ...Array(firstHit - 1).fill("fuzzy"),
    ]);
    expect(calls.map((call) => call.options.classifyDefinitions)).toEqual([
      "definition_only",
      "classify",
      "definition_only",
      ...(fuzzyDefinitions ? ["definition_only"] : ["classify", "classify"]),
    ]);
    expect(new Set(calls.map((call) => call.query))).toEqual(
      new Set(["src/ !test/ " + params.pattern]),
    );
    expect(calls[firstHit].options.cursor).toEqual({ _offset: 9 });
    expect(second.content[0].text).toContain("Maybe you meant");
    await shutdown(setup);
  });
}

for (const stage of [3, 4]) {
  test(`definition fallback moves past a fuzzy stage that spent its budget at stage ${stage}`, async () => {
    const calls: any[] = [];
    grepImpl = (_query, options) => {
      calls.push(options);
      if (calls.length === stage) return grepPage(undefined, { _offset: 7 });
      return grepPage();
    };
    const setup = await start("tools-and-ui");
    const tool = setup.pi.registerTool.mock.calls
      .map(([tool]) => tool)
      .find((tool) => tool.name === "ffgrep");
    const result = await tool.execute(
      "budget",
      { pattern: "Needle", definitions_only: true },
      abortOptions().signal,
    );
    expect(calls).toHaveLength(4);
    expect(calls[stage - 1]).toMatchObject({ mode: "fuzzy", enforceTimeBudget: true });
    expect(result.content[0].text).toContain(
      "No matching definitions or other matches found.",
    );
    expect(result.content[0].text).not.toContain("cursor=");
    await shutdown(setup);
  });
}

for (const stage of [1, 2, 3, 4]) {
  if (stage <= 2)
    test(`definition fallback waits for an empty partial page at stage ${stage}`, async () => {
      const calls: any[] = [];
      grepImpl = (_query, options) => {
        calls.push(options);
        if (calls.length === stage) return grepPage(undefined, { _offset: 7 });
        return grepPage();
      };
      const setup = await start("tools-and-ui");
      const tool = setup.pi.registerTool.mock.calls
        .map(([tool]) => tool)
        .find((tool) => tool.name === "ffgrep");
      const params = { pattern: "Needle", definitions_only: true };
      const first = await tool.execute("partial", params, abortOptions().signal);
      expect(calls).toHaveLength(stage);
      expect(finders[0].fileSearch).not.toHaveBeenCalled();
      const cursor = first.content[0].text.match(/cursor="([^"]+)"/)[1];
      const last = await tool.execute(
        "finish",
        { pattern: params.pattern, cursor },
        abortOptions().signal,
      );
      expect(calls).toHaveLength(5);
      expect(calls[stage].cursor).toEqual({ _offset: 7 });
      expect(calls[stage].mode).toBe(calls[stage - 1].mode);
      expect(calls[stage].classifyDefinitions).toBe(calls[stage - 1].classifyDefinitions);
      for (const call of calls.slice(stage + 1)) expect(call.cursor).toBeNull();
      expect(last.content[0].text).toContain(
        "No matching definitions or other matches found.",
      );
      await shutdown(setup);
    });

  test(`definition fallback stops after matches were returned at stage ${stage}`, async () => {
    const calls: any[] = [];
    grepImpl = (_query, options) => {
      calls.push(options);
      return calls.length === stage ? grepPage("Needle", { _offset: 7 }) : grepPage();
    };
    const setup = await start("tools-and-ui");
    const tool = setup.pi.registerTool.mock.calls
      .map(([tool]) => tool)
      .find((tool) => tool.name === "ffgrep");
    const first = await tool.execute(
      "found",
      { pattern: "Needle", definitions_only: true },
      abortOptions().signal,
    );
    expect(calls).toHaveLength(stage);
    const cursor = first.content[0].text.match(/cursor="([^"]+)"/)[1];
    await tool.execute("finish", { pattern: "Needle", cursor }, abortOptions().signal);
    expect(calls).toHaveLength(stage + 1);
    expect(calls[stage].cursor).toEqual({ _offset: 7 });
    expect(finders[0].fileSearch).not.toHaveBeenCalled();
    await shutdown(setup);
  });
}

test("empty definition search suggests other files and preserves exclusions", async () => {
  const calls: any[] = [];
  grepImpl = (query, options) => {
    calls.push({ query, options });
    if (calls.length <= 4) return grepPage();
    const page = grepPage("Needle");
    page.value.items[0].relativePath = "other/hit.rs";
    return page;
  };
  const setup = await start("tools-and-ui");
  const tool = setup.pi.registerTool.mock.calls
    .map(([tool]) => tool)
    .find((tool) => tool.name === "ffgrep");
  const result = await tool.execute(
    "suggest",
    {
      pattern: "Needle",
      path: "missing/",
      exclude: "vendor/",
      definitions_only: true,
    },
    abortOptions().signal,
  );
  expect(calls).toHaveLength(5);
  expect(calls[4].query).toBe("!vendor/ Needle");
  expect(calls[4].options).toMatchObject({
    classifyDefinitions: "off",
    enforceTimeBudget: true,
    pageSize: 3,
  });
  expect(result.content[0].text).toContain("elsewhere in the indexed project");
  expect(result.content[0].text).toContain("other/hit.rs");
  expect(result.content[0].text).not.toContain("cursor=");
  expect(result.details.totalMatched).toBe(0);
  await shutdown(setup);
});

test("empty definition search labels filename suggestions as unverified", async () => {
  fileSearchImpl = () => ({
    ok: true,
    value: {
      items: [{ relativePath: "NeedleGuide.md" }],
      scores: [{ baseScore: 200 }],
      totalMatched: 1,
      totalFiles: 1,
    },
  });
  const setup = await start("tools-and-ui");
  const tool = setup.pi.registerTool.mock.calls
    .map(([tool]) => tool)
    .find((tool) => tool.name === "ffgrep");
  const result = await tool.execute(
    "suggest",
    { pattern: "NeedleGuide", definitions_only: true },
    abortOptions().signal,
  );
  expect(result.content[0].text).toContain("contents are unverified");
  expect(result.content[0].text).toContain("NeedleGuide.md");
  expect(finders[0].grep).toHaveBeenCalledTimes(4);
  await shutdown(setup);
});

test("definition fallback stops on cancellation or a native error", async () => {
  for (const cancel of [false, true]) {
    let calls = 0;
    const controller = new AbortController();
    grepImpl = () => {
      calls++;
      if (cancel) {
        controller.abort();
        return grepPage();
      }
      return { ok: false, error: "Search failed" };
    };
    const setup = await start("tools-and-ui");
    const tool = setup.pi.registerTool.mock.calls
      .map(([tool]) => tool)
      .find((tool) => tool.name === "ffgrep");
    await expect(
      tool.execute(
        "failed",
        { pattern: "Needle", definitions_only: true },
        controller.signal,
      ),
    ).rejects.toThrow(cancel ? "Operation aborted" : "Search failed");
    expect(calls).toBe(1);
    await shutdown(setup);
  }
});

test("grep cursors keep the original finder when path changes or is omitted", async () => {
  const original = fs.mkdtempSync(path.join(agentDir, "cursor-original-"));
  const changed = fs.mkdtempSync(path.join(agentDir, "cursor-changed-"));
  const queries: string[] = [];
  grepImpl = (query) => {
    queries.push(query);
    return grepPage("Needle();", { _offset: queries.length });
  };
  const setup = await start("tools-and-ui");
  const tool = setup.pi.registerTool.mock.calls
    .map(([tool]) => tool)
    .find((tool) => tool.name === "ffgrep");
  const first = await tool.execute(
    "first",
    { pattern: "Needle", path: original },
    abortOptions().signal,
  );
  const originalFinder = finders[finders.length - 1];
  const cursor = first.content[0].text.match(/cursor="([^"]+)"/)[1];
  const second = await tool.execute(
    "second",
    { pattern: "Needle", path: changed, cursor },
    abortOptions().signal,
  );
  const next = second.content[0].text.match(/cursor="([^"]+)"/)[1];
  await tool.execute("third", { pattern: "Needle", cursor: next }, abortOptions().signal);
  expect(originalFinder.grep).toHaveBeenCalledTimes(3);
  expect(new Set(queries).size).toBe(1);
  expect(createCalls.some((options: any) => options.basePath === changed)).toBe(false);
  await shutdown(setup);
});

test("definition search resolves excludes against an outside-workspace finder", async () => {
  const root = fs.mkdtempSync(path.join(agentDir, "outside-definitions-"));
  fs.mkdirSync(path.join(root, "vendor"));
  const queries: string[] = [];
  grepImpl = (query) => {
    queries.push(query);
    return grepPage();
  };
  const setup = await start("tools-and-ui");
  const tool = setup.pi.registerTool.mock.calls
    .map(([tool]) => tool)
    .find((tool) => tool.name === "ffgrep");
  const result = await tool.execute(
    "outside",
    {
      pattern: "Needle",
      path: root,
      exclude: path.join(root, "vendor"),
      definitions_only: true,
    },
    abortOptions().signal,
  );
  expect(result.content[0].text).toContain(
    "No matching definitions or other matches found",
  );
  expect(queries.length).toBeGreaterThan(0);
  expect(queries.every((query) => query.includes("!vendor/"))).toBe(true);
  await shutdown(setup);
});

test("grep rejects cursors whose finder was destroyed", async () => {
  grepImpl = () => grepPage("Needle();", { _offset: 1 });
  const setup = await start("tools-and-ui");
  const tool = setup.pi.registerTool.mock.calls
    .map(([tool]) => tool)
    .find((tool) => tool.name === "ffgrep");
  const first = await tool.execute("first", { pattern: "Needle" }, abortOptions().signal);
  const cursor = first.content[0].text.match(/cursor="([^"]+)"/)[1];
  finders[finders.length - 1].destroy();
  await expect(
    tool.execute("expired", { pattern: "Needle", cursor }, abortOptions().signal),
  ).rejects.toThrow("Search cursor expired");
  await shutdown(setup);
});

function grepPage(line?: string, nextCursor: unknown = null, isDefinition = false) {
  const items = line
    ? [
        {
          relativePath: "src/hit.rs",
          fileName: "hit.rs",
          lineNumber: 1,
          lineContent: line,
          matchRanges: [],
          isDefinition,
          totalFrecencyScore: 0,
        },
      ]
    : [];
  return {
    ok: true,
    value: {
      items,
      totalMatched: items.length,
      totalFiles: 1,
      totalFilesSearched: 1,
      filteredFileCount: 1,
      nextCursor,
    },
  };
}
