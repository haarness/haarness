import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { SELECTION_SHARE, terminalTheme, themes, themeColors } from "../src/themes";

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

test("text selected in the terminal is marked as selected text is everywhere else, and stands out from the page", () => {
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
      assert.ok(contrast(colors.foreground, selected) >= 4.5, `${name}: text on the selection`);
    }
});
