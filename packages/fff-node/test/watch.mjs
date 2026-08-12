/**
 * E2E tests for the filesystem watch subscription API
 * (watch).
 *
 * Uses a fresh temp directory so events are fully deterministic. Delivery is
 * push-based: the native dispatch thread invokes a single process-wide
 * ffi-rs trampoline (napi threadsafe_function) that routes batches to the
 * right JS callback by watch id — no polling.
 */

import { after, before, describe, it, mock } from "node:test";
import { strict as assert } from "node:assert";
import { execFile } from "node:child_process";
import { mkdtempSync, realpathSync, renameSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, sep } from "node:path";
import { promisify } from "node:util";
import { FileFinder } from "../dist/index.js";

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/** Poll `cond` every 50ms until truthy or timeout. Returns cond() result. */
async function waitFor(cond, timeoutMs = 10_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const value = cond();
    if (value) return value;
    await sleep(50);
  }
  return cond();
}

/** All events delivered to a batch-callback mock, flattened across calls. */
const deliveredEvents = (fn) => fn.mock.calls.flatMap((call) => call.arguments[0]);

/** @type {import("../dist/finder.js").FileFinder | null} */
let finder = null;
/** @type {string} */
let baseDir = "";

describe("fff-node watch", { concurrency: 1 }, () => {
  before(async () => {
    // realpath: Windows tmpdir() may return an 8.3 short path (RUNNER~1) that
    // won't prefix-match the core's canonicalized base when used as a pattern
    baseDir = realpathSync(mkdtempSync(join(tmpdir(), "fff-watch-test-")));
    const dbDir = mkdtempSync(join(tmpdir(), "fff-watch-db-"));

    // Seed files so the initial scan has content
    writeFileSync(join(baseDir, "seed-a.txt"), "seed a\n");
    writeFileSync(join(baseDir, "seed-b.js"), "// seed b\n");

    const result = FileFinder.create({
      basePath: baseDir,
      frecencyDbPath: join(dbDir, "frecency.mdb"),
      historyDbPath: join(dbDir, "history.mdb"),
    });
    assert.ok(result.ok, `create failed: ${!result.ok ? result.error : ""}`);
    finder = result.value;

    const wait = await finder.waitForScan(10_000);
    assert.ok(wait.ok, `waitForScan failed: ${!wait.ok ? wait.error : ""}`);
    assert.equal(wait.value, true, "scan should finish within 10s");

    // Wait until the background watcher is ready, then let it settle
    const ready = await waitFor(() => {
      const progress = finder.getScanProgress();
      return progress.ok && progress.value.isWatcherReady;
    }, 10_000);
    assert.ok(ready, "watcher should become ready within 10s");
    await sleep(400);
  });

  after(() => {
    if (finder && !finder.isDestroyed) {
      finder.destroy();
    }
  });

  it("watch delivers matching events and filters by pattern", async () => {
    const callback = mock.fn();
    const sub = finder.watch("**/*.txt", callback);
    assert.ok(sub.ok, `watch failed: ${!sub.ok ? sub.error : ""}`);

    writeFileSync(join(baseDir, "hello.txt"), "hello watch\n");
    writeFileSync(join(baseDir, "noise.js"), "// should not match\n");

    const delivered = await waitFor(() =>
      deliveredEvents(callback).some((e) => e.path.endsWith("hello.txt")),
    );
    assert.ok(
      delivered,
      `expected hello.txt event, got: ${JSON.stringify(deliveredEvents(callback))}`,
    );

    // Batch contract: every invocation receives exactly one WatchEvent[] argument
    for (const call of callback.mock.calls) {
      assert.equal(call.arguments.length, 1);
      assert.ok(Array.isArray(call.arguments[0]), "callback must receive event batches");
      assert.ok(call.arguments[0].length > 0, "empty batches must not be delivered");
    }

    // Give any (incorrect) .js delivery a chance to arrive, then assert absence
    await sleep(700);
    assert.ok(
      deliveredEvents(callback).every((e) => !e.path.endsWith(".js")),
      `non-matching .js events must not be delivered: ${JSON.stringify(deliveredEvents(callback))}`,
    );

    for (const event of deliveredEvents(callback)) {
      assert.equal(typeof event.path, "string");
      assert.ok(
        ["created", "modified", "removed", "rescan", "renamed"].includes(event.kind),
        `unexpected kind: ${event.kind}`,
      );
    }

    // Unsubscribe: further changes must not be delivered
    sub.value();
    const callsAfterUnsub = callback.mock.callCount();
    writeFileSync(join(baseDir, "after-unsub.txt"), "too late\n");
    await sleep(700);
    assert.equal(
      callback.mock.callCount(),
      callsAfterUnsub,
      `no calls after unsubscribe, got: ${JSON.stringify(
        callback.mock.calls.slice(callsAfterUnsub).map((c) => c.arguments),
      )}`,
    );

    // Idempotent unsubscribe must not throw
    sub.value();
  });

  it("a rename arrives as one renamed event carrying both paths", async () => {
    const callback = mock.fn();
    const sub = finder.watch(callback);
    assert.ok(sub.ok, `watch failed: ${!sub.ok ? sub.error : ""}`);

    const from = join(baseDir, "move-me.txt");
    const to = join(baseDir, "moved.txt");
    writeFileSync(from, "move me\n");
    // Let the create land so the rename lands in its own batch.
    await waitFor(() =>
      deliveredEvents(callback).some((e) => e.path.endsWith("move-me.txt")),
    );
    callback.mock.resetCalls();

    renameSync(from, to);

    const renamed = await waitFor(() =>
      deliveredEvents(callback).find((e) => e.kind === "renamed" && e.path === to),
    );
    assert.ok(
      renamed,
      `expected a renamed event, got: ${JSON.stringify(deliveredEvents(callback))}`,
    );
    assert.equal(renamed.from, from);
    // The move is one event, not a removal plus a creation.
    assert.ok(
      deliveredEvents(callback).every(
        (e) => !(e.kind === "removed" && e.path === from),
      ),
      "a recognised rename must not also report the source as removed",
    );

    sub.value();
  });

  it("per-event consumption is a one-line loop over watch", async () => {
    const perEvent = mock.fn();
    const sub = finder.watch("**/*.md", (events) => {
      for (const event of events) perEvent(event);
    });
    assert.ok(sub.ok, `watch failed: ${!sub.ok ? sub.error : ""}`);

    writeFileSync(join(baseDir, "notes.md"), "# notes\n");

    const delivered = await waitFor(() =>
      perEvent.mock.calls.some((call) => call.arguments[0].path?.endsWith("notes.md")),
    );
    assert.ok(
      delivered,
      `expected notes.md event, got: ${JSON.stringify(
        perEvent.mock.calls.map((c) => c.arguments),
      )}`,
    );

    sub.value();
  });

  it("routes events to the right callback across concurrent subscriptions", async () => {
    const txtCallback = mock.fn();
    const mdCallback = mock.fn();
    const txtSub = finder.watch("**/*.route-txt", txtCallback);
    const mdSub = finder.watch("**/*.route-md", mdCallback);
    assert.ok(txtSub.ok && mdSub.ok);

    writeFileSync(join(baseDir, "routed.route-txt"), "txt\n");
    writeFileSync(join(baseDir, "routed.route-md"), "md\n");

    const bothDelivered = await waitFor(
      () =>
        deliveredEvents(txtCallback).some((e) => e.path.endsWith("routed.route-txt")) &&
        deliveredEvents(mdCallback).some((e) => e.path.endsWith("routed.route-md")),
    );
    assert.ok(bothDelivered, "both subscriptions must receive their events");

    // No cross-talk through the shared trampoline
    assert.ok(
      deliveredEvents(txtCallback).every((e) => !e.path.endsWith(".route-md")),
      "txt subscription must not receive md events",
    );
    assert.ok(
      deliveredEvents(mdCallback).every((e) => !e.path.endsWith(".route-txt")),
      "md subscription must not receive txt events",
    );

    txtSub.value();
    mdSub.value();
  });

  it("watch on a directory respects the ignore option", async () => {
    const received = [];
    const sub = finder.watch(
      baseDir,
      (events) => {
        received.push(...events);
      },
      { ignore: ["*.skiplog"] },
    );
    assert.ok(sub.ok, `watch failed: ${!sub.ok ? sub.error : ""}`);

    writeFileSync(join(baseDir, "dir-shape.txt"), "hello\n");
    writeFileSync(join(baseDir, "dir-noise.skiplog"), "noise\n");

    const got = await waitFor(() =>
      received.some((e) => e.path.endsWith(`${sep}dir-shape.txt`)),
    );
    assert.ok(got, `expected dir-shape.txt event, got ${JSON.stringify(received)}`);
    assert.ok(
      !received.some((e) => e.path.endsWith(`${sep}dir-noise.skiplog`)),
      `ignore glob leaked: ${JSON.stringify(received)}`,
    );

    sub.value();
    const countAfter = received.length;
    writeFileSync(join(baseDir, "dir-late.txt"), "late\n");
    await sleep(700);
    assert.equal(received.length, countAfter, "no events after unsubscribe");
  });

  it("destroy() unsubscribes active watchers without crashing", async () => {
    const callback = mock.fn();
    const sub = finder.watch("**/*.txt", callback);
    assert.ok(sub.ok, `watch failed: ${!sub.ok ? sub.error : ""}`);

    finder.destroy();
    assert.equal(finder.isDestroyed, true);

    // destroy() joins the native dispatch thread; nothing may arrive after
    await sleep(700);
    assert.equal(callback.mock.callCount(), 0);

    // Late unsubscribe after destroy must be a no-op
    sub.value();
  });

  it("process exits naturally after unsubscribing (trampoline released)", async () => {
    // The threadsafe_function refs the event loop; if the trampoline is not
    // freed on last unsubscribe, this child process would hang and time out.
    const script = `
      import { mkdtempSync, writeFileSync } from "node:fs";
      import { tmpdir } from "node:os";
      import { join } from "node:path";
      import { FileFinder } from ${JSON.stringify(new URL("../dist/index.js", import.meta.url).href)};

      const dir = mkdtempSync(join(tmpdir(), "fff-watch-exit-"));
      writeFileSync(join(dir, "seed.txt"), "seed");
      const created = FileFinder.create({ basePath: dir });
      if (!created.ok) throw new Error(created.error);
      const finder = created.value;
      const wait = await finder.waitForScan(10_000);
      if (!wait.ok || !wait.value) throw new Error("waitForScan failed");
      const deadline = Date.now() + 2_000;
      let sub = finder.watch("**/*.txt", () => {});
      while (!sub.ok && Date.now() < deadline) {
        await new Promise((r) => setTimeout(r, 50));
        sub = finder.watch("**/*.txt", () => {});
      }
      if (!sub.ok) throw new Error(sub.error);
      await new Promise((r) => setTimeout(r, 300));
      sub.value();
      finder.destroy();
      console.log("DONE");
      // no process.exit(): exit must happen naturally
    `;
    const { stdout } = await promisify(execFile)(
      process.execPath,
      ["--input-type=module", "-e", script],
      { timeout: 15_000 },
    );
    assert.match(stdout, /DONE/);
  });
});
