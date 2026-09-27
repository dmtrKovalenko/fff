import { after, before, describe, it } from "node:test";
import { strict as assert } from "node:assert";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { FileFinder } from "../dist/index.js";

const __dirname = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(__dirname, "..", "..", "..");
const normalizePath = (p) => p.replace(/\\/g, "/");

/** @type {import("../dist/finder.js").FileFinder | null} */
let finder = null;

describe("fff-node", { concurrency: 1 }, () => {
  before(async () => {
    const result = FileFinder.create({ basePath: REPO_ROOT });
    assert.ok(result.ok, `create failed: ${!result.ok ? result.error : ""}`);
    finder = result.value;

    const wait = await finder.waitForScan(5_000);
    if (!wait.ok) {
      assert.ok(wait.ok, `waitForScan failed: ${wait.error}`);
    }

    assert.equal(wait.value, true, "scan should finish within 5s");
  });

  after(() => {
    if (finder && !finder.isDestroyed) {
      finder.destroy();
    }
  });

  it("isAvailable returns true when the native library is loadable", () => {
    assert.equal(FileFinder.isAvailable(), true);
  });

  it("getScanProgress reports >100 indexed files", () => {
    const r = finder.getScanProgress();
    assert.ok(r.ok, "getScanProgress failed");
    assert.equal(typeof r.value.scannedFilesCount, "number");
    assert.ok(
      r.value.scannedFilesCount > 100,
      `expected >100, got ${r.value.scannedFilesCount}`,
    );

    assert.equal(r.value.isScanning, false);
  });

  describe("search", { concurrency: 1 }, () => {
    it("finds the root Cargo.toml", () => {
      const r = finder.fileSearch("Cargo.toml", { pageSize: 10 });
      assert.ok(r.ok, `search failed: ${!r.ok ? r.error : ""}`);
      const paths = r.value.items.map((i) => i.relativePath);
      assert.ok(
        paths.includes("Cargo.toml"),
        `expected Cargo.toml in results, got: ${paths}`,
      );
    });

    it("finds crates/fff-c/src/lib.rs", () => {
      const r = finder.fileSearch("lib.rs", { pageSize: 20 });
      assert.ok(r.ok);
      const paths = r.value.items.map((i) => i.relativePath);
      assert.ok(
        paths.some((p) => p.includes("fff-c") && p.endsWith("lib.rs")),
        `expected fff-c/src/lib.rs, got: ${paths}`,
      );
    });

    it("respects pageSize", () => {
      const r = finder.fileSearch("lua", { pageSize: 3 });
      assert.ok(r.ok);
      assert.ok(
        r.value.items.length <= 3,
        `expected <=3, got ${r.value.items.length}`,
      );
    });

    it("items contain all required fields", () => {
      const r = finder.fileSearch("Cargo.toml", { pageSize: 1 });
      assert.ok(r.ok);
      const item = r.value.items[0];
      for (const f of ["relativePath", "fileName", "gitStatus"]) {
        assert.equal(typeof item[f], "string", `${f} should be a string`);
      }
      for (const f of ["size", "modified"]) {
        assert.equal(typeof item[f], "number", `${f} should be a number`);
      }
    });

    it("scores contain all required fields", () => {
      const r = finder.fileSearch("Cargo.toml", { pageSize: 1 });
      assert.ok(r.ok);
      assert.ok(r.value.scores.length > 0);
      const s = r.value.scores[0];
      for (const f of ["total", "baseScore"]) {
        assert.equal(typeof s[f], "number", `${f} should be a number`);
      }
      assert.equal(typeof s.matchType, "string");
    });

    it("exact filename query decodes exactMatch=true (#888)", () => {
      const r = finder.fileSearch("Cargo.toml", { pageSize: 5 });
      assert.ok(r.ok);
      const idx = r.value.items.findIndex((i) => i.relativePath === "Cargo.toml");
      assert.ok(idx >= 0, "root Cargo.toml should be in results");
      const s = r.value.scores[idx];
      assert.equal(s.matchType, "exact_filename");
      assert.equal(s.exactMatch, true);
      assert.equal(typeof s.pathAlignmentBonus, "number");
    });

    it("totalMatched <= totalFiles", () => {
      const r = finder.fileSearch("rs", { pageSize: 1 });
      assert.ok(r.ok);
      assert.ok(r.value.totalMatched > 0);
      assert.ok(r.value.totalFiles > 100);
      assert.ok(r.value.totalFiles >= r.value.totalMatched);
    });

    it("empty query returns files", () => {
      const r = finder.fileSearch("", { pageSize: 5 });
      assert.ok(r.ok, `empty query failed: ${!r.ok ? r.error : ""}`);
      assert.equal(typeof r.value.totalFiles, "number");
    });
  });

  describe("glob", { concurrency: 1 }, () => {
    it("filters by extension via raw glob pattern", () => {
      const r = finder.glob("**/*.rs", { pageSize: 50 });
      assert.ok(r.ok, `glob failed: ${!r.ok ? r.error : ""}`);
      assert.ok(r.value.items.length > 0, "expected at least one .rs file");
      for (const item of r.value.items) {
        assert.ok(
          item.relativePath.endsWith(".rs"),
          `unexpected file: ${item.relativePath}`,
        );
      }
    });

    it("returns empty result for non-matching pattern", () => {
      const r = finder.glob("**/this-extension-does-not-exist-anywhere.zzz");
      assert.ok(r.ok);
      assert.equal(r.value.items.length, 0);
    });

    it("rejects empty pattern", () => {
      const r = finder.glob("");
      assert.equal(r.ok, false);
    });

    it("respects pageSize", () => {
      const r = finder.glob("**/*.rs", { pageSize: 3 });
      assert.ok(r.ok);
      assert.ok(r.value.items.length <= 3);
    });
  });

  describe("grep", { concurrency: 1 }, () => {
    it("finds FffResult in Rust sources", () => {
      // Constrain to .rs files so the assertion doesn't depend on result ordering
      // or content-indexing timing for other file types.
      const rustResults = finder.grep("*.rs FffResult", { mode: "plain" });
      assert.ok(
        rustResults.ok,
        `grep failed: ${!rustResults.ok ? rustResults.error : ""}`,
      );
      assert.ok(
        rustResults.value.items.length > 0,
        "expected at least one .rs match",
      );
      assert.ok(
        rustResults.value.items.some((m) => m.relativePath.endsWith(".rs")),
      );

      const cResults = finder.grep("!**/*.{js,ts,rs} FffResult", {
        mode: "plain",
      });
      assert.ok(
        cResults.ok,
        `grep failed: ${!cResults.ok ? cResults.error : ""}`,
      );
      assert.ok(
        cResults.value.items.length > 0,
        "expected at least one non-js/ts/rs match",
      );
      assert.ok(
        cResults.value.items.some((m) => m.relativePath.endsWith(".h")),
      );
    });

    it("match items contain all required fields", () => {
      const r = finder.grep("FffResult", { mode: "plain" });
      assert.ok(r.ok);
      const m = r.value.items[0];
      assert.equal(typeof m.relativePath, "string");
      assert.equal(typeof m.lineNumber, "number");
      assert.ok(m.lineNumber > 0, "lineNumber is 1-based");
      assert.equal(typeof m.lineContent, "string");
      assert.ok(m.lineContent.includes("FffResult"));
      assert.equal(typeof m.col, "number");
      assert.equal(typeof m.byteOffset, "number");
      assert.ok(Array.isArray(m.matchRanges));
      console.log(m.matchRanges);
    });

    it("plain grep on text files never reports binary/definition/fuzzy flags (#888)", () => {
      const r = finder.grep("FffResult", { mode: "plain", pageSize: 100 });
      assert.ok(r.ok);
      assert.ok(r.value.items.length > 1);
      for (const m of r.value.items) {
        assert.equal(m.isBinary, false, `${m.relativePath}:${m.lineNumber} flagged binary`);
        assert.equal(m.isDefinition, undefined, `${m.relativePath}:${m.lineNumber} flagged definition`);
        assert.equal(m.fuzzyScore, undefined, `${m.relativePath}:${m.lineNumber} has fuzzyScore in plain mode`);
      }
    });

    it("fuzzy grep decodes fuzzyScore as u16", () => {
      const r = finder.grep("FfRslt", { mode: "fuzzy", pageSize: 20 });
      assert.ok(r.ok);
      assert.ok(r.value.items.length > 0);
      for (const m of r.value.items) {
        assert.equal(typeof m.fuzzyScore, "number");
        assert.ok(m.fuzzyScore >= 0 && m.fuzzyScore <= 0xffff, `fuzzyScore out of u16 range: ${m.fuzzyScore}`);
      }
    });

    it("pagination returns a second page", () => {
      const r = finder.grep("fn", { mode: "plain" });
      assert.ok(r.ok);
      if (r.value.nextCursor) {
        const r2 = finder.grep("fn", { cursor: r.value.nextCursor });
        assert.ok(r2.ok, `page 2 failed: ${!r2.ok ? r2.error : ""}`);
        assert.equal(typeof r2.value.totalMatched, "number");
      }
    });

    it("respects pageSize", () => {
      // Cap to one match per file so pageSize bounds the total deterministically.
      const unbounded = finder.grep("fn", {
        mode: "plain",
        maxMatchesPerFile: 1,
      });
      assert.ok(
        unbounded.ok,
        `grep failed: ${!unbounded.ok ? unbounded.error : ""}`,
      );
      assert.ok(unbounded.value.items.length > 2);

      const limited = finder.grep("fn", {
        mode: "plain",
        maxMatchesPerFile: 1,
        pageSize: 2,
      });
      assert.ok(limited.ok, `grep failed: ${!limited.ok ? limited.error : ""}`);
      assert.ok(
        limited.value.items.length <= 2,
        `expected <=2 items, got ${limited.value.items.length}`,
      );
      assert.ok(
        limited.value.items.length < unbounded.value.items.length,
        "limited page should yield fewer matches than the default",
      );
      assert.ok(limited.value.nextCursor !== null, "nextCursor should be set");
    });

    it("regex mode matches pub fn declarations", () => {
      const r = finder.grep("pub fn \\w+", { mode: "regex" });
      assert.ok(r.ok, `regex grep failed: ${!r.ok ? r.error : ""}`);
      assert.ok(r.value.items.length > 0);
    });

    it("decodes before/after context lines", () => {
      const r = finder.grep(
        "LOLLOWOIEJIWOIUOIWUIWUIOUWE", // the random text visible here
        {
          mode: "plain",
          beforeContext: 1,
          afterContext: 1,
          maxMatchesPerFile: 5,
        },
      );
      assert.ok(r.ok, `grep with context failed: ${!r.ok ? r.error : ""}`);

      const match = r.value.items.find(
        (m) =>
          normalizePath(m.relativePath) === "packages/fff-node/test/e2e.mjs",
      );
      assert.ok(
        match,
        `expected a single match in the codebase, got: ${r.value.items
          .map((m) => normalizePath(m.relativePath))
          .join(", ")}`,
      );
      assert.deepEqual(match.contextBefore, ["      const r = finder.grep("]);
      assert.deepEqual(match.contextAfter, ["        {"]);
    });
  });

  describe("multiGrep", { concurrency: 1 }, () => {
    it("finds lines matching any of the C FFI function names", () => {
      const r = finder.multiGrep({
        patterns: ["fff_create_instance", "fff_destroy", "fff_search"],
      });
      assert.ok(r.ok, `multiGrep failed: ${!r.ok ? r.error : ""}`);
      assert.ok(r.value.items.length > 0);
    });

    it("rejects empty patterns array", () => {
      const r = finder.multiGrep({ patterns: [] });
      assert.ok(!r.ok);
    });
  });

  it("refreshGitStatus returns a positive count", () => {
    const r = finder.refreshGitStatus();
    assert.ok(r.ok, `refreshGitStatus failed: ${!r.ok ? r.error : ""}`);
    assert.equal(typeof r.value, "number");
    assert.ok(r.value > 0);
  });

  it("isScanning returns a boolean", () => {
    assert.equal(typeof finder.isScanning(), "boolean");
  });

  describe("healthCheck", { concurrency: 1 }, () => {
    it("reports initialized state with instance", () => {
      const r = finder.healthCheck(REPO_ROOT);
      assert.ok(r.ok, `healthCheck failed: ${!r.ok ? r.error : ""}`);
      assert.equal(typeof r.value.version, "string");
      assert.ok(r.value.version.length > 0);
      assert.equal(r.value.git.available, true);
      assert.equal(r.value.git.repositoryFound, true);
      assert.equal(r.value.filePicker.initialized, true);
      assert.ok(r.value.filePicker.indexedFiles > 100);
    });

    it("static check works without instance", () => {
      const r = FileFinder.healthCheckStatic(REPO_ROOT);
      assert.ok(r.ok, `static healthCheck failed: ${!r.ok ? r.error : ""}`);
      assert.equal(typeof r.value.version, "string");
      assert.equal(r.value.filePicker.initialized, false);
      assert.equal(r.value.git.repositoryFound, true);
    });
  });

  it("create rejects a non-existent path", () => {
    const r = FileFinder.create({ basePath: "/nonexistent/fff-test-path" });
    assert.ok(!r.ok);
    assert.ok(
      r.error.toLowerCase().includes("invalid path") ||
        r.error.toLowerCase().includes("not exist"),
      `unexpected error: ${r.error}`,
    );
  });

  // This must be the last describe block since it destroys the shared instance.
  describe("after destroy", { concurrency: 1 }, () => {
    before(() => {
      finder.destroy();
    });

    it("isDestroyed is true", () => {
      assert.equal(finder.isDestroyed, true);
    });

    it("search returns an error", () => {
      const r = finder.fileSearch("test");
      assert.ok(!r.ok);
      assert.ok(r.error.includes("destroyed"));
    });

    it("grep returns an error", () => {
      const r = finder.grep("test");
      assert.ok(!r.ok);
      assert.ok(r.error.includes("destroyed"));
    });

    it("double destroy is a safe no-op", () => {
      finder.destroy();
      assert.equal(finder.isDestroyed, true);
    });
  });
});
