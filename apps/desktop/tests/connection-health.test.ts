import test from "node:test";
import assert from "node:assert/strict";
import { EVENT_IDLE_MS, eventIdleMs, onlineAgain, retryWaits } from "../src/main/connection-health";

test("a wake ends the waits under way at once and leaves nothing behind", () => {
  const timers = new Map<number, () => void>();
  let next = 0;
  const waits = retryWaits(
    (run) => {
      timers.set(++next, run);
      return next as unknown as ReturnType<typeof setTimeout>;
    },
    (timer) => timers.delete(timer as unknown as number),
  );
  const ran: string[] = [];
  waits.wait(() => ran.push("notifications"), 16_000);
  const cancel = waits.wait(() => ran.push("stopped"), 30_000);
  cancel();
  assert.equal(waits.waiting, 1);
  waits.wake();
  assert.deepEqual(ran, ["notifications"]);
  assert.equal(timers.size, 0, "the backoff timer is gone");
  // Nothing waits now: a second wake, or a healthy connection, runs nothing.
  waits.wake();
  assert.deepEqual(ran, ["notifications"]);
  // A wait that ran out by itself is not run again by a wake.
  waits.wait(() => ran.push("later"), 1_000);
  [...timers.values()][0]();
  waits.wake();
  assert.deepEqual(ran, ["notifications", "later"]);
});

test("the network coming back wakes, staying online does not", async () => {
  let online = false;
  let woken = 0;
  const stop = onlineAgain(() => online, () => woken++, 5);
  await new Promise((resolve) => setTimeout(resolve, 20));
  assert.equal(woken, 0);
  online = true;
  await new Promise((resolve) => setTimeout(resolve, 20));
  assert.equal(woken, 1);
  await new Promise((resolve) => setTimeout(resolve, 20));
  assert.equal(woken, 1);
  stop();
});

test("only end-to-end tests may shorten the event stream's silence limit", () => {
  assert.equal(eventIdleMs({}), EVENT_IDLE_MS);
  assert.equal(eventIdleMs({ HAARNESS_DESKTOP_EVENT_IDLE_MS: "2000" }), EVENT_IDLE_MS);
  assert.equal(
    eventIdleMs({ HAARNESS_DESKTOP_TEST: "1", HAARNESS_DESKTOP_EVENT_IDLE_MS: "2000" }),
    2000,
  );
  assert.equal(
    eventIdleMs({ HAARNESS_DESKTOP_TEST: "1", HAARNESS_DESKTOP_EVENT_IDLE_MS: "soon" }),
    EVENT_IDLE_MS,
  );
});
