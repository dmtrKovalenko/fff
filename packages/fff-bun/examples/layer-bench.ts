#!/usr/bin/env bun
/**
 * Index layer benchmark: fff layers driven over FFI vs a naive JS index.
 *
 * The JS baseline keeps paths in an array + Set, persists with JSON, and
 * searches with a substring filter. Each fff number is the FFI call in
 * isolation, so the comparison isolates per-call overhead vs batching.
 *
 * Usage:
 *   bun examples/layer-bench.ts [file_count] [iterations]
 *
 *   file_count  default: 100000
 *   iterations  default: 5  (best + median reported)
 */

import { performance } from "node:perf_hooks";
import { mkdtempSync, rmSync, statSync, writeFileSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { FileFinder, type Result } from "../src/index";

const fileCount = Number(process.argv[2] ?? 100_000);
const iterations = Number(process.argv[3] ?? 5);
if (![fileCount, iterations].every((n) => Number.isSafeInteger(n) && n > 0)) {
  throw new Error("file_count and iterations must be positive integers");
}
const work = mkdtempSync(join(tmpdir(), "fff-layer-bench-"));
const queries = ["handler", "pkg7 service", "idx99"];

const paths = syntheticPaths(fileCount);
function checked<T>(result: Result<T>): T {
  if (!result.ok) throw new Error(result.error);
  return result.value;
}
console.log(`files:      ${fileCount}`);
console.log(`iterations: ${iterations}`);
console.log();

function bench(name: string, fn: () => void, setup?: () => void): number {
  const times: number[] = [];
  for (let i = 0; i < iterations; i++) {
    setup?.();
    const t0 = performance.now();
    fn();
    times.push(performance.now() - t0);
  }
  times.sort((a, b) => a - b);
  const best = times[0]!;
  const median = times[Math.floor(times.length / 2)]!;
  console.log(`${name.padEnd(46)} best ${fmt(best)}  median ${fmt(median)}`);
  return best;
}

function heap(label: string): void {
  Bun.gc(true);
  const mb = (process.memoryUsage().rss / 1024 / 1024).toFixed(1);
  console.log(`${label.padEnd(46)} rss ${mb} MB`);
}

// --- fff over FFI --------------------------------------------------------

// A fresh instance per iteration keeps layer compaction out of the timings.
let fff = openFinder();
const fresh = () => {
  fff.destroy();
  fff = openFinder();
};

heap("baseline rss");
console.log("\n== fff (FFI) ==");

bench("addPaths  one batch", () => checked(fff.addPaths(paths)), fresh);
bench(
  "addPaths  batches of 1k",
  () => {
    for (let i = 0; i < paths.length; i += 1000)
      checked(fff.addPaths(paths.slice(i, i + 1000)));
  },
  fresh,
);
bench(
  "addPaths  batches of 100",
  () => {
    for (let i = 0; i < paths.length; i += 100)
      checked(fff.addPaths(paths.slice(i, i + 100)));
  },
  fresh,
);
const trickle = 1000;
bench(`addPaths  one at a time (${trickle} new files into ${fileCount})`, () => {
  for (let i = 0; i < trickle; i++)
    checked(fff.addPaths([`trickle/${Math.random()}/file_${i}.ts`]));
});

const layerFile = join(work, "layer.fff");
bench("buildLayerFile (off-instance)", () => {
  checked(FileFinder.buildLayerFile(paths, layerFile, "built"));
});
console.log(`${"layer file size".padEnd(46)} ${fmtBytes(statSync(layerFile).size)}`);
bench("loadLayer into empty instance", () => checked(fff.loadLayer(layerFile)), fresh);
bench(`loadLayer with all ${fileCount} already indexed (dedup)`, () =>
  checked(fff.loadLayer(layerFile)));
fresh();
const loaded = checked(fff.loadLayer(layerFile));
bench(`saveLayer (${fileCount})`, () => {
  checked(fff.saveLayer(loaded, join(work, "resave.fff")));
});

heap("rss with layers");
const total = checked(fff.listLayers()).reduce((n, l) => n + l.liveFileCount, 0);
console.log(`${"live files across layers".padEnd(46)} ${total}`);

for (const query of queries) {
  bench(`fileSearch "${query}"`, () => {
    checked(fff.fileSearch(query, { pageSize: 50 }));
  });
}

// --- naive JS index ------------------------------------------------------

console.log("\n== naive JS (array + Set, JSON on disk) ==");
heap("rss before js index");

let jsIndex: string[] = [];
let jsSet = new Set<string>();
bench("push  one batch", () => {
  jsIndex = [];
  jsSet = new Set();
  jsAdd(jsIndex, jsSet, paths);
});
bench("push  batches of 1k", () => {
  jsIndex = [];
  jsSet = new Set();
  for (let i = 0; i < paths.length; i += 1000) {
    jsAdd(jsIndex, jsSet, paths.slice(i, i + 1000));
  }
});
bench(`push  one at a time (${trickle} new files)`, () => {
  for (let i = 0; i < trickle; i++) {
    jsAdd(jsIndex, jsSet, [`trickle/${Math.random()}/file_${i}.ts`]);
  }
});

const jsonFile = join(work, "index.json");
bench("JSON.stringify + write", () => {
  writeFileSync(jsonFile, JSON.stringify(jsIndex));
});
console.log(`${"json file size".padEnd(46)} ${fmtBytes(statSync(jsonFile).size)}`);
bench("read + JSON.parse + rebuild Set", () => {
  jsIndex = JSON.parse(readFileSync(jsonFile, "utf8"));
  jsSet = new Set(jsIndex);
});
heap("rss with js index");

for (const query of queries) {
  const words = query.toLowerCase().split(" ");
  bench(`substring filter "${query}"`, () => {
    const hits: string[] = [];
    for (const p of jsIndex) {
      const lower = p.toLowerCase();
      if (words.every((w) => lower.includes(w))) hits.push(p);
    }
  });
}

fff.destroy();
rmSync(work, { recursive: true, force: true });

// --- utilities -----------------------------------------------------------

function openFinder(): FileFinder {
  const finder = checked(FileFinder.create({ basePath: work, disableWatch: true }));
  if (!checked(finder.waitForScanBlocking(5000)))
    throw new Error("Initial scan timed out");
  return finder;
}

function jsAdd(index: string[], seen: Set<string>, batch: string[]): void {
  for (const p of batch) {
    if (seen.has(p)) continue;
    seen.add(p);
    index.push(p);
  }
}

function syntheticPaths(n: number): string[] {
  const dirs = ["core", "ui", "api", "util"];
  const names = ["handler", "service", "model", "view", "index"];
  const exts = ["ts", "rs", "go", "py"];
  const out: string[] = Array.from({ length: n }, () => "");
  for (let i = 0; i < n; i++) {
    out[i] =
      `packages/pkg${i % 40}/src/${dirs[i % 4]}/${names[i % 5]}_${i}.${exts[i % 4]}`;
  }
  return out;
}

function fmt(ms: number): string {
  return ms < 1
    ? `${(ms * 1000).toFixed(0).padStart(7)} µs`
    : `${ms.toFixed(2).padStart(7)} ms`;
}

function fmtBytes(n: number): string {
  return `${(n / 1024 / 1024).toFixed(2)} MB (${n} bytes)`;
}
