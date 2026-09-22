import test from "node:test";
import assert from "node:assert/strict";
import { marked } from "marked";
import { projectMarkdown, markdownPatch, type MarkdownProjection } from "../src/renderer/markdown-project";

const examples = [
  "# Heading\n\nParagraph with **bold**, _emphasis_ and `code`.\n\nNext paragraph.",
  "A setext heading\n===\n\nAnother\n---\n",
  "| left | right |\n| --- | ---: |\n| one | two |\n| three | four |\n\nDone.",
  "- first\n\n- second\n  - nested\n\n    continuation\n\nAfter.\n",
  "1. first\n2. second\n\n   continued\n\n3. third\n",
  "```typescript\nconst a = '<unsafe>';\n\nconst b = 2;\n```\n\nAfter.",
  "~~~text\none\n```\ntwo\n~~~\n",
  "> quote\n> continued\n>\n> - nested\n\n> next\n\nAfter.",
  "[reference][target]\n\nAnother paragraph.\n\n[target]: https://example.test \"A title\"\n",
  "[target]\n\n> [target]: /quote\n\n[target]\n",
  "<div>\nraw **HTML**\n\n</div>\n\nAfter.",
  "<script>alert('unsafe')</script>\n\n[bad](javascript:alert(1))\n",
  "    indented\n\n    more\n\nAfter.\n",
  "one\r\n\r\ntwo\r\n\r\nthree\r",
  "one  \nline break\n\n---\n\nDone 😊 日本語 العربية\n",
  "- [x] checked\n- [ ] open\n\nhttps://example.test\n",
];

for (const [index, source] of examples.entries()) {
  test(`streaming Markdown matches whole-document parser at every prefix: ${index}`, () => {
    let previous: MarkdownProjection | undefined;
    let html: string[] = [];
    for (let size = 1; size <= source.length; size++) {
      const text = source.slice(0, size);
      const next = projectMarkdown(previous, text, true);
      const patch = markdownPatch(previous, next);
      html.splice(patch.start, Infinity, ...patch.html);
      html.length = patch.count;
      assert.equal(html.join(""), marked.parse(text, { gfm: true }), JSON.stringify(text));
      previous = next;
    }
    assert.equal(projectMarkdown(previous, source, false).blocks.map((block) => block.html).join(""), marked.parse(source));
  });
}

test("append keeps completed blocks and lexes only the last block; replacement resets", () => {
  const text = Array.from({ length: 1000 }, (_, i) => `Paragraph ${i}.\n\n`).join("") + "Tail";
  const previous = projectMarkdown(undefined, text, true);
  const next = projectMarkdown(previous, text + " grows", true);
  assert.ok(previous.offset > text.length - 30);
  assert.equal(next.blocks[0], previous.blocks[0]);
  assert.ok(markdownPatch(previous, next).start >= 1999);
  const replaced = projectMarkdown(next, "# Different session", true);
  assert.equal(replaced.blocks.map((block) => block.html).join(""), marked.parse("# Different session"));
  assert.equal(markdownPatch(next, replaced).start, 0);
  assert.deepEqual(projectMarkdown(replaced, "", false).blocks, []);
});

test("later reference definitions invalidate completed blocks", () => {
  const previous = projectMarkdown(undefined, "[one][target]\n\nTail", true);
  const text = previous.text + "\n\n[target]: /resolved\n";
  const next = projectMarkdown(previous, text, true);
  assert.equal(markdownPatch(previous, next).start, 0);
  assert.equal(next.blocks.map((block) => block.html).join(""), marked.parse(text));
});

test("mixed Markdown block boundaries agree with whole-document parsing throughout streaming", () => {
  const parts = ["a\n", "\n", "- one\n", "1. one\n", "2. two\n", "  continued\n", "> quote\n", "---\n", "```\n", "~~~\n", "| a | b |\n", "| - | - |\n", "[r]: /foo\n", "[r]\n", "<div>\n", "</div>\n"];
  let seed = 91;
  for (let run = 0; run < 100; run++) {
    let source = "";
    for (let index = 0; index < 12; index++) {
      seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0;
      source += parts[seed % parts.length];
    }
    let previous: MarkdownProjection | undefined;
    for (let size = 1; size <= source.length; size++) {
      const text = source.slice(0, size);
      previous = projectMarkdown(previous, text, true);
      assert.equal(previous.blocks.map(block => block.html).join(""), marked.parse(text), JSON.stringify(text));
    }
  }
});
