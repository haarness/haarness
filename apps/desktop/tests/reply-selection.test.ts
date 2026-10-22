import { test } from "node:test";
import assert from "node:assert/strict";
import { restorePlan, textPoint } from "../src/renderer/reply-selection";

test("a text offset falls in its piece, a seam opening the later piece for a start and closing the earlier one for an end", () => {
  const lengths = [4, 1, 8];
  assert.deepEqual(textPoint(lengths, 0, "start"), [0, 0]);
  assert.deepEqual(textPoint(lengths, 4, "start"), [1, 0]);
  assert.deepEqual(textPoint(lengths, 4, "end"), [0, 4]);
  assert.deepEqual(textPoint(lengths, 13, "end"), [2, 8]);
  assert.equal(textPoint(lengths, 13, "start"), undefined);
  assert.equal(textPoint(lengths, 14, "end"), undefined);
  assert.equal(textPoint(lengths, -1, "start"), undefined);
});

test("a selection saved from the live reply lands on the same text of the saved reply, its direction kept", () => {
  // "Live markdown" in a list item and a code block, drawn in other pieces.
  const live = ["Live", " ", "markdown", "\n", "streams while working"];
  const saved = ["Live markdown", "\n", "streams while ", "working"];
  const text = live.join("");
  const start = text.indexOf("markdown");
  const end = text.indexOf("while") + "while".length;
  const forward = { anchor: start, focus: end, text: text.slice(start, end) };
  assert.deepEqual(restorePlan(forward, saved), { anchor: [0, 5], focus: [2, 13] });
  const backward = { ...forward, anchor: end, focus: start };
  assert.deepEqual(restorePlan(backward, saved), { anchor: [2, 13], focus: [0, 5] });
  // Right-to-left text is kept in reading order, so its offsets are too.
  const hebrew = ["שלום ", "עולם"];
  assert.deepEqual(restorePlan({ anchor: 5, focus: 9, text: "עולם" }, hebrew), {
    anchor: [1, 0],
    focus: [1, 4],
  });
});

test("a saved reply whose text differs at those offsets selects nothing rather than the wrong words", () => {
  const selection = { anchor: 5, focus: 13, text: "markdown" };
  assert.equal(restorePlan(selection, ["Live Markdown"]), undefined);
  assert.equal(restorePlan(selection, [" Live markdown"]), undefined);
  assert.equal(restorePlan(selection, ["Live mark"]), undefined);
  assert.equal(restorePlan({ anchor: 3, focus: 3, text: "" }, ["Live"]), undefined);
});
