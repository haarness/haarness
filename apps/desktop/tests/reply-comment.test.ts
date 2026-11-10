import { test } from "node:test";
import assert from "node:assert/strict";
import { commentSpot, quoteLine, replyCommentNote, replyQuote, withNote, type SpotInput } from "../src/renderer/reply-comment";

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
  const first = { left: 140, right: 800, top: 1060, bottom: 1080 };
  const last = { left: 140, right: 400, top: 1100, bottom: 1120 };
  const caret = (x: number) => ({ left: x, right: x, top: 1101, bottom: 1119 });
  const spot = (input: Partial<SpotInput>) =>
    commentSpot({ first, last, reply, row, rtl: false, gap: 4, button: 26, space: 8, ...input });
  // Left to right: the selection ends at the line's right.
  assert.deepEqual(spot({ caret: caret(400) }), {
    left: 20,
    width: 700,
    under: 120,
    over: 60,
    from: 280,
    backward: false,
  });
  // Right to left, in either interface: it ends at the line's left, measured from the right.
  assert.equal(spot({ caret: caret(140) }).from, 680);
  assert.equal(spot({ caret: caret(140) }).backward, true);
  assert.equal(spot({ caret: caret(140), rtl: true }).backward, true);
  // A caret the page cannot place, or one on another line, leaves it to the interface.
  assert.equal(spot({}).from, 280);
  assert.deepEqual([spot({ rtl: true }).from, spot({ rtl: true }).backward], [680, true]);
  assert.equal(spot({ caret: { ...caret(140), top: 1130, bottom: 1150 } }).from, 280);
  // A line wider than the reply (a table that scrolls) is held inside it.
  assert.equal(spot({ last: { ...last, right: 2000 }, caret: caret(2000) }).from, 700);
  assert.equal(spot({ last: { ...last, left: -50 }, caret: caret(-50), rtl: true }).from, 700);
});

test("on the reply's last line the button moves into its footer, centred on it, after its actions", () => {
  const row = { left: 100, right: 900, top: 1000, bottom: 1400 };
  const reply = { left: 120, right: 820, top: 1010, bottom: 1120 };
  const last = { left: 120, right: 180, top: 1100, bottom: 1120 };
  // The footer starts 6px under the text and is 28px high; its actions take 80px.
  const footer = { left: 120, right: 820, top: 1126, bottom: 1154 };
  const actions = { left: 150, right: 230, top: 1126, bottom: 1154 };
  const base = { first: last, last, reply, row, footer, actions, gap: 4, button: 26, space: 8 };
  // The words end before the actions do: the button follows them, 8px on.
  assert.deepEqual(commentSpot({ ...base, caret: { left: 180, right: 180, top: 1101, bottom: 1119 }, rtl: false }).footer, {
    top: 127,
    from: 118,
    backward: false,
    after: 118,
    below: 158,
  });
  // They end further along: the button starts where they end.
  const far = { ...last, right: 600 };
  assert.deepEqual(
    commentSpot({ ...base, last: far, first: far, caret: { left: 600, right: 600, top: 1101, bottom: 1119 }, rtl: false })
      .footer,
    { top: 127, from: 480, backward: false, after: 118, below: 158 },
  );
  // Right to left the footer's actions start at the right, and the button follows them leftward.
  const mirrored = { left: 710, right: 790, top: 1126, bottom: 1154 };
  const rtlLast = { left: 760, right: 820, top: 1100, bottom: 1120 };
  assert.deepEqual(
    commentSpot({ ...base, first: rtlLast, last: rtlLast, actions: mirrored, caret: { left: 760, right: 760, top: 1101, bottom: 1119 }, rtl: true })
      .footer,
    { top: 127, from: 118, backward: true, after: 118, below: 158 },
  );
  // A selection ending higher up leaves the footer alone.
  const higher = { ...last, top: 1040, bottom: 1060 };
  assert.equal(commentSpot({ ...base, first: higher, last: higher, rtl: false }).footer, undefined);
  // Without a footer drawn, nothing to avoid.
  assert.equal(commentSpot({ ...base, footer: undefined, actions: undefined, rtl: false }).footer, undefined);
});

test("the box's heading quotes at most 80 characters on one line", () => {
  assert.equal(quoteLine("Use the cache.\n\n  Then restart it."), "Use the cache. Then restart it.");
  const long = "word ".repeat(40);
  const line = quoteLine(long);
  assert.equal(line.length, 80);
  assert.ok(line.endsWith("…"));
  assert.ok(!line.slice(0, -1).endsWith(" "));
  assert.equal(quoteLine("x".repeat(80)), "x".repeat(80));
});
