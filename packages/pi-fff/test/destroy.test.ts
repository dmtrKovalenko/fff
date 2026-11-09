import { describe, expect, mock, test } from "bun:test";
import type { FileFinderApi } from "@ff-labs/fff-node";
import path from "node:path";
import os from "node:os";
import { AuxFinderPool } from "../src/aux-finders";
import { destroyPickers, FilePickerFactory } from "../src/file-picker";

describe("picker cleanup", () => {
  test("prefers async cleanup and retains the synchronous SDK fallback", async () => {
    const synchronous = { destroy: mock(() => {}) };
    const asynchronous = {
      destroy: mock(() => {}),
      destroyAsync: mock(async () => {}),
    };
    await destroyPickers([synchronous, asynchronous]);
    expect(synchronous.destroy).toHaveBeenCalledTimes(1);
    expect(asynchronous.destroyAsync).toHaveBeenCalledTimes(1);
    expect(asynchronous.destroy).not.toHaveBeenCalled();
  });

  test("waits for other cleanups before reporting a failure", async () => {
    const pending = deferred<void>();
    const failure = new Error("native cleanup failed");
    let settled = false;
    const closing = destroyPickers([
      {
        destroy: () => {
          throw failure;
        },
      },
      { destroy: () => {}, destroyAsync: () => pending.promise },
    ]).finally(() => {
      settled = true;
    });
    const rejected = closing.catch((error) => error);
    await Promise.resolve();
    expect(settled).toBe(false);
    pending.resolve();
    expect(await rejected).toMatchObject({ errors: [failure] });
    expect(settled).toBe(true);
  });
});

describe("auxiliary pool async cleanup", () => {
  test("waits for a pending creation and native disposal, sharing one promise", async () => {
    const creation = deferred<FileFinderApi>();
    const native = deferred<void>();
    const entered = deferred<void>();
    const finder = {
      isDestroyed: false,
      destroy: mock(() => {}),
      destroyAsync: mock(() => {
        entered.resolve();
        return native.promise;
      }),
    };
    const factory = new FilePickerFactory({
      frecencyDbPath: "unused",
      historyDbPath: "unused",
    });
    factory.create = async () => creation.promise;
    const pool = new AuxFinderPool({ pickers: factory, enableFsRootScanning: false });
    const acquiring = pool.acquire(path.join(os.tmpdir(), "fff-pending-cleanup"));
    const closing = pool.destroyAsync();
    expect(pool.destroyAsync()).toBe(closing);
    expect(finder.destroyAsync).not.toHaveBeenCalled();
    await expect(
      pool.acquire(path.join(os.tmpdir(), "fff-after-cleanup")),
    ).rejects.toThrow("closing");
    creation.resolve(finder as unknown as FileFinderApi);
    await acquiring;
    await entered.promise;
    let settled = false;
    void closing.then(() => {
      settled = true;
    });
    await Promise.resolve();
    expect(settled).toBe(false);
    native.resolve();
    await closing;
    expect(finder.destroyAsync).toHaveBeenCalledTimes(1);
    expect(finder.destroy).not.toHaveBeenCalled();
    expect(pool.size()).toBe(0);
    expect(pool.destroyAsync()).toBe(closing);
  });

  test("a failed creation does not prevent cleanup of existing finders", async () => {
    const finder = { isDestroyed: false, destroy: mock(() => {}) };
    const factory = new FilePickerFactory({
      frecencyDbPath: "unused",
      historyDbPath: "unused",
    });
    factory.create = async () => finder as unknown as FileFinderApi;
    const pool = new AuxFinderPool({ pickers: factory, enableFsRootScanning: false });
    await pool.acquire(path.join(os.tmpdir(), "fff-ready-cleanup"));
    const creation = deferred<FileFinderApi>();
    factory.create = async () => creation.promise;
    const acquiring = pool.acquire(path.join(os.tmpdir(), "fff-failed-cleanup"));
    const rejected = acquiring.catch((error) => error);
    const closing = pool.destroyAsync();
    creation.reject(new Error("creation failed"));
    expect(await rejected).toMatchObject({ message: "creation failed" });
    await closing;
    expect(finder.destroy).toHaveBeenCalledTimes(1);
    expect(pool.size()).toBe(0);
  });
});

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}
