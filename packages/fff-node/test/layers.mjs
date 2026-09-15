import { strict as assert } from "node:assert";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { after, before, describe, it } from "node:test";
import { FileFinder } from "../dist/index.js";

const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "fff-node-layers-"));
fs.writeFileSync(path.join(tmp, "scanned.ts"), "export {};\n");

function open() {
  const created = FileFinder.create({ basePath: tmp, watch: false });
  assert.ok(created.ok, created.ok ? "" : created.error);
  created.value.waitForScanBlocking(5000);
  return created.value;
}

describe("fff-node index layers", { concurrency: 1 }, () => {
  let finder;

  before(() => {
    finder = open();
  });

  after(() => {
    finder.destroy();
    fs.rmSync(tmp, { recursive: true, force: true });
  });

  it("addPaths with metadata", () => {
    const added = finder.addPaths([
      "virtual/a.ts",
      { path: "virtual/b.ts", size: 42, modified: 1_700_000_000 },
      "scanned.ts",
    ]);
    assert.deepEqual(added, { ok: true, value: 2 });
    const found = finder.fileSearch("virtual/b");
    assert.ok(found.ok);
    assert.equal(found.value.items[0].relativePath, "virtual/b.ts");
    assert.equal(found.value.items[0].size, 42);
  });

  it("createLayer / listLayers", () => {
    assert.deepEqual(finder.createLayer("second"), { ok: true, value: 2 });
    assert.deepEqual(finder.currentLayer(), { ok: true, value: 2 });
    finder.addPaths(["second/x.ts"]);
    const layers = finder.listLayers();
    assert.ok(layers.ok);
    assert.deepEqual(
      layers.value.map((l) => l.label),
      ["session", "second"],
    );
  });

  it("save / load layer and base", () => {
    const layerFile = path.join(tmp, "second.fff");
    const baseFile = path.join(tmp, "base.fff");
    assert.ok(finder.saveLayer(2, layerFile).ok);
    assert.ok(finder.saveLayer(0, baseFile).ok);

    const built = path.join(tmp, "built.fff");
    const entries = Array.from({ length: 500 }, (_, i) => `gen/${i % 7}/m${i}.ts`);
    assert.ok(FileFinder.buildLayerFile(entries, built, "generated").ok);

    const other = open();
    try {
      assert.ok(other.loadBase(baseFile).ok);
      assert.ok(other.waitForScanBlocking(5000).ok);
      assert.deepEqual(other.loadLayer(layerFile), { ok: true, value: 2 });
      const id = other.loadLayer(built);
      assert.ok(id.ok);
      assert.deepEqual(other.layerFileCount(id.value), { ok: true, value: 500 });
      const found = other.fileSearch("m499");
      assert.ok(found.ok);
      assert.equal(found.value.items[0].relativePath, "gen/2/m499.ts");
      assert.equal(other.loadLayer(path.join(tmp, "missing.fff")).ok, false);
    } finally {
      other.destroy();
    }
  });
});
