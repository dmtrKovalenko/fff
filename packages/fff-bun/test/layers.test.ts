import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { FileFinder } from "../src/index";

const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "fff-layers-"));
fs.writeFileSync(path.join(tmp, "scanned.ts"), "export {};\n");

function open(): FileFinder {
  const created = FileFinder.create({ basePath: tmp, watch: false });
  if (!created.ok) throw new Error(created.error);
  const finder = created.value;
  finder.waitForScanBlocking(5000);
  return finder;
}

describe("FileFinder - index layers", () => {
  let finder: FileFinder;

  beforeAll(() => {
    finder = open();
  });

  afterAll(() => {
    finder.destroy();
    fs.rmSync(tmp, { recursive: true, force: true });
  });

  test("addPaths lands in the session layer and dedups", () => {
    const added = finder.addPaths([
      "virtual/a.ts",
      { path: "virtual/b.ts", size: 42, modified: 1_700_000_000 },
      "scanned.ts",
      "virtual/a.ts",
    ]);
    expect(added).toEqual({ ok: true, value: 2 });
    expect(finder.layerFileCount(1)).toEqual({ ok: true, value: 2 });

    const found = finder.fileSearch("virtual");
    expect(found.ok).toBe(true);
    if (found.ok) {
      const paths = found.value.items.map((i) => i.relativePath).sort();
      expect(paths).toEqual(["virtual/a.ts", "virtual/b.ts"]);
      expect(found.value.items.find((i) => i.relativePath === "virtual/b.ts")?.size).toBe(
        42,
      );
    }
  });

  test("createLayer / listLayers / currentLayer", () => {
    const id = finder.createLayer("second");
    expect(id).toEqual({ ok: true, value: 2 });
    expect(finder.currentLayer()).toEqual({ ok: true, value: 2 });
    finder.addPaths(["second/x.ts"]);

    const layers = finder.listLayers();
    expect(layers.ok).toBe(true);
    if (layers.ok) {
      expect(layers.value.map((l) => l.label)).toEqual(["session", "second"]);
      expect(layers.value[1].liveFileCount).toBe(1);
      expect(layers.value[1].isOpen).toBe(true);
    }
  });

  test("saveLayer / loadLayer round-trip", () => {
    const file = path.join(tmp, "second.fff");
    const saved = finder.saveLayer(2, file);
    expect(saved.ok).toBe(true);
    if (saved.ok) expect(saved.value).toBe(fs.statSync(file).size);

    const other = open();
    try {
      const loaded = other.loadLayer(file);
      expect(loaded).toEqual({ ok: true, value: 2 });
      expect(other.layerFileCount(2)).toEqual({ ok: true, value: 1 });
      const found = other.fileSearch("second/x");
      expect(found.ok && found.value.items[0]?.relativePath).toBe("second/x.ts");
    } finally {
      other.destroy();
    }
  });

  test("buildLayerFile builds without an instance", () => {
    const file = path.join(tmp, "built.fff");
    const entries = Array.from({ length: 2000 }, (_, i) => `gen/${i % 13}/mod${i}.ts`);
    const built = FileFinder.buildLayerFile(entries, file, "generated");
    expect(built.ok).toBe(true);

    const loaded = finder.loadLayer(file);
    expect(loaded.ok).toBe(true);
    if (loaded.ok) {
      expect(finder.layerFileCount(loaded.value)).toEqual({ ok: true, value: 2000 });
      const found = finder.fileSearch("mod1999");
      expect(found.ok && found.value.items[0]?.relativePath).toBe("gen/10/mod1999.ts");
    }
  });

  test("saveLayer(0) / loadBase restores the scan", () => {
    const file = path.join(tmp, "base.fff");
    expect(finder.saveLayer(0, file).ok).toBe(true);

    const other = open();
    try {
      const loaded = other.loadBase(file);
      expect(loaded.ok).toBe(true);
      expect(other.waitForScanBlocking(5000).ok).toBe(true);
      const found = other.fileSearch("scanned");
      expect(found.ok && found.value.items[0]?.relativePath).toBe("scanned.ts");
    } finally {
      other.destroy();
    }
  });

  test("errors surface as Result", () => {
    expect(finder.loadLayer(path.join(tmp, "missing.fff")).ok).toBe(false);
    expect(finder.saveLayer(42, path.join(tmp, "nope.fff")).ok).toBe(false);
  });
});
