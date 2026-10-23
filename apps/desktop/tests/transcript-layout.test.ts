import test from "node:test";
import assert from "node:assert/strict";
import { keepsRowsAbove } from "../src/renderer/measure-row";
import { lineExtra } from "../src/renderer/picture-lines";

const directions = ["forward", "backward", null] as const;
const virtualizer = (
  scrollOffset: number | null,
  scrollDirection: (typeof directions)[number],
  measured: string[] = [],
) => ({
  scrollOffset,
  scrollDirection,
  scrollAdjustments: 0,
  itemSizeCache: new Map(measured.map((key) => [key, 100])),
});

test("a row measured again keeps what is read in place only when it lies wholly above the view, scrolling either way", () => {
  // TanStack's own rule leaves out scrolling up; this one decides the same way in every direction.
  for (const direction of directions) {
    const view = (measured = ["a"]) => virtualizer(500, direction, measured);
    // Wholly above the view: an image coming in there moves the view with it.
    assert.equal(keepsRowsAbove({ key: "a", start: 0, end: 400 }, view()), true, String(direction));
    assert.equal(keepsRowsAbove({ key: "a", start: 0, end: 500 }, view()), true, String(direction));
    // Reaching into the view (a reply being written grows below its top) or below it: no.
    assert.equal(keepsRowsAbove({ key: "a", start: 300, end: 700 }, view()), false, String(direction));
    assert.equal(keepsRowsAbove({ key: "a", start: 600, end: 900 }, view()), false, String(direction));
    // Adjustments already made count with the offset.
    assert.equal(keepsRowsAbove({ key: "a", start: 0, end: 550 }, { ...view(), scrollAdjustments: 60 }), true, String(direction));
  }
});

test("a row measured for the first time keeps the view when it starts above it, as before, scrolling either way", () => {
  for (const direction of directions) {
    assert.equal(keepsRowsAbove({ key: "b", start: 300, end: 430 }, virtualizer(500, direction)), true, String(direction));
    assert.equal(keepsRowsAbove({ key: "b", start: 600, end: 730 }, virtualizer(500, direction)), false, String(direction));
    assert.equal(keepsRowsAbove({ key: "b", start: 0, end: 130 }, virtualizer(null, direction)), false, String(direction));
  }
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
