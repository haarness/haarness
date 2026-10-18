import test from "node:test";
import assert from "node:assert/strict";
import { readdir, readFile } from "node:fs/promises";

// The terminal panel's states (opening, empty, reconnecting, cut off) are
// rare enough that a lost rule goes unseen: every class the panel names has
// a rule in the renderer's stylesheets.
test("every class the terminal panel uses is styled", async () => {
  const directory = new URL("../src/renderer/", import.meta.url);
  const source = await readFile(new URL("Terminal.tsx", directory), "utf8");
  let css = "";
  for (const name of await readdir(directory))
    if (name.endsWith(".css")) css += await readFile(new URL(name, directory), "utf8");
  const classes = new Set(
    [...source.matchAll(/\bclass="([^"]+)"/g)].flatMap(([, names]) => names.split(/\s+/)),
  );
  assert.ok(classes.has("terminal-empty"));
  const unstyled = [...classes].filter(
    (name) => !new RegExp(`\\.${name.replace(/[-]/g, "\\-")}(?![\\w-])`).test(css),
  );
  assert.deepEqual(unstyled, []);
});
