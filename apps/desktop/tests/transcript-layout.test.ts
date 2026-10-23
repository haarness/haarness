import test from "node:test";
import assert from "node:assert/strict";
import { keepsRowsAbove } from "../src/renderer/measure-row";
import { lineExtra } from "../src/renderer/picture-lines";

const virtualizer = (scrollOffset: number, measured: string[] = []) => ({
  scrollOffset,
  scrollAdjustments: 0,
  itemSizeCache: new Map(measured.map((key) => [key, 100])),
});

test("a row measured again keeps what is read in place only when it lies wholly above the view, scrolling either way", () => {
  // Wholly above the view: an image coming in there moves the view with it.
  assert.equal(keepsRowsAbove({ key: "a", start: 0, end: 400 }, virtualizer(500, ["a"])), true);
  assert.equal(keepsRowsAbove({ key: "a", start: 0, end: 500 }, virtualizer(500, ["a"])), true);
  // Reaching into the view (a reply being written grows below its top) or below it: no.
  assert.equal(keepsRowsAbove({ key: "a", start: 300, end: 700 }, virtualizer(500, ["a"])), false);
  assert.equal(keepsRowsAbove({ key: "a", start: 600, end: 900 }, virtualizer(500, ["a"])), false);
  // Adjustments already made count with the offset.
  assert.equal(keepsRowsAbove({ key: "a", start: 0, end: 550 }, { ...virtualizer(500, ["a"]), scrollAdjustments: 60 }), true);
});

test("a row measured for the first time keeps the view when it starts above it, as before", () => {
  assert.equal(keepsRowsAbove({ key: "b", start: 300, end: 430 }, virtualizer(500)), true);
  assert.equal(keepsRowsAbove({ key: "b", start: 600, end: 730 }, virtualizer(500)), false);
  assert.equal(keepsRowsAbove({ key: "b", start: 0, end: 130 }, virtualizer(null as unknown as number)), false);
});

test("pictures side by side count as one line in a message's length", () => {
  // Three 120px thumbnails on one 22px line add 98px, not three times that.
  assert.equal(lineExtra([{ top: 0, bottom: 120 }, { top: 0, bottom: 120 }, { top: 2, bottom: 118 }], 22), 98);
  // On two lines, each line counts.
  assert.equal(lineExtra([{ top: 0, bottom: 120 }, { top: 130, bottom: 250 }], 22), 196);
  // A placeholder no taller than the line adds nothing.
  assert.equal(lineExtra([{ top: 0, bottom: 20 }], 22), 0);
  assert.equal(lineExtra([], 22), 0);
});
