import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

// A touch screen cannot hover, so what appears on hover has to show at rest
// there: a code block's Copy stayed hidden on a tablet.
test("on a touch screen a code block shows Copy at rest", async () => {
  const css = await readFile(new URL("../src/renderer/chat.css", import.meta.url), "utf8");
  const start = css.indexOf("@media (hover: none) {");
  assert.ok(start >= 0);
  let depth = 0;
  let end = start;
  for (let index = css.indexOf("{", start); index < css.length; index++) {
    if (css[index] === "{") depth++;
    if (css[index] === "}" && --depth === 0) {
      end = index;
      break;
    }
  }
  const block = css.slice(css.indexOf("{", start) + 1, end);
  const rule = [...block.matchAll(/([^{}]+)\{([^{}]*)\}/g)].find(([, selector]) =>
    /\.code-copy\s*$/.test(selector.trim()),
  );
  assert.ok(rule, "no .code-copy rule for touch screens");
  assert.match(rule[2], /opacity:\s*1;/);
  // The hidden rule comes later in the file, so only a more specific
  // selector than `.code-copy` alone wins over it.
  const parts = rule[1].replace(/\/\*[^]*?\*\//g, "").trim().split(/\s+/);
  assert.ok(parts.length > 1, rule[1]);
});
