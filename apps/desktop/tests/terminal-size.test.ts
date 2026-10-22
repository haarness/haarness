import test from "node:test";
import assert from "node:assert/strict";
import { pendingSize, sizePusher } from "../src/renderer/terminal-size";

test("a terminal size is pushed once, and a resize undone before the push sends nothing", () => {
  // Nothing pushed yet: a newly attached PTY learns the view's size.
  assert.deepEqual(pendingSize(undefined, { cols: 80, rows: 24 }), { cols: 80, rows: 24 });
  // A font size change refits to a new size, which the PTY must learn.
  assert.deepEqual(pendingSize({ cols: 80, rows: 24 }, { cols: 64, rows: 19 }), { cols: 64, rows: 19 });
  // Back to the size the PTY already has: the pending push is dropped.
  assert.equal(pendingSize({ cols: 80, rows: 24 }, { cols: 80, rows: 24 }), undefined);
});

test("a size push that fails is sent again instead of being skipped as already pushed", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const sent: string[] = [];
  let fail = true;
  const sizes = sizePusher(async (size) => {
    sent.push(size.cols + "x" + size.rows);
    if (fail) throw new Error("offline");
  });
  sizes.schedule({ cols: 80, rows: 24 });
  // A size waits until it settles; a later one replaces it.
  sizes.schedule({ cols: 90, rows: 24 });
  t.mock.timers.tick(100);
  assert.deepEqual(sent, ["90x24"]);
  await Promise.resolve();
  await Promise.resolve();
  // The failed push is forgotten, so asking for that size again sends it.
  fail = false;
  sizes.schedule({ cols: 90, rows: 24 });
  t.mock.timers.tick(100);
  assert.deepEqual(sent, ["90x24", "90x24"]);
  await Promise.resolve();
  // Once the PTY took it, the same size is not sent a third time.
  sizes.schedule({ cols: 90, rows: 24 });
  t.mock.timers.tick(100);
  assert.deepEqual(sent, ["90x24", "90x24"]);
  sizes.dispose();
  sizes.schedule({ cols: 100, rows: 30 });
  t.mock.timers.tick(100);
  assert.deepEqual(sent, ["90x24", "90x24"]);
});

test("the size a view connects with is pushed at once, in place of one still settling", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const sent: string[] = [];
  let settle = () => {};
  const sizes = sizePusher(
    (size) =>
      new Promise<void>((resolve) => {
        sent.push(size.cols + "x" + size.rows);
        settle = resolve;
      }),
  );
  // The size a view mounts with waits to settle; connecting pushes it now,
  // and the connect waits until the PTY has it.
  sizes.schedule({ cols: 146, rows: 15 });
  let pushed = false;
  const now = sizes.now({ cols: 146, rows: 15 }).then(() => (pushed = true));
  assert.deepEqual(sent, ["146x15"]);
  await Promise.resolve();
  assert.equal(pushed, false);
  settle();
  await now;
  assert.equal(pushed, true);
  // The push it replaced does not follow, and the size is not sent again.
  t.mock.timers.tick(100);
  await sizes.now({ cols: 146, rows: 15 });
  assert.deepEqual(sent, ["146x15"]);
  sizes.dispose();
  await sizes.now({ cols: 80, rows: 24 });
  assert.deepEqual(sent, ["146x15"]);
});
