/**
 * Seams the integration with the shared driver (packages/browser-driver)
 * depends on: one door for page operations, no driver schema handed to this
 * package's own `effect`, and a browser-free core.
 */
import assert from "node:assert/strict";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import test from "node:test";

const root = new URL("..", import.meta.url).pathname;

function files(directory: string, out: string[] = []) {
  for (const name of readdirSync(directory)) {
    const path = join(directory, name);
    if (statSync(path).isDirectory()) files(path, out);
    else if (/\.tsx?$/.test(name)) out.push(path);
  }
  return out;
}

/** Source without comments, which may name browser APIs. */
const code = (source: string) => source.replace(/\/\*[\s\S]*?\*\//g, "").replace(/^\s*\/\/.*$/gm, "");
const sources = files(join(root, "src")).map((path) => ({ path: relative(root, path), source: code(readFileSync(path, "utf8")) }));

test("no driver schema is passed to this package's own effect Schema", () => {
  for (const { path, source } of sources) assert.doesNotMatch(source, /Schema\.\w+\(\s*Browser\./, path);
});

test("page operations go through src/core/driver.ts only", () => {
  for (const { path, source } of sources) {
    if (path === "src/core/driver.ts") continue;
    assert.doesNotMatch(source, /@haarness\/browser-driver(?!\/types)/, path);
    assert.doesNotMatch(source, /import\s*\{[^}]*\bcreatePage\b[^}]*\}\s*from\s*"(?!\.\/driver")/, path);
  }
});

test("the core never touches a browser API; platform code lives under src/platform", () => {
  for (const { path, source } of sources.filter(({ path }) => path.startsWith("src/core/")))
    assert.doesNotMatch(source, /\bchrome\.|\bbrowser\.runtime\b|\bdocument\./, path);
});

test("only the service worker opens the native port", () => {
  for (const { path, source } of sources) {
    if (/connectNative/.test(source))
      assert.ok(["src/platform/chrome/index.ts", "src/core/platform.ts", "src/core/bridge.ts"].includes(path), path);
  }
});
