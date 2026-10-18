import test from "node:test";
import assert from "node:assert/strict";
import { marked } from "marked";
import { plainLine, selectionLines, sourceBlocks, type Boundary } from "../src/renderer/markdown-source";
import { projectMarkdown } from "../src/renderer/markdown-project";
import { fileLinesContext, contextLabel, fileContext } from "../src/renderer/prompt-context";

const GUIDE = [
  "# Guide", //                 1
  "", //                        2
  "Intro paragraph with *emphasis*", // 3
  "spans two lines.", //        4
  "", //                        5
  "- first item", //            6
  "- second item", //           7
  "  - nested one", //          8
  "  - nested two", //          9
  "- third item", //            10
  "", //                        11
  "> quoted line one", //       12
  "> quoted line two", //       13
  ">", //                       14
  "> - quoted item", //         15
  "", //                        16
  "| a | b |", //               17
  "| - | - |", //               18
  "| 1 | 2 |", //               19
  "| 3 | 4 |", //               20
  "", //                        21
  "```ts", //                   22
  "const a = 1;", //            23
  "const b = 2;", //            24
  "```", //                     25
  "", //                        26
  "Last paragraph.", //         27
  "",
].join("\n");

const blocks = sourceBlocks(GUIDE);
/** The block a rendered element of this type is, as the Preview draws them in order. */
const block = (type: string, nth = 0) => blocks.map((item, index) => [item.type, index] as const).filter(([kind]) => kind === type)[nth][1];
const at = (index: number, path: Boundary["path"] = [], codeLine?: number): Boundary => ({ block: index, path, ...(codeLine !== undefined && { codeLine }) });
const lines = (start: Boundary, end: Boundary, selected = "") => selectionLines(GUIDE, blocks, start, end, selected);

test("the source map has one block for each block the Preview draws", () => {
  const projection = projectMarkdown(undefined, GUIDE, false, false);
  assert.equal(blocks.length, projection.blocks.length);
  assert.deepEqual(blocks.map((item) => item.type), marked.lexer(GUIDE, { gfm: true }).map((token) => token.type));
  // A text the lexer cannot cover exactly is one block, as the Preview draws it.
  assert.equal(sourceBlocks("plain").length, 1);
});

test("headings, paragraphs and the last block keep their own lines", () => {
  assert.deepEqual(lines(at(block("heading")), at(block("heading"))), { from: 1, to: 1 });
  assert.deepEqual(lines(at(block("paragraph")), at(block("paragraph"))), { from: 3, to: 4 });
  assert.deepEqual(lines(at(block("paragraph", 1)), at(block("paragraph", 1))), { from: 27, to: 27 });
});

test("a selection inside one line of a paragraph is that line, whatever its marks", () => {
  const paragraph = at(block("paragraph"));
  assert.deepEqual(lines(paragraph, paragraph, "with emphasis"), { from: 3, to: 3 });
  assert.deepEqual(lines(paragraph, paragraph, "two lines"), { from: 4, to: 4 });
  // Text across the soft line break takes both lines; text both lines hold, or none, keeps the paragraph.
  assert.deepEqual(lines(paragraph, paragraph, "emphasis\nspans two"), { from: 3, to: 4 });
  assert.deepEqual(lines(paragraph, paragraph, "s"), { from: 3, to: 4 });
  assert.deepEqual(lines(paragraph, paragraph, "not in the file"), { from: 3, to: 4 });
});

test("list items, nested items included, have their own lines", () => {
  const list = block("list");
  assert.deepEqual(lines(at(list, [{ kind: "item", index: 0 }]), at(list, [{ kind: "item", index: 0 }])), { from: 6, to: 6 });
  // An item with nested items spans them; a nested item is its own line.
  assert.deepEqual(lines(at(list, [{ kind: "item", index: 1 }]), at(list, [{ kind: "item", index: 1 }]), "second item"), { from: 7, to: 7 });
  assert.deepEqual(
    lines(at(list, [{ kind: "item", index: 1 }, { kind: "item", index: 1 }]), at(list, [{ kind: "item", index: 1 }, { kind: "item", index: 1 }])),
    { from: 9, to: 9 },
  );
  assert.deepEqual(lines(at(list, [{ kind: "item", index: 0 }]), at(list, [{ kind: "item", index: 2 }])), { from: 6, to: 10 });
  // A path the source does not have stops at the deepest part it does.
  assert.deepEqual(lines(at(list, [{ kind: "row", index: 4 }]), at(list, [{ kind: "row", index: 4 }])), { from: 6, to: 10 });
});

test("quoted paragraphs and lists inside a quote keep their lines", () => {
  const quote = block("blockquote");
  const paragraph = [{ kind: "child" as const, index: 0 }];
  assert.deepEqual(lines(at(quote, paragraph), at(quote, paragraph)), { from: 12, to: 13 });
  assert.deepEqual(lines(at(quote, paragraph), at(quote, paragraph), "line two"), { from: 13, to: 13 });
  const item = [{ kind: "child" as const, index: 1 }, { kind: "item" as const, index: 0 }];
  assert.deepEqual(lines(at(quote, item), at(quote, item)), { from: 15, to: 15 });
});

test("a table's header comes with its delimiter row, and each body row is its line", () => {
  const table = block("table");
  assert.deepEqual(lines(at(table, [{ kind: "row", index: 0 }]), at(table, [{ kind: "row", index: 0 }])), { from: 17, to: 18 });
  assert.deepEqual(lines(at(table, [{ kind: "row", index: 2 }]), at(table, [{ kind: "row", index: 2 }])), { from: 20, to: 20 });
  assert.deepEqual(lines(at(table, [{ kind: "row", index: 1 }]), at(table, [{ kind: "row", index: 2 }])), { from: 19, to: 20 });
});

test("a line in a fenced code block is that line, never a fence", () => {
  const code = block("code");
  assert.deepEqual(lines(at(code, [], 0), at(code, [], 0)), { from: 23, to: 23 });
  assert.deepEqual(lines(at(code, [], 0), at(code, [], 1)), { from: 23, to: 24 });
  assert.deepEqual(lines(at(code, [], 5), at(code, [], 9)), { from: 24, to: 24 });
  // Without a line, the whole block, fences included.
  assert.deepEqual(lines(at(code), at(code)), { from: 22, to: 25 });
  const indented = sourceBlocks("Text\n\n    indented one\n    indented two\n");
  const index = indented.findIndex((item) => item.type === "code");
  assert.deepEqual(selectionLines("Text\n\n    indented one\n    indented two\n", indented, at(index, [], 1), at(index, [], 1), ""), { from: 4, to: 4 });
});

test("a selection across blocks runs from its first block's first line to its last block's last line", () => {
  assert.deepEqual(lines(at(block("heading")), at(block("paragraph"))), { from: 1, to: 4 });
  assert.deepEqual(lines(at(block("list"), [{ kind: "item", index: 2 }]), at(block("table"), [{ kind: "row", index: 1 }])), { from: 10, to: 19 });
  // Dragged upward, it is the same range.
  assert.deepEqual(lines(at(block("code"), [], 1), at(block("blockquote"), [{ kind: "child", index: 0 }])), { from: 12, to: 24 });
  assert.equal(lines(at(99), at(99)), undefined);
});

test("rendered text drops the line's own marks", () => {
  assert.equal(plainLine("## A *bold* [link](https://example.com) `code`"), "A bold link code");
  assert.equal(plainLine("> - [x] done"), "done");
  assert.equal(plainLine("12. twelfth"), "twelfth");
  assert.equal(plainLine("| a | b |"), "a b");
});

test("selected lines reach the chat as the same file context a code selection does", () => {
  const item = fileLinesContext("docs/guide.md", GUIDE, 6, 10);
  assert.deepEqual(item, {
    kind: "file",
    path: "docs/guide.md",
    startLine: 6,
    endLine: 10,
    content: GUIDE.split("\n").slice(5, 10).join("\n"),
    unsaved: false,
  });
  assert.equal(contextLabel(item), "docs/guide.md:6-10");
  assert.equal(contextLabel(fileLinesContext("docs/guide.md", GUIDE, 27, 27, true)), "docs/guide.md:27");
  // The editor's own selections build the same context.
  const offset = GUIDE.indexOf("- first item");
  assert.deepEqual(fileContext("docs/guide.md", GUIDE, { from: offset, to: GUIDE.indexOf("- third item") + 3 }), item);
  // Lines outside the file are clamped to it.
  assert.deepEqual(fileLinesContext("a.md", "one\ntwo", 2, 9), {
    kind: "file",
    path: "a.md",
    startLine: 2,
    endLine: 2,
    content: "two",
    unsaved: false,
  });
});
