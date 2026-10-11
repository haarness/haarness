import test from "node:test";
import assert from "node:assert/strict";
import { readdir, readFile } from "node:fs/promises";

const desktop = new URL("../", import.meta.url);
const driver = new URL("../../../packages/browser-driver/", import.meta.url);

async function sources(directory: URL): Promise<URL[]> {
  const found: URL[] = [];
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    if (entry.name === "node_modules" || entry.name.startsWith(".")) continue;
    const url = new URL(entry.name + (entry.isDirectory() ? "/" : ""), directory);
    if (entry.isDirectory()) found.push(...(await sources(url)));
    else if (/\.(?:[cm]?ts|tsx)$/.test(entry.name)) found.push(url);
  }
  return found;
}

// Under tsx the driver's schemas come from its own copy of effect. Desktop's
// effect cannot decode with them (it fails at runtime, not in tsc), so Desktop
// uses the driver's codecs (decodeCommand, encodeOutcome, …) instead.
test("Desktop never passes a browser driver schema to its own effect", async () => {
  const offenders: string[] = [];
  for (const directory of ["src/", "tests/", "e2e/", "scripts/"])
    for (const file of await sources(new URL(directory, desktop))) {
      const source = await readFile(file, "utf8");
      for (const match of source.matchAll(/Schema\.\w+\(\s*Browser\./g))
        offenders.push(
          `${file.pathname.slice(desktop.pathname.length)}:${source.slice(0, match.index).split("\n").length}`,
        );
    }
  assert.deepEqual(offenders, []);
});

// One effect version everywhere keeps the bundled main process on a single copy
// and the driver's codecs on the API Desktop compiles against.
test("Desktop and the browser driver pin the same effect and DevTools protocol", async () => {
  const json = async (url: URL) => JSON.parse(await readFile(url, "utf8"));
  const app = await json(new URL("package.json", desktop));
  const appLock = await json(new URL("package-lock.json", desktop));
  const pkg = await json(new URL("package.json", driver));
  const lock = await json(new URL("package-lock.json", driver));
  assert.equal(app.devDependencies["@haarness/browser-driver"], "file:../../packages/browser-driver");
  assert.equal(app.dependencies["@haarness/browser-driver"], undefined);
  for (const name of ["effect", "devtools-protocol"]) {
    const version = app.dependencies[name];
    assert.match(version, /^\d+\.\d+\.\d+(?:-[\w.]+)?$/, `${name} is pinned exactly`);
    assert.equal(pkg.peerDependencies[name], version, `driver peer ${name}`);
    assert.equal(pkg.devDependencies[name], version, `driver dev ${name}`);
    assert.equal(appLock.packages[`node_modules/${name}`].version, version, `Desktop lock ${name}`);
    assert.equal(lock.packages[`node_modules/${name}`].version, version, `driver lock ${name}`);
  }
});
