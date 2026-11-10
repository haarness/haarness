import { test } from "node:test";
import assert from "node:assert/strict";
import { commentSpot, quoteLine, replyCommentNote, replyQuote, withNote } from "../src/renderer/reply-comment";

test("a quote keeps the selected words without stray space, one blank line at most", () => {
  assert.equal(replyQuote("  \n Use the cache.  \r\n\n\n\nThen restart it.\t\n"), "Use the cache.\n\nThen restart it.");
  assert.equal(replyQuote(" \n\t "), "");
  // Indentation inside the passage (a code block) stays.
  assert.equal(replyQuote("fn main() {\n    run();\n}"), "fn main() {\n    run();\n}");
});

test("a long quote keeps its two ends, cut between words, and says it left the middle out", () => {
  const words = Array.from({ length: 400 }, (_, index) => "word" + index).join(" ");
  const quote = replyQuote(words);
  const [start, gap, end] = quote.split("\n");
  assert.equal(gap, "…");
  assert.ok(start.startsWith("word0 word1 "));
  assert.ok(end.endsWith("word398 word399"));
  // Whole words only, and much shorter than the passage.
  assert.ok(words.startsWith(start + " "));
  assert.ok(words.endsWith(" " + end));
  assert.ok(quote.length < 1000);
  // A passage up to 1,200 characters goes in whole.
  assert.equal(replyQuote("a".repeat(1200)), "a".repeat(1200));
});

test("a long quote with no spaces is cut by length, never inside a character", () => {
  const quote = replyQuote("界".repeat(479) + "😀" + "界".repeat(800) + "😀" + "界".repeat(479));
  const [start, , end] = quote.split("\n");
  assert.equal(start, "界".repeat(479));
  assert.equal(end, "界".repeat(479));
  assert.doesNotMatch(quote, /[\ud800-\udbff](?![\udc00-\udfff])|(?<![\ud800-\udbff])[\udc00-\udfff]/);
});

test("a comment goes into the message as the quoted words, then the comment", () => {
  assert.equal(
    replyCommentNote("Use the cache.\n\nThen restart it.", "  Why restart?\nIt is idle.  "),
    "Comment on this part of your reply:\n> Use the cache.\n>\n> Then restart it.\n\nWhy restart?\nIt is idle.",
  );
  assert.equal(quoteLine("Use the cache.\n\n  Then restart it."), "Use the cache. Then restart it.");
});

test("a note follows what the message box holds, a blank line between", () => {
  assert.equal(withNote("", "note"), "note");
  assert.equal(withNote(" \n", "note"), "note");
  assert.equal(withNote("Draft text\n\n", "note"), "Draft text\n\nnote");
});

test("the Comment button starts under the selection's end, in the text's direction there, and stays inside the reply", () => {
  const row = { left: 100, right: 900, top: 1000, bottom: 1400 };
  const reply = { left: 120, right: 820, top: 1010, bottom: 1380 };
  const line = { left: 140, right: 400, top: 1100, bottom: 1120 };
  const caret = (x: number) => ({ left: x, right: x, top: 1101, bottom: 1119 });
  // Left to right: the selection ends at the line's right.
  assert.deepEqual(commentSpot(line, caret(400), reply, row, false), { top: 120, left: 20, width: 700, from: 280, backward: false });
  // Right to left, in either interface: it ends at the line's left, measured from the right.
  const back = { top: 120, left: 20, width: 700, from: 680, backward: true };
  assert.deepEqual(commentSpot(line, caret(140), reply, row, false), back);
  assert.deepEqual(commentSpot(line, caret(140), reply, row, true), back);
  // A caret the page cannot place, or one on another line, leaves it to the interface.
  assert.deepEqual(commentSpot(line, undefined, reply, row, false).from, 280);
  assert.deepEqual(commentSpot(line, undefined, reply, row, true), back);
  assert.deepEqual(commentSpot(line, { ...caret(140), top: 1130, bottom: 1150 }, reply, row, false).from, 280);
  // A line wider than the reply (a table that scrolls) is held inside it.
  assert.equal(commentSpot({ ...line, right: 2000 }, caret(2000), reply, row, false).from, 700);
  assert.equal(commentSpot({ ...line, left: -50 }, caret(-50), reply, row, true).from, 700);
});
