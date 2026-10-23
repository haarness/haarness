import { test } from "node:test";
import assert from "node:assert/strict";
import {
  FOLD_MAX,
  INSET,
  PAD,
  PITCH,
  ROW,
  ROWS_MAX,
  TOP,
  fits,
  fold,
  markAt,
  plainLine,
  promptTitle,
  readingMark,
  rowAt,
  sliceOf,
  slices,
  unfold,
  visible,
} from "../src/renderer/outline-layout";

test("the stack starts at the top, whatever the lane's height", () => {
  assert.deepEqual(fold(11, 800), { pitch: PITCH, top: TOP });
  // A growing composer shrinks the lane from the bottom; the stack stays put.
  assert.equal(fold(11, 400).top, TOP);
});

test("a session too long for the lane packs tighter", () => {
  const { pitch, top } = fold(24, 200);
  assert.ok(Math.abs(pitch - (200 - TOP - INSET) / 23) < 1e-9);
  assert.equal(top, TOP);
  // Too short for even that: the stack moves up but keeps its inset.
  assert.ok(fold(24, 30).top >= INSET);
});

test("every prompt has a dash up to the cap, then even slices", () => {
  assert.deepEqual(slices(5), [0, 1, 2, 3, 4]);
  const starts = slices(1000);
  assert.equal(starts.length, FOLD_MAX);
  assert.equal(starts[0], 0);
  assert.equal(starts[1], Math.floor(1000 / FOLD_MAX));
  assert.equal(sliceOf(starts, 0), 0);
  assert.equal(sliceOf(starts, 999), FOLD_MAX - 1);
  assert.equal(sliceOf(starts, starts[7]! - 1), 6);
});

test("the panel opens over the stack, its first row level with the first dash", () => {
  const box = unfold(10, 4, 120, 800);
  assert.equal(box.top + PAD + ROW / 2, TOP);
  assert.equal(box.size, 10 * ROW + PAD * 2);
  assert.equal(box.scroll, 0);
  assert.equal(rowAt(10, TOP, box.top, box.scroll), 0);
  assert.equal(rowAt(10, TOP + ROW * 3, box.top, box.scroll), 3);
  assert.deepEqual(visible(10, box.size, box.scroll), [0, 9]);
});

test("a long list caps the panel and scrolls to the pointer's row", () => {
  const box = unfold(1000, 500, 200, 2000);
  assert.equal(box.size, ROWS_MAX * ROW + PAD * 2);
  assert.equal(box.top + PAD + 500 * ROW + ROW / 2 - box.scroll, 200);
  assert.equal(rowAt(1000, 200, box.top, box.scroll), 500);
  assert.ok(box.scroll <= box.max);
  const [first, last] = visible(1000, box.size, box.scroll);
  assert.ok(last - first + 1 <= ROWS_MAX + 2);
  assert.ok(first <= 500 && last >= 500);
});

test("a short lane keeps the panel inside it", () => {
  const box = unfold(30, 0, TOP, 300);
  assert.ok(box.top + box.size <= 300 - INSET);
  assert.equal(rowAt(30, -50, box.top, box.scroll), -1);
});

test("a lane too short for the panel's smallest size shows no outline at all", () => {
  const smallest = INSET + ROW + PAD * 2 + INSET;
  assert.equal(fits(smallest, fold(5, smallest).top), true);
  for (const height of [smallest - 1, 30, 12, 0, -20])
    assert.equal(fits(height, fold(5, height).top), false, String(height));
  // Where it shows, the panel opens inside the lane.
  const box = unfold(5, 0, TOP, smallest);
  assert.ok(box.size > 0 && box.top + box.size <= smallest - INSET);
});

test("the reading prompt is the last one above the reading line, or the newest at the end", () => {
  const marks = [
    { id: "a", offset: 0 },
    { id: "b", offset: 400 },
    { id: "c", offset: 900 },
  ];
  assert.equal(markAt(marks, -1), -1);
  assert.equal(markAt(marks, 450), 1);
  assert.equal(readingMark(marks, 0, 2000, 80), "a");
  assert.equal(readingMark(marks, 330, 2000, 80), "b");
  assert.equal(readingMark(marks, 850, 2000, 80), "c");
  // At the bottom, or when nothing scrolls, the newest prompt is being read.
  assert.equal(readingMark(marks, 1998, 2000, 80), "c");
  assert.equal(readingMark(marks, 0, 0, 80), "c");
  assert.equal(readingMark([], 0, 100, 80), undefined);
});

test("a prompt's title is its first written line, not its attached context", () => {
  const name = (value: string) => value.split("/").pop()!;
  assert.equal(promptTitle("\n  Fix the login bug  \nmore detail", [], name, "Attachment"), "Fix the login bug");
  assert.equal(
    promptTitle("<details>\n<summary>File context: a.ts</summary>\n</details>", ["/tmp/a.png"], name, "Attachment"),
    "a.png",
  );
  assert.equal(promptTitle("", [], name, "Attachment"), "Attachment");
});

test("a prompt's title reads as text, not as the Markdown it was typed in", () => {
  const name = (value: string) => value;
  assert.equal(promptTitle("**Fix** the `parser` bug\nmore", [], name, ""), "Fix the parser bug");
  assert.equal(plainLine("Explain [the docs](https://example.com) _briefly_"), "Explain the docs briefly");
  assert.equal(plainLine("# Heading prompt"), "Heading prompt");
  assert.equal(plainLine("- [ ] a task"), "a task");
  assert.equal(plainLine("1. first step"), "first step");
  assert.equal(plainLine("> quoted *word* here"), "quoted word here");
  assert.equal(plainLine("~~old~~ new"), "old new");
  assert.equal(plainLine("![chart](out/chart.png) explained"), "chart explained");
  // Words and arithmetic that only look like markup stay as written.
  assert.equal(plainLine("rename snake_case_name"), "rename snake_case_name");
  assert.equal(plainLine("compute 2*3*4"), "compute 2*3*4");
  // A line that is nothing but marks keeps them rather than going blank.
  assert.equal(plainLine("**"), "**");
});
