import test from "node:test";
import assert from "node:assert/strict";
import { lineExtra } from "../src/renderer/picture-lines";

test("pictures side by side count as one line in a message's length", () => {
  // Three 120px thumbnails on one 22px line add 98px, not three times that.
  assert.equal(lineExtra([{ top: 0, bottom: 120 }, { top: 0, bottom: 120 }, { top: 2, bottom: 118 }], 22), 98);
  // On two lines, each line counts.
  assert.equal(lineExtra([{ top: 0, bottom: 120 }, { top: 130, bottom: 250 }], 22), 196);
  // A placeholder no taller than the line adds nothing.
  assert.equal(lineExtra([{ top: 0, bottom: 20 }], 22), 0);
  assert.equal(lineExtra([], 22), 0);
});
