import test from "node:test";
import assert from "node:assert/strict";
import { readdir, readFile } from "node:fs/promises";
import { themeColors, themeIds } from "../src/themes";

// A `var(--x)` with no fallback whose `--x` nothing defines makes the whole
// declaration invalid: `border: 1px solid var(--border)` drew no border at
// all, and `font: 12px/1.4 var(--font)` lost its size too. Every name used
// must be defined by a stylesheet, a palette (themes.ts, written onto the
// page at runtime) or a style the renderer sets itself.

const renderer = new URL("../src/renderer/", import.meta.url);

/** The renderer's stylesheets and sources, the third-party files it ships aside. */
async function sources(directory = renderer): Promise<{ name: string; text: string }[]> {
  const found: { name: string; text: string }[] = [];
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const url = new URL(entry.name + (entry.isDirectory() ? "/" : ""), directory);
    if (entry.isDirectory()) {
      if (entry.name !== "third-party") found.push(...(await sources(url)));
    } else if (/\.(css|tsx?|html)$/.test(entry.name))
      found.push({ name: url.pathname.slice(renderer.pathname.length), text: await readFile(url, "utf8") });
  }
  return found;
}

/** `text` without its comments, which may name a property without using it. */
function uncommented(name: string, text: string) {
  const blocks = text.replace(/\/\*[\s\S]*?\*\//g, "");
  return name.endsWith(".css") ? blocks : blocks.replace(/^\s*\/\/.*$/gm, "");
}

test("every CSS variable the renderer uses without a fallback is defined", async () => {
  const files = await sources();
  const defined = new Set<string>();
  for (const id of themeIds)
    for (const appearance of ["light", "dark"] as const)
      for (const key of Object.keys(themeColors(id, appearance))) defined.add("--" + key);
  for (const { name, text } of files) {
    const code = uncommented(name, text);
    // A declaration in a stylesheet or a style string (`--x: …`), a style
    // object's key (`"--x": …`) and a property the renderer sets.
    for (const match of code.matchAll(/(?:^|[\s{;"'`(,])(--[\w-]+)["'`]?\s*:/g)) defined.add(match[1]);
    for (const match of code.matchAll(/setProperty\(\s*["'`](--[\w-]+)["'`]/g)) defined.add(match[1]);
  }
  const missing: string[] = [];
  for (const { name, text } of files)
    for (const match of uncommented(name, text).matchAll(/var\(\s*(--[\w-]+)\s*([,)])/g))
      if (match[2] === ")" && !defined.has(match[1])) missing.push(`${name}: ${match[1]}`);
  assert.deepEqual([...new Set(missing)], []);
  // The scan sees the definitions it relies on.
  for (const name of ["--hairline", "--sans", "--text", "--diff-add", "--toast-clearance"]) assert.ok(defined.has(name), name);
});
