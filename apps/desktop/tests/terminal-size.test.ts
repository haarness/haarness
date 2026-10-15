import test from "node:test";
import assert from "node:assert/strict";
import { pendingSize } from "../src/renderer/terminal-size";

test("a terminal size is pushed once, and a resize undone before the push sends nothing", () => {
  // Nothing pushed yet: a newly attached PTY learns the view's size.
  assert.deepEqual(pendingSize(undefined, { cols: 80, rows: 24 }), { cols: 80, rows: 24 });
  // A font size change refits to a new size, which the PTY must learn.
  assert.deepEqual(pendingSize({ cols: 80, rows: 24 }, { cols: 64, rows: 19 }), { cols: 64, rows: 19 });
  // Back to the size the PTY already has: the pending push is dropped.
  assert.equal(pendingSize({ cols: 80, rows: 24 }, { cols: 80, rows: 24 }), undefined);
});
