import test from "node:test";
import assert from "node:assert/strict";
import { readdir, readFile } from "node:fs/promises";

// The mono stack ends in the generic `monospace`, which on Linux is whatever
// the system names: Fira Code or JetBrains Mono there joined `!=` and `->`
// into one glyph in paths, diffs, ids and the editor. Every rule that sets
// the mono face turns the font's ligatures off with it.
test("monospace text has no ligatures, wherever the mono face is set", async () => {
  const directory = new URL("../src/renderer/", import.meta.url);
  const missing: string[] = [];
  for (const name of await readdir(directory)) {
    if (!name.endsWith(".css")) continue;
    const css = await readFile(new URL(name, directory), "utf8");
    for (const [, selector, body] of css.matchAll(/([^{}]+)\{([^{}]*)\}/g)) {
      const mono = /(?:^|[;\s])font(?:-family)?:[^;]*var\(--mono\)/.test(body);
      // A later `font` shorthand would reset the ligatures again.
      if (mono && !/font-variant-ligatures:\s*none;(?![^]*\bfont:)/.test(body))
        missing.push(`${name}: ${selector.trim().split("\n").at(-1)}`);
    }
  }
  assert.deepEqual(missing, []);
  const editor = await readFile(new URL("Editor.tsx", directory), "utf8");
  assert.match(editor, /"\.cm-scroller": \{[^}]*fontVariantLigatures: "none"/);
  const shell = await readFile(new URL("shell.css", directory), "utf8");
  assert.match(shell, /\.terminal-view \.xterm \{[^}]*font-variant-ligatures: none;/);
});
