import test from "node:test";
import assert from "node:assert/strict";
import { readdir, readFile } from "node:fs/promises";
import { cssReferencesOutside, loadsNetworkPath, parsedAddress } from "../src/renderer/link-target";

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

// An image (or srcset candidate, SVG image, video poster) at `//host/x.png`
// resolves against the window's file: page to `file://host/x.png`, which
// Windows fetches over SMB with the user's sign-in as soon as it is drawn.
// The URL parser drops tabs and newlines anywhere and reads `\` as `/`.
test("an address a reply loads by itself never names a network path, however it is spelled", async () => {
  for (const value of [
    "//evil.example/x.png",
    "\\\\evil.example\\share\\x.png",
    "/\\evil.example/x.png",
    "\\/evil.example/x.png",
    "/\t/evil.example/x.png",
    "/\n/evil.example/x.png",
    "/\r\n/evil.example/x.png",
    "\\\t\\evil.example\\x.png",
    "\t//evil.example/x.png",
    "\x01\f //evil.example/x.png",
    "a.png 1x, /\t/evil.example/b.png 2x",
    "a.png 1x,\n\\\\evil.example\\b.png 2x",
  ])
    assert.equal(loadsNetworkPath(value), true, JSON.stringify(value));
  for (const value of [
    "x.png",
    "/x.png",
    "./images/a.png",
    "/a//b.png",
    "https://example.com/a.png",
    "data:image/png;base64,AAAA",
    "a.png 1x, b.png 2x",
  ])
    assert.equal(loadsNetworkPath(value), false, JSON.stringify(value));
  assert.equal(parsedAddress(" \tja\nva\rscript:x\x00 "), "javascript:x");

  // The sanitizer applies it to every address an element loads, and reads a
  // link the same way before deciding whether to keep it.
  const markdown = await readFile(new URL("../src/renderer/markdown-dom.ts", import.meta.url), "utf8");
  const attributes = /const URL_ATTRIBUTES = new Set\(\[([^\]]*)\]\)/.exec(markdown)?.[1] ?? "";
  for (const name of ["href", "xlink:href", "src", "srcset", "poster", "background"])
    assert.ok(attributes.includes(`"${name}"`), name);
  assert.match(markdown, /if \(loadsNetworkPath\(data\.attrValue\)\) data\.keepAttr = false;/);
  assert.match(markdown, /const value = parsedAddress\(data\.attrValue\);/);
});

// SVG presentation attributes are CSS, and their `url()` loads what it names:
// `<rect mask="url(//host/s/m.svg#m)">` reaches a network share from a file:
// window just as an image does. Only a `#fragment` of the reply itself stays.
test("an SVG attribute's url() may point only into the reply", async () => {
  for (const value of [
    "url(//evil.example/s/f.svg#f)",
    "url( '//evil.example/s/m.svg#m' )",
    'URL("\\\\\\\\evil.example\\\\s\\\\m.svg#m")',
    "url(m.svg#m)",
    "url(https://example.com/f.svg#f)",
    "url(#a) url(//evil.example/b.svg#b)",
    "\\75 rl(//evil.example/f.svg#f)",
    "u\\rl(//evil.example/f.svg#f)",
  ])
    assert.equal(cssReferencesOutside(value), true, JSON.stringify(value));
  for (const value of ["url(#clip)", "url( '#mask' )", 'url("#f") red', "red", "none", "#fff"])
    assert.equal(cssReferencesOutside(value), false, JSON.stringify(value));

  // Chromium may read `mask` as the CSS shorthand, whose images include the
  // strings of image-set() and the other image functions.
  for (const value of [
    "image-set('//evil.example/m.png' 1x)",
    '-webkit-image-set("//evil.example/m.png" 1x)',
    "IMAGE-SET(url(#a) 1x, '//evil.example/m.png' 2x)",
    "src(//evil.example/m.svg)",
    "image(//evil.example/m.png)",
    "cross-fade(url(#a), url(#b))",
    "element(#a)",
    "url(#a) '//evil.example/m.png'",
    'url("#a)',
    "\\69 mage-set('//evil.example/m.png' 1x)",
  ])
    assert.equal(cssReferencesOutside(value), true, JSON.stringify(value));
  for (const value of ['url("#grad")', "url('#m') red", "rgb(0, 0, 0)", "url(#a) none"])
    assert.equal(cssReferencesOutside(value), false, JSON.stringify(value));

  const markdown = await readFile(new URL("../src/renderer/markdown-dom.ts", import.meta.url), "utf8");
  const attributes = /const CSS_URL_ATTRIBUTES = new Set\(\[([^\]]*)\]\)/.exec(markdown)?.[1] ?? "";
  for (const name of ["fill", "stroke", "filter", "mask", "clip-path", "marker", "marker-start", "marker-mid", "marker-end", "cursor"])
    assert.ok(attributes.includes(`"${name}"`), name);
  assert.match(markdown, /CSS_URL_ATTRIBUTES\.has\(data\.attrName\) \|\| \/url\\\(\/i\.test\(data\.attrValue\)/);
  assert.match(markdown, /if \(css && cssReferencesOutside\(data\.attrValue\)\) data\.keepAttr = false;/);
});
