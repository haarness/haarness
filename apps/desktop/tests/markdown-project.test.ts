import test from "node:test";
import assert from "node:assert/strict";
import { marked } from "marked";
import { projectMarkdown, markdownPatch, markdownOptions, type MarkdownProjection } from "../src/renderer/markdown-project";

// Replies render with line breaks; files with standard paragraphs.
const parse = (text: string, breaks = true) => marked.parse(text, { async: false, ...markdownOptions(breaks) }) as string;

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
  "Replace <path> with the file, then <text>x</text> stays.\n\n[<dev@example.com>](https://example.test) and [[a](/x)](/y)\n",
  "1. <path>\n2. <file>\n\n<title> is required\n\n<file>\nline two\n\n<svg>\n<path d=\"M0\"/>\n</svg>\n\n<math><mi>x</mi></math>\n",
  "<plan>\n\n1. Do X\n2. Do Y\n\n</plan>\n\n<example>\nfoo\n\nbar\n</example>\n\nText\n\n</path>\n\nMore\n",
];

for (const [index, source] of examples.entries()) {
  for (const breaks of [true, false])
  test(`streaming Markdown matches whole-document parser at every prefix: ${index}${breaks ? "" : " (file)"}`, () => {
    let previous: MarkdownProjection | undefined;
    let html: string[] = [];
    for (let size = 1; size <= source.length; size++) {
      const text = source.slice(0, size);
      const next = projectMarkdown(previous, text, true, breaks);
      const patch = markdownPatch(previous, next);
      html.splice(patch.start, Infinity, ...patch.html);
      html.length = patch.count;
      assert.equal(html.join(""), parse(text, breaks), JSON.stringify(text));
      previous = next;
    }
    assert.equal(projectMarkdown(previous, source, false, breaks).blocks.map((block) => block.html).join(""), parse(source, breaks));
  });
}

test("a reply keeps single line breaks; a Markdown file, lists and code keep their own", () => {
  const html = (text: string, breaks = true) => projectMarkdown(undefined, text, false, breaks).blocks.map((block) => block.html).join("");
  assert.equal(html("line 1\nline 2\n\nNext"), "<p>line 1<br>line 2</p>\n<p>Next</p>\n");
  assert.equal(html("line 1\nline 2\n\nNext", false), "<p>line 1\nline 2</p>\n<p>Next</p>\n");
  assert.equal(html("- one\n- two\n"), "<ul>\n<li>one</li>\n<li>two</li>\n</ul>\n");
  assert.equal(html("```\na\nb\n```"), "<pre><code>a\nb\n</code></pre>\n");
  // A cached block never leaks between the two modes.
  assert.notEqual(html("same\ntext"), html("same\ntext", false));
});

// Read as markup, `<path>` (an SVG name outside an svg) was removed by the
// sanitizer with everything after it in its paragraph, `<title>` and `<svg>`
// hid what followed, and `<file>` vanished; the TUI shows them as written.
test("a bare tag in a reply's prose is a word of it; markup and drawings stay markup", () => {
  const html = (text: string, breaks = true) => projectMarkdown(undefined, text, false, breaks).blocks.map((block) => block.html).join("");
  assert.equal(
    html("Replace <path> with the file, then run <App />."),
    "<p>Replace &lt;path&gt; with the file, then run &lt;App /&gt;.</p>\n",
  );
  assert.equal(html("Set its <title>, use an <svg> or <file>"), "<p>Set its &lt;title&gt;, use an &lt;svg&gt; or &lt;file&gt;</p>\n");
  // A closing tag goes with the opening one shown as text, never on its own.
  assert.equal(html("<text>label</text> and </div>"), "<p>&lt;text&gt;label&lt;/text&gt; and </div></p>\n");
  assert.equal(html("- see <path>\n"), "<ul>\n<li>see &lt;path&gt;</li>\n</ul>\n");
  assert.equal(html("| a |\n| - |\n| <path> |"), "<table>\n<thead>\n<tr>\n<th>a</th>\n</tr>\n</thead>\n<tbody><tr>\n<td>&lt;path&gt;</td>\n</tr>\n</tbody></table>\n");
  assert.equal(html("Edit <path> in a file", false), "<p>Edit &lt;path&gt; in a file</p>\n");
  // Elements that show their text, tags with attributes and block HTML are markup.
  assert.equal(html("Press <kbd>K</kbd><br><img src=x> <sub>2</sub>"), "<p>Press <kbd>K</kbd><br><img src=x> <sub>2</sub></p>\n");
  assert.equal(html("| a |\n| - |\n| <ul><li>x</li></ul> |"), "<table>\n<thead>\n<tr>\n<th>a</th>\n</tr>\n</thead>\n<tbody><tr>\n<td><ul><li>x</li></ul></td>\n</tr>\n</tbody></table>\n");
  assert.equal(
    html('An icon <svg viewBox="0 0 8 8"><path d="M0 0h8v8z"/></svg> inline'),
    '<p>An icon <svg viewBox="0 0 8 8"><path d="M0 0h8v8z"/></svg> inline</p>\n',
  );
  assert.equal(html("<details>\n<summary>More</summary>\n\nbody\n</details>"), "<details>\n<summary>More</summary>\n\n<p>body</p>\n</details>");
  assert.equal(html("`<path>` and\n\n```\n<path>\n```"), "<p><code>&lt;path&gt;</code> and</p>\n<pre><code>&lt;path&gt;\n</code></pre>\n");
});

// A bare tag alone on a line, or starting one with a block element's name,
// begins a block of HTML; the sanitizer dropped it as it did inline.
test("a block that starts with a bare tag is a paragraph of the reply; block HTML stays markup", () => {
  const html = (text: string, breaks = true) => projectMarkdown(undefined, text, false, breaks).blocks.map((block) => block.html).join("");
  assert.equal(html("1. <path>\n2. <file>"), "<ol>\n<li>&lt;path&gt;</li>\n<li>&lt;file&gt;</li>\n</ol>\n");
  assert.equal(html("Options:\n\n<path>\n\n<file>\n"), "<p>Options:</p>\n<p>&lt;path&gt;</p>\n<p>&lt;file&gt;</p>\n");
  assert.equal(html("<file>\nline two"), "<p>&lt;file&gt;<br>line two</p>\n");
  assert.equal(html("<file>\nline two", false), "<p>&lt;file&gt;\nline two</p>\n");
  assert.equal(html("<title> is required, in <head>"), "<p>&lt;title&gt; is required, in &lt;head&gt;</p>\n");
  for (const name of ["head", "body", "form", "option", "iframe", "dialog", "menu"])
    assert.equal(html(`<${name}> holds it`), `<p>&lt;${name}&gt; holds it</p>\n`, name);
  // Real block HTML: an element showing its text, attributes, a comment.
  assert.equal(html("<details>\n<summary>More</summary>\n\nbody\n</details>"), "<details>\n<summary>More</summary>\n\n<p>body</p>\n</details>");
  assert.equal(html('<div class="note">\n<path>\n</div>'), '<div class="note">\n<path>\n</div>');
  assert.equal(html("<table>\n<tr><td>x</td></tr>\n</table>"), "<table>\n<tr><td>x</td></tr>\n</table>");
  assert.equal(html("<!-- note -->\ntext"), "<!-- note -->\n<p>text</p>\n");
  assert.equal(html('<img src="a.png" alt="a">'), '<img src="a.png" alt="a">');
  // A closing tag starting its own block is a word too, so a section a reply
  // wraps in made-up tags shows both, and no empty paragraph is left.
  assert.equal(
    html("<plan>\n\n1. Do X\n2. Do Y\n\n</plan>"),
    "<p>&lt;plan&gt;</p>\n<ol>\n<li>Do X</li>\n<li>Do Y</li>\n</ol>\n<p>&lt;/plan&gt;</p>\n",
  );
  assert.equal(html("<example>\nfoo\n\n</example>"), "<p>&lt;example&gt;<br>foo</p>\n<p>&lt;/example&gt;</p>\n");
  assert.equal(html("Text\n\n</path>\n\nMore"), "<p>Text</p>\n<p>&lt;/path&gt;</p>\n<p>More</p>\n");
  // A drawing's closing tag in a block of its own stays markup.
  assert.equal(html('<svg width="8">\n<path d="M0"/>\n\n</svg>'), '<svg width="8">\n<path d="M0"/>\n\n</svg>');
});

// Written without attributes around elements of their own, an svg or a
// formula is a drawing, as it was; around words only, it is a word.
test("a bare svg or math wrapping elements still draws", () => {
  const html = (text: string) => projectMarkdown(undefined, text, false).blocks.map((block) => block.html).join("");
  assert.equal(html('An icon <svg><path d="M0 0h8v8z"/></svg> here'), '<p>An icon <svg><path d="M0 0h8v8z"/></svg> here</p>\n');
  assert.equal(html("x is <math><mi>x</mi><mo>+</mo><mn>1</mn></math>"), "<p>x is <math><mi>x</mi><mo>+</mo><mn>1</mn></math></p>\n");
  assert.equal(html('<svg>\n<path d="M0 0h8v8z"/>\n</svg>'), '<svg>\n<path d="M0 0h8v8z"/>\n</svg>');
  assert.equal(html("Use an <svg> here, then </svg>"), "<p>Use an &lt;svg&gt; here, then &lt;/svg&gt;</p>\n");
  assert.equal(html("An <svg> holds a <path>"), "<p>An &lt;svg&gt; holds a &lt;path&gt;</p>\n");
  assert.equal(html("<svg>\nwords only\n</svg>"), "<p>&lt;svg&gt;<br>words only<br>&lt;/svg&gt;</p>\n");
});

// An anchor inside an anchor is split by the HTML parser: the label went to
// the inner address and an empty link to the label's own address was left.
test("a link inside a link's label keeps only its text, so the label goes where its link points", () => {
  const html = (text: string) => projectMarkdown(undefined, text, false).blocks.map((block) => block.html).join("");
  assert.equal(html("[<dev@example.com>](https://example.com)"), '<p><a href="https://example.com">dev@example.com</a></p>\n');
  assert.equal(html("[[inner](https://a.example)](https://example.com)"), '<p><a href="https://example.com">inner</a></p>\n');
  assert.equal(
    html("[**<dev@example.com>** ![logo](x.png)](https://example.com)"),
    '<p><a href="https://example.com"><strong>dev@example.com</strong> <img src="x.png" alt="logo"></a></p>\n',
  );
  // Links outside a label are untouched.
  assert.equal(html("<dev@example.com> and [a](/b)"), '<p><a href="mailto:dev@example.com">dev@example.com</a> and <a href="/b">a</a></p>\n');
});

test("append keeps completed blocks and lexes only the last block; replacement resets", () => {
  const text = Array.from({ length: 1000 }, (_, i) => `Paragraph ${i}.\n\n`).join("") + "Tail";
  const previous = projectMarkdown(undefined, text, true);
  const next = projectMarkdown(previous, text + " grows", true);
  assert.ok(previous.offset > text.length - 30);
  assert.equal(next.blocks[0], previous.blocks[0]);
  assert.ok(markdownPatch(previous, next).start >= 1999);
  const replaced = projectMarkdown(next, "# Different session", true);
  assert.equal(replaced.blocks.map((block) => block.html).join(""), parse("# Different session"));
  assert.equal(markdownPatch(next, replaced).start, 0);
  assert.deepEqual(projectMarkdown(replaced, "", false).blocks, []);
});

test("later reference definitions invalidate completed blocks", () => {
  const previous = projectMarkdown(undefined, "[one][target]\n\nTail", true);
  const text = previous.text + "\n\n[target]: /resolved\n";
  const next = projectMarkdown(previous, text, true);
  assert.equal(markdownPatch(previous, next).start, 0);
  assert.equal(next.blocks.map((block) => block.html).join(""), parse(text));
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
      assert.equal(previous.blocks.map(block => block.html).join(""), parse(text), JSON.stringify(text));
    }
  }
});
