/**
 * Seams the integration with the shared driver (packages/browser-driver)
 * depends on: one door for page operations, no driver schema handed to this
 * package's own `effect`, one pinned effect and DevTools protocol, and a
 * browser-free core.
 */
import assert from "node:assert/strict";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("..", import.meta.url));
const driver = fileURLToPath(new URL("../../../packages/browser-driver/", import.meta.url));

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
// Paths as the tests name them, with forward slashes on Windows too.
const sources = files(join(root, "src")).map((path) => ({ path: relative(root, path).replaceAll("\\", "/"), source: code(readFileSync(path, "utf8")) }));

// Under tsx the driver's schemas come from the driver's own copy of effect,
// which this package's effect cannot decode with (it fails at runtime, not in
// tsc), so the extension calls the driver's codecs instead.
test("no driver schema is passed to this package's own effect Schema", () => {
  const offenders: string[] = [];
  for (const directory of ["src", "tests", "e2e", "build"])
    for (const path of files(join(root, directory))) {
      const source = readFileSync(path, "utf8");
      for (const match of source.matchAll(/Schema\.\w+\(\s*Browser\./g))
        offenders.push(`${relative(root, path)}:${source.slice(0, match.index).split("\n").length}`);
    }
  assert.deepEqual(offenders, []);
});

test("page operations go through src/core/driver.ts only", () => {
  for (const { path, source } of sources) {
    if (path === "src/core/driver.ts") continue;
    assert.doesNotMatch(source, /@haarness\/browser-driver\/page\b/, path);
    assert.doesNotMatch(source, /import\s*\{[^}]*\bcreatePage\b[^}]*\}\s*from\s*"(?!\.\/driver")/, path);
  }
});

// One effect version everywhere keeps the bundle on a single copy (vite's
// resolve.dedupe) and the driver's codecs on the API this package compiles against.
test("the extension and the browser driver pin the same effect and DevTools protocol", () => {
  const json = (path: string) => JSON.parse(readFileSync(path, "utf8"));
  const app = json(join(root, "package.json"));
  const appLock = json(join(root, "package-lock.json"));
  const pkg = json(join(driver, "package.json"));
  const lock = json(join(driver, "package-lock.json"));
  assert.equal(app.dependencies["@haarness/browser-driver"], "file:../../packages/browser-driver");
  for (const name of ["effect", "devtools-protocol"]) {
    const version = app.dependencies[name] ?? app.devDependencies[name];
    assert.match(version, /^\d+\.\d+\.\d+(?:-[\w.]+)?$/, `${name} is pinned exactly`);
    assert.equal(pkg.peerDependencies[name], version, `driver peer ${name}`);
    assert.equal(pkg.devDependencies[name], version, `driver dev ${name}`);
    assert.equal(appLock.packages[`node_modules/${name}`].version, version, `extension lock ${name}`);
    assert.equal(lock.packages[`node_modules/${name}`].version, version, `driver lock ${name}`);
  }
  const vite = readFileSync(join(root, "vite.config.ts"), "utf8");
  assert.match(vite, /dedupe:\s*\[[^\]]*"effect"/);
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
