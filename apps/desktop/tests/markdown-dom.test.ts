import test from "node:test";
import assert from "node:assert/strict";
import { readdir, readFile } from "node:fs/promises";
import { cssReferencesOutside, loadsNetworkPath, parsedAddress, sanitizedLink } from "../src/renderer/link-target";
import { REPLY_ID, replyAttribute, replyIds } from "../src/renderer/remote-images";

// Markdown sanitizing needs a DOM, which these tests do not have, so this
// reads the source: replies are sanitized by a DOMPurify of their own, and
// nothing configures the shared one, whose hooks a library such as Mermaid
// extends.
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

// DOMPurify 3.4.17 fixes GHSA-c6gr-qvv4-5v3f (an `attributeName` rewriting a
// link in XHTML mode), GHSA-cg2g-xh64-v4q7 (template expressions in shadow
// roots) and GHSA-mrv2-3pqr-j5jr (in-place hooks that detach another node).
// Markdown's sanitizer uses none of those modes; the app ships the fixed
// release anyway, so no later use of them can bring the bugs back.
test("the app ships a DOMPurify with the October 2026 advisories fixed", async () => {
  const { default: DOMPurify } = await import("dompurify");
  const at = (version: string) => version.split(".").map(Number);
  const [major, minor, patch] = at(DOMPurify.version);
  assert.ok(major > 3 || (major === 3 && (minor > 4 || (minor === 4 && patch >= 17))), DOMPurify.version);
  const lock = JSON.parse(await readFile(new URL("../package-lock.json", import.meta.url), "utf8"));
  assert.equal(lock.packages["node_modules/dompurify"].version, DOMPurify.version);
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

// `[README.md:3](README.md:3)` is a link to a file at the project's top, but
// `readme.md:` reads as a scheme: the sanitizer made it a placeholder, so the
// link said it does not open. Its address now stays as written.
test("a reply's link to a top-level file with its line keeps its address as written", async () => {
  const markdown = await readFile(new URL("../src/renderer/markdown-dom.ts", import.meta.url), "utf8");
  assert.match(markdown, /const rule = sanitizedLink\(value\);\n\s*if \(rule === "keep"\) data\.forceKeepAttr = true;/);
  assert.match(markdown, /else if \(rule === "placeholder"\) \{\n[^}]*setAttribute\("href", "unsupported:"\)/);
  for (const href of ["README.md:3", " main.rs:42 ", "a.ts:12-20", "a.ts:12:4"])
    assert.equal(sanitizedLink(parsedAddress(href)), "keep", href);
  for (const href of ["tel:123", "vscode://file/a.ts", "x-acme.settings:privacy"])
    assert.equal(sanitizedLink(parsedAddress(href)), "placeholder", href);
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

// A class is the app's own vocabulary. Kept, a reply's
// `<div class="modal-backdrop">` drew the app's blurred overlay over the chat
// while the reply streamed, so Stop could not be clicked, and a README with it
// did the same in the Files panel's preview.
test("a reply keeps no class but the language Markdown writes on a code block's code", () => {
  for (const [element, value] of [
    ["DIV", "modal-backdrop"],
    ["DIV", "dialog-shell palette"],
    ["SPAN", "spelled-code"],
    ["SPAN", "code-copy"],
    ["PRE", "language-ts"],
    ["LI", "task-item"],
    ["rect", "icon"],
    ["CODE", "modal-backdrop"],
    ["CODE", "languages"],
    ["CODE", "language-"],
    ["CODE", ""],
  ])
    assert.equal(replyAttribute("class", value, element), undefined, `${element} ${value}`);
  assert.equal(replyAttribute("class", "language-ts", "CODE"), "language-ts");
  assert.equal(replyAttribute("class", "language-c++", "CODE"), "language-c++");
  assert.equal(replyAttribute("class", " modal-backdrop language-rust  open ", "CODE"), "language-rust");
  assert.equal(replyAttribute("CLASS", "language-py overlay", "code"), "language-py");
  // Other attributes stay as written.
  assert.equal(replyAttribute("title", "modal-backdrop", "DIV"), "modal-backdrop");
});

// The app finds parts of its own by id (`slash-command-1` in the message
// box's command menu), and a reply drawn before them would be found instead.
test("a reply's ids, and its references to them, are its own", () => {
  assert.equal(REPLY_ID, "reply-");
  assert.equal(replyIds("id", "slash-command-1", false), "reply-slash-command-1");
  assert.equal(replyIds("ID", "shade", true), "reply-shade");
  assert.equal(replyIds("aria-labelledby", " a  b ", false), "reply-a reply-b");
  assert.equal(replyIds("for", "name", false), "reply-name");
  assert.equal(replyIds("headers", "h1 h2", false), "reply-h1 reply-h2");
  assert.equal(replyIds("fill", "url(#shade)", true), "url(#reply-shade)");
  assert.equal(replyIds("mask", "url( '#m' ) url(\"#n\")", true), "url( '#reply-m' ) url(\"#reply-n\")");
  assert.equal(replyIds("href", "#shape", true), "#reply-shape");
  assert.equal(replyIds("xlink:href", " #shape", true), "#reply-shape");
  // What names no id of the reply stays as written.
  for (const [name, value, svg] of [
    ["href", "#section", false],
    ["href", "https://example.test/#x", true],
    ["fill", "teal", true],
    ["title", "slash-command-1", false],
    ["id", "", false],
  ] as const)
    assert.equal(replyIds(name, value, svg), value, `${name}=${value}`);
});
