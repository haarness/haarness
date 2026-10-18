import test from "node:test";
import assert from "node:assert/strict";
import { readdir, readFile } from "node:fs/promises";

// Markdown sanitizing needs a DOM, which these tests do not have, so this
// reads the source: replies are sanitized by a DOMPurify of their own, and
// nothing configures the shared one, whose hooks a library such as Mermaid
// extends (Upstream PR #1082).
test("markdown is sanitized by a DOMPurify instance of its own", async () => {
  const directory = new URL("../src/renderer/", import.meta.url);
  const markdown = await readFile(new URL("markdown-dom.ts", directory), "utf8");
  assert.match(markdown, /const purifier = DOMPurify\(window\);/);
  assert.match(markdown, /purifier\.addHook\(/);
  assert.match(markdown, /purifier\.sanitize\(/);
  for (const name of await readdir(directory)) {
    if (!/\.tsx?$/.test(name)) continue;
    const source = await readFile(new URL(name, directory), "utf8");
    assert.doesNotMatch(source, /DOMPurify\.(addHook|sanitize|setConfig|removeHook)\(/, name);
  }
});
