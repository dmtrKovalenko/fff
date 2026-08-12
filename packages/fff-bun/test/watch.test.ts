import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { mkdtempSync, realpathSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { WatchEvent } from "../src/fff-api";
import { FileFinder } from "../src/index";

/**
 * Integration test: filesystem watch subscriptions.
 *
 * Threadsafe JSCallback delivery happens on the JS event loop, so all
 * assertions poll with `Bun.sleep` to keep the loop alive.
 */

const POLL_INTERVAL_MS = 50;
const EVENT_TIMEOUT_MS = 10_000;
/** Grace period to assert an event did NOT arrive. */
const SILENCE_MS = 700;

function sleep(ms: number) {
  return Bun.sleep(ms);
}

/** Poll until `predicate` is true or the timeout expires. */
async function waitFor(
  predicate: () => boolean,
  timeoutMs = EVENT_TIMEOUT_MS,
): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (predicate()) return true;
    await sleep(POLL_INTERVAL_MS);
  }
  return predicate();
}

function hasEventFor(events: WatchEvent[], fileName: string): boolean {
  return events.some((e) => e.path.endsWith(`/${fileName}`));
}

async function createReadyFinder(baseDir: string): Promise<FileFinder> {
  const result = FileFinder.create({ basePath: baseDir });
  expect(result.ok).toBe(true);
  if (!result.ok) throw new Error(result.error);
  const finder = result.value;

  const scanned = await finder.waitForScan(10_000);
  expect(scanned.ok).toBe(true);

  // Wait for the background watcher to come online, then let FSEvents settle.
  await waitFor(() => {
    const progress = finder.getScanProgress();
    return progress.ok && progress.value.isWatcherReady;
  });
  await sleep(400);

  return finder;
}

describe("FileFinder - Watch Subscriptions", () => {
  let baseDir: string;
  let finder: FileFinder;

  beforeAll(async () => {
    baseDir = realpathSync(mkdtempSync(join(tmpdir(), "fff-watch-test-")));
    writeFileSync(join(baseDir, "seed-one.txt"), "seed one\n");
    writeFileSync(join(baseDir, "seed-two.js"), "// seed two\n");

    finder = await createReadyFinder(baseDir);
  }, 30_000);

  afterAll(() => {
    finder?.destroy();
    rmSync(baseDir, { recursive: true, force: true });
  }, 20_000);

  test("watch delivers batches for matching files only", async () => {
    const received: WatchEvent[] = [];
    const batchSizes: number[] = [];

    const sub = finder.watch("**/*.txt", (events) => {
      batchSizes.push(events.length);
      received.push(...events);
    });
    expect(sub.ok).toBe(true);
    if (!sub.ok) return;

    writeFileSync(join(baseDir, "watched.txt"), "hello\n");
    writeFileSync(join(baseDir, "ignored.js"), "// nope\n");

    const gotTxt = await waitFor(() => hasEventFor(received, "watched.txt"));
    expect(gotTxt).toBe(true);

    // Give the .js event (if any were mistakenly routed) a chance to arrive.
    await sleep(SILENCE_MS);
    expect(received.some((e) => e.path.endsWith(".js"))).toBe(false);
    expect(batchSizes.every((n) => n > 0)).toBe(true);
    for (const event of received) {
      expect(["created", "modified", "removed", "rescan", "renamed"]).toContain(
        event.kind,
      );
    }

    sub.value();
  }, 20_000);

  test("a rename arrives as one renamed event carrying both paths", async () => {
    const received: WatchEvent[] = [];

    const sub = finder.watch((events) => {
      received.push(...events);
    });
    expect(sub.ok).toBe(true);
    if (!sub.ok) return;

    const from = join(baseDir, "move-me.txt");
    const to = join(baseDir, "moved.txt");
    writeFileSync(from, "move me\n");
    // Let the create land so the rename is its own batch.
    expect(await waitFor(() => hasEventFor(received, "move-me.txt"))).toBe(true);
    received.length = 0;

    renameSync(from, to);

    const gotRename = await waitFor(() =>
      received.some((e) => e.kind === "renamed" && e.path === to),
    );
    expect(gotRename).toBe(true);

    const renamed = received.find((e) => e.kind === "renamed");
    expect(renamed?.from).toBe(from);
    // The move is one event, not a removal plus a creation.
    expect(received.some((e) => e.kind === "removed" && e.path === from)).toBe(false);

    sub.value();
  }, 20_000);

  test("per-event consumption is a one-line loop over watch", async () => {
    const received: WatchEvent[] = [];

    const sub = finder.watch("**/*.txt", (events) => {
      for (const event of events) {
        expect(typeof event.path).toBe("string");
        expect(typeof event.kind).toBe("string");
        received.push(event);
      }
    });
    expect(sub.ok).toBe(true);
    if (!sub.ok) return;

    writeFileSync(join(baseDir, "fanout-one.txt"), "1\n");
    writeFileSync(join(baseDir, "fanout-two.txt"), "2\n");

    const gotBoth = await waitFor(
      () =>
        hasEventFor(received, "fanout-one.txt") &&
        hasEventFor(received, "fanout-two.txt"),
    );
    expect(gotBoth).toBe(true);

    sub.value();
  }, 20_000);

  test("watch without a pattern receives events for the whole tree", async () => {
    const received: WatchEvent[] = [];

    const sub = finder.watch((events) => {
      received.push(...events);
    });
    expect(sub.ok).toBe(true);
    if (!sub.ok) return;

    writeFileSync(join(baseDir, "no-pattern.txt"), "1\n");
    writeFileSync(join(baseDir, "no-pattern.js"), "// 2\n");

    const gotBoth = await waitFor(
      () =>
        hasEventFor(received, "no-pattern.txt") && hasEventFor(received, "no-pattern.js"),
    );
    expect(gotBoth).toBe(true);

    sub.value();
  }, 20_000);

  test("unsubscribe stops delivery and is idempotent", async () => {
    const received: WatchEvent[] = [];

    const sub = finder.watch("**/*.txt", (events) => {
      received.push(...events);
    });
    expect(sub.ok).toBe(true);
    if (!sub.ok) return;

    writeFileSync(join(baseDir, "before-unsub.txt"), "before\n");
    const gotBefore = await waitFor(() => hasEventFor(received, "before-unsub.txt"));
    expect(gotBefore).toBe(true);

    sub.value();
    const countAfterUnsub = received.length;

    writeFileSync(join(baseDir, "after-unsub.txt"), "after\n");
    await sleep(SILENCE_MS);
    expect(received.length).toBe(countAfterUnsub);
    expect(hasEventFor(received, "after-unsub.txt")).toBe(false);

    // Idempotent: repeating the handle is safe.
    sub.value();
    sub.value();
  }, 20_000);

  test("watch on a directory respects the ignore option", async () => {
    const received: WatchEvent[] = [];

    const sub = finder.watch(
      baseDir,
      (events) => {
        received.push(...events);
      },
      { ignore: ["*.log"] },
    );
    expect(sub.ok).toBe(true);
    if (!sub.ok) return;

    writeFileSync(join(baseDir, "dir-shape.txt"), "hello\n");
    writeFileSync(join(baseDir, "dir-noise.log"), "noise\n");

    const got = await waitFor(() => hasEventFor(received, "dir-shape.txt"));
    expect(got).toBe(true);
    // the ignore glob filtered the .log file out
    expect(hasEventFor(received, "dir-noise.log")).toBe(false);

    sub.value();
    const countAfter = received.length;
    writeFileSync(join(baseDir, "dir-after-unsub.txt"), "late\n");
    await sleep(SILENCE_MS);
    expect(received.length).toBe(countAfter);
  }, 20_000);

  test("destroy with an active watcher does not crash", async () => {
    const otherDir = realpathSync(mkdtempSync(join(tmpdir(), "fff-watch-destroy-")));
    try {
      writeFileSync(join(otherDir, "seed.txt"), "seed\n");
      const other = await createReadyFinder(otherDir);

      const received: WatchEvent[] = [];
      const sub = other.watch("**/*.txt", (events) => {
        received.push(...events);
      });
      expect(sub.ok).toBe(true);
      if (!sub.ok) return;

      writeFileSync(join(otherDir, "active.txt"), "active\n");
      await waitFor(() => hasEventFor(received, "active.txt"), 5_000);

      // Destroy without unsubscribing first; must clean up the subscription.
      other.destroy();
      expect(other.isDestroyed).toBe(true);

      // Unsubscribing after destroy is a safe no-op.
      sub.value();

      // Keep the loop alive briefly so any stray delivery would surface.
      await sleep(SILENCE_MS);
    } finally {
      rmSync(otherDir, { recursive: true, force: true });
    }
  }, 20_000);
});
