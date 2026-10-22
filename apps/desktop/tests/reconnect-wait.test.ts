import { test } from "node:test";
import assert from "node:assert/strict";
import {
  createReconnectWait,
  isWake,
  wakeDelay,
} from "../src/renderer/reconnect-wait";

test("a stream that ends as soon as it opens waits longer each time, up to 30 s", () => {
  let now = 0;
  const wait = createReconnectWait(() => now);
  const waits: number[] = [];
  for (let attempt = 0; attempt < 9; attempt++) {
    waits.push(wait.next());
    now += waits.at(-1)!;
    // Opened, then refused or dropped at once.
    wait.opened();
    now += 50;
    wait.ended();
  }
  assert.deepEqual(
    waits,
    [500, 1000, 2000, 4000, 8000, 16000, 30000, 30000, 30000],
  );
});

test("a stream that stayed open starts the wait over, and saying it ended twice counts once", () => {
  let now = 0;
  const wait = createReconnectWait(() => now);
  // Failed connections in a row.
  wait.next();
  wait.next();
  wait.next();
  wait.opened();
  now += 10_000;
  wait.ended();
  wait.ended();
  assert.equal(wait.next(), 500);
  // A failure without a stream open in between does not start over.
  wait.ended();
  assert.equal(wait.next(), 1000);
});

test("a wake starts the wait over", () => {
  const wait = createReconnectWait(() => 0);
  for (let attempt = 0; attempt < 6; attempt++) wait.next();
  wait.reset();
  assert.equal(wait.next(), 500);
});

test("sidebar projects retry one after another after a wake, behind the open project", () => {
  assert.deepEqual(
    [wakeDelay(1000), wakeDelay(1000), wakeDelay(1100)],
    [750, 1000, 1150],
  );
  // A later wake starts again from the first slot.
  assert.equal(wakeDelay(60_000), 750);
});

test("only Desktop's wake notice counts as a wake", () => {
  assert.equal(isWake({ kind: "connection", data: "resume" }), true);
  assert.equal(
    isWake({ kind: "connection", connection: "local:/p", data: "resume" }),
    false,
  );
  assert.equal(
    isWake({ kind: "connection", connection: "local:/p", id: "s", data: "resync" }),
    false,
  );
  assert.equal(isWake({ kind: "server", data: "resume" }), false);
});
