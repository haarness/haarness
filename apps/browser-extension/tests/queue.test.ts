import assert from "node:assert/strict";
import test from "node:test";
import { createQueues } from "../src/core/queue";

const tick = () => new Promise((resolve) => setTimeout(resolve, 5));

test("one chat's commands run in order; other chats run alongside", async () => {
  const queues = createQueues();
  const log: string[] = [];
  let release!: () => void;
  const gate = new Promise<void>((resolve) => (release = resolve));
  const first = queues.run("a", "1", async () => {
    log.push("a1 start");
    await gate;
    log.push("a1 end");
  });
  const second = queues.run("a", "2", async () => void log.push("a2"));
  const other = queues.run("b", "3", async () => void log.push("b1"));
  await other;
  await tick();
  assert.deepEqual(log, ["a1 start", "b1"]);
  assert.equal(queues.pending("a"), 2);
  release();
  await Promise.all([first, second]);
  assert.deepEqual(log, ["a1 start", "b1", "a1 end", "a2"]);
  assert.equal(queues.pending(), 0);
});

test("a cancelled command still runs once, with an aborted signal, so it is answered", async () => {
  const queues = createQueues();
  let release!: () => void;
  const running = queues.run("a", "1", (signal) => new Promise<boolean>((resolve) => {
    release = () => resolve(signal.aborted);
  }));
  const waiting = queues.run("a", "2", async (signal) => signal.aborted);
  await tick();
  assert.equal(queues.cancel("2"), true);
  assert.equal(queues.cancel("missing"), false);
  release();
  assert.equal(await running, false);
  assert.equal(await waiting, true);
});

test("cancelAll aborts running and waiting commands (Pause)", async () => {
  const queues = createQueues();
  const seen: boolean[] = [];
  const running = queues.run("a", "1", (signal) => new Promise<void>((resolve) => {
    if (signal.aborted) return resolve(void seen.push(true));
    signal.addEventListener("abort", () => resolve(void seen.push(true)));
  }));
  const failed = queues.run("a", "2", async () => { throw new Error("boom"); });
  const after = queues.run("a", "3", async (signal) => void seen.push(signal.aborted));
  queues.cancelAll();
  await running;
  await assert.rejects(failed, /boom/);
  await after;
  assert.deepEqual(seen, [true, true]);
});
