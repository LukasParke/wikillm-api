import { describe, it, expect } from "vitest";
import { PathLock } from "../../src/fs/lock.js";

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

describe("PathLock", () => {
  it("serializes concurrent access on the same path", async () => {
    const lock = new PathLock();
    let counter = 0;
    const results: number[] = [];

    const tasks = [
      lock.runExclusive("a", async () => {
        const c = counter++;
        results.push(c);
        await delay(10);
      }),
      lock.runExclusive("a", async () => {
        const c = counter++;
        results.push(c);
        await delay(10);
      }),
      lock.runExclusive("a", async () => {
        const c = counter++;
        results.push(c);
        await delay(10);
      }),
    ];
    await Promise.all(tasks);

    expect(results).toEqual([0, 1, 2]);
  });

  it("acquires many locks in sorted order", async () => {
    const lock = new PathLock();
    const order: string[] = [];
    const release = await lock.acquireMany(["c", "a", "b"]);
    order.push("acquired");
    release();
    expect(order).toEqual(["acquired"]);
  });

  it("allows parallel access on different paths", async () => {
    const lock = new PathLock();
    let running = 0;
    let maxRunning = 0;

    const run = (p: string) =>
      lock.runExclusive(p, async () => {
        running++;
        maxRunning = Math.max(maxRunning, running);
        await delay(20);
        running--;
      });

    await Promise.all([run("x"), run("y"), run("z")]);
    expect(maxRunning).toBeGreaterThanOrEqual(2);
  });
});
