import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { SELECTION_SHARE, terminalTheme, themes, themeColors } from "../src/themes";
import { terminalOptions } from "../src/renderer/terminal-options";

function luminance(hex: string) {
  const channels = [1, 3, 5].map(
    (offset) => parseInt(hex.slice(offset, offset + 2), 16) / 255,
  );
  const linear = channels.map((value) =>
    value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4,
  );
  return linear[0] * 0.2126 + linear[1] * 0.7152 + linear[2] * 0.0722;
}
test("every desktop palette preserves readable text in light and dark modes", () => {
  for (const theme of themes)
    for (const mode of ["light", "dark"] as const) {
      const colors = themeColors(theme.id, mode);
      // "raised" carries popovers, menus, chips and the jump-to-latest pill,
      // so secondary text has to stay readable on it too.
      for (const surface of ["page", "sidebar", "surface", "raised"])
        for (const text of ["text", "muted", "primary"]) {
          const a = luminance(colors[surface]),
            b = luminance(colors[text]);
          const contrast = (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
          assert.ok(
            contrast >= 4.5,
            `${theme.name} ${mode}: ${text} on ${surface} has contrast ${contrast.toFixed(2)}`,
          );
        }
    }
});

test("terminal black is dark and white light in every palette, so black text stays readable on a light page", () => {
  const contrast = (x: string, y: string) => {
    const a = luminance(x),
      b = luminance(y);
    return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
  };
  for (const theme of themes)
    for (const mode of ["light", "dark"] as const) {
      const colors = terminalTheme(theme.id, mode);
      const name = `${theme.name} ${mode}`;
      assert.ok(luminance(colors.black) < luminance(colors.white), `${name}: black is lighter than white`);
      // Black on white: a status line, a badge.
      assert.ok(contrast(colors.black, colors.white) >= 4.5, `${name}: black on white`);
      // The colour a program picks for text on this background.
      const text = mode === "light" ? colors.black : colors.white;
      assert.ok(contrast(text, colors.background) >= 4.5, `${name}: ${mode === "light" ? "black" : "white"} text on the page`);
    }
});

/** The sixteen colours a program picks from. */
const ansi = ["black", "red", "green", "yellow", "blue", "magenta", "cyan", "white"].flatMap((name) => [
  name,
  "bright" + name[0].toUpperCase() + name.slice(1),
]);

test("text selected in the terminal is marked with the app's selection colour, stands out from the page, and reads in the text colour", () => {
  // The app's selection: the primary colour mixed at this share.
  const css = readFileSync(new URL("../src/renderer/base.css", import.meta.url), "utf8");
  assert.match(
    css,
    new RegExp(`::selection \\{\\s*background: color-mix\\(in srgb, var\\(--primary\\) ${Math.round(SELECTION_SHARE * 100)}%, transparent\\);`),
  );
  const contrast = (x: string, y: string) => {
    const a = luminance(x),
      b = luminance(y);
    return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
  };
  // xterm lays the selection over the page.
  const over = (page: string, color: string) => {
    const alpha = parseInt(color.slice(7, 9), 16) / 255;
    return (
      "#" +
      [1, 3, 5]
        .map((offset) =>
          Math.round(
            parseInt(page.slice(offset, offset + 2), 16) * (1 - alpha) + parseInt(color.slice(offset, offset + 2), 16) * alpha,
          )
            .toString(16)
            .padStart(2, "0"),
        )
        .join("")
    );
  };
  for (const theme of themes)
    for (const mode of ["light", "dark"] as const) {
      const colors = terminalTheme(theme.id, mode);
      const name = `${theme.name} ${mode}`;
      assert.equal(colors.selectionBackground, themeColors(theme.id, mode).primary + "4d", name);
      const selected = over(colors.background, colors.selectionBackground);
      // It was about 1.2:1, hard to find on the page.
      assert.ok(contrast(selected, colors.background) >= 1.5, `${name}: selection on the page`);
      // xterm draws selected text in this colour whatever colour a program
      // gave it: red, green or muted output would be faint on the selection.
      assert.equal(colors.selectionForeground, colors.foreground, name);
      assert.ok(contrast(colors.selectionForeground, selected) >= 4.5, `${name}: text on the selection`);
    }
});

test("the terminal's contrast floor lifts only the colours that would vanish on the page, and cyan is a teal of its own", () => {
  // xterm makes text under this contrast with what is behind it lighter or
  // darker until it has that much.
  const floor = terminalOptions(13).minimumContrastRatio!;
  assert.equal(floor, 3);
  const contrast = (x: string, y: string) => {
    const a = luminance(x),
      b = luminance(y);
    return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
  };
  for (const theme of themes)
    for (const mode of ["light", "dark"] as const) {
      const colors = terminalTheme(theme.id, mode);
      const name = `${theme.name} ${mode}`;
      const drawn: Record<string, string> = colors;
      // Black on the dark page and white on the light one; the palettes'
      // other colours are drawn as they are.
      assert.deepEqual(
        ["foreground", ...ansi].filter((role) => contrast(drawn[role], colors.background) < floor),
        mode === "dark" ? ["black"] : ["white", "brightWhite"],
        name,
      );
      // The character under the block cursor.
      assert.equal(colors.cursorAccent, colors.background, name);
      assert.ok(contrast(colors.cursorAccent, colors.cursor) >= 4.5, `${name}: the character under the cursor`);
      assert.equal(colors.brightCyan, colors.cyan, name);
      for (const other of ["foreground", "green", "blue"] as const)
        assert.notEqual(colors.cyan, colors[other], `${name}: cyan is ${other}`);
      assert.ok(contrast(colors.cyan, colors.background) >= 4.5, `${name}: cyan on the page`);
    }
});
