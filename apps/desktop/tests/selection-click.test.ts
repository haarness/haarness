import test from "node:test";
import assert from "node:assert/strict";
import { markSelection, selectedByClick, type SelectionState } from "../src/renderer/selection-click";

// Stand-ins for DOM nodes: only identity matters to these checks.
const row = { name: "row" } as unknown as Node;
const text = { name: "text" } as unknown as Node;
const other = { name: "other row text" } as unknown as Node;

function selection(
  anchor: Node | null,
  anchorOffset: number,
  focus: Node | null,
  focusOffset: number,
  over: Node[] = [],
): SelectionState {
  return {
    isCollapsed: anchor === focus && anchorOffset === focusOffset,
    rangeCount: anchor ? 1 : 0,
    anchorNode: anchor,
    anchorOffset,
    focusNode: focus,
    focusOffset,
    getRangeAt: () => ({ intersectsNode: (node: Node) => over.includes(node) }),
  };
}
const none = selection(null, 0, null, 0);
const press = (x: number, y: number, state: SelectionState) =>
  markSelection({ clientX: x, clientY: y, currentTarget: row }, state);
const click = (x: number, y: number, detail = 1) => ({ clientX: x, clientY: y, detail });

test("a plain click toggles the row", () => {
  const mark = press(10, 10, none);
  assert.equal(selectedByClick(row, click(11, 10), mark, none), false);
  // A press that leaves an earlier selection elsewhere untouched is still a click.
  const elsewhere = selection(other, 0, other, 5, [other]);
  assert.equal(selectedByClick(row, click(10, 10), press(10, 10, elsewhere), elsewhere), false);
});

test("a drag that selects the row's text does not toggle it", () => {
  const mark = press(10, 10, none);
  const after = selection(text, 0, text, 8, [row]);
  assert.equal(selectedByClick(row, click(80, 10), mark, after), true);
  // A double-click selects a word without moving the pointer: the selection changed.
  assert.equal(selectedByClick(row, click(10, 10, 2), mark, after), true);
});

test("selecting the same text in another row is still a selection there", () => {
  // The same string, but other boundary points: the pointer went down with
  // the first row's text selected and ended with this row's.
  const first = selection(other, 0, other, 8, [other]);
  const mark = press(10, 40, first);
  const second = selection(text, 0, text, 8, [row]);
  assert.equal(selectedByClick(row, click(80, 40), mark, second), true);
});

test("dragging text already selected in the row is not a click, even if the browser drops it", () => {
  const held = selection(text, 0, text, 8, [row]);
  const mark = press(20, 10, held);
  assert.equal(selectedByClick(row, click(60, 30), mark, none), true);
  // Clicking inside the selection without moving clears it and toggles, as a click should.
  assert.equal(selectedByClick(row, click(20, 10), mark, none), false);
});

test("keyboard activation and a click without a press always toggle", () => {
  const after = selection(text, 0, text, 8, [row]);
  assert.equal(selectedByClick(row, click(80, 10, 0), press(10, 10, none), after), false);
  assert.equal(selectedByClick(row, click(80, 10), undefined, after), false);
});
