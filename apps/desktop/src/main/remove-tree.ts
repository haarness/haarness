import { rm } from "node:fs/promises";
import { createRequire } from "node:module";

/**
 * Electron's own `fs` reads an `.asar` archive as a folder, so it cannot
 * delete one: removing a copy of Haarness.app stopped at its
 * `Contents/Resources/app.asar` (ENOTEMPTY) and left that file, a quarter of
 * a gigabyte, behind for good. Electron's `original-fs` is the file system
 * without archives; outside Electron (tests) Node's own already is.
 */
const plainRm: typeof rm = (() => {
  try {
    return (createRequire(import.meta.url)("original-fs") as typeof import("node:fs")).promises.rm;
  } catch {
    return rm;
  }
})();

/** Removes a file or a folder with everything in it, app bundles included;
 * nothing there is no failure. */
export function removeTree(path: string) {
  return plainRm(path, { recursive: true, force: true });
}
