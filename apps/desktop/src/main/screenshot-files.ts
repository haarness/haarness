import { constants, type BigIntStats } from "node:fs";
import { lstat, open, readdir } from "node:fs/promises";
import { basename, extname, join } from "node:path";
import { SCREENSHOTS_CHANGED, SCREENSHOT_TYPES, imageName, newestImages } from "../screenshots";

/** What one message's attachments may total, as the server and the SDK's attachment reader allow. */
const ATTACHMENT_BYTES = 20 * 1024 * 1024;

/** Which file a listed screenshot was: a pick must still be that file when it is read. */
export type FileIdentity = { dev: bigint; ino: bigint; size: bigint; mtimeNs: bigint };
/** A screenshot as main keeps it in a listing; the renderer only sees its name and time. */
export type ListedScreenshot = { name: string; modified: number; path: string; identity: FileIdentity };

export const fileIdentity = (info: BigIntStats): FileIdentity => ({
  dev: info.dev,
  ino: info.ino,
  size: info.size,
  mtimeNs: info.mtimeNs,
});

export function sameFile(a: FileIdentity, b: FileIdentity) {
  return a.dev === b.dev && a.ino === b.ino && a.size === b.size && a.mtimeNs === b.mtimeNs;
}

/** The newest images in `folder`. A folder that does not exist (yet) has none. */
export async function listScreenshots(folder: string): Promise<ListedScreenshot[]> {
  let names;
  try {
    names = await readdir(folder, { withFileTypes: true });
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return [];
    throw error;
  }
  // Only image names are looked at: a Desktop may hold thousands of other files.
  const entries = await Promise.all(
    names
      .filter((entry) => entry.isFile() && imageName(entry.name))
      .map(async (entry) => {
        const path = join(folder, entry.name);
        // Not followed: a name made a link since the folder was read is left out.
        const info = await lstat(path, { bigint: true }).catch(() => undefined);
        if (!info?.isFile()) return undefined;
        return { name: entry.name, modified: Number(info.mtimeMs), file: true, path, identity: fileIdentity(info) };
      }),
  );
  return newestImages(entries.filter((entry) => !!entry)).map(({ name, modified, path, identity }) => ({
    name,
    modified,
    path,
    identity,
  }));
}

/**
 * Opens a listed screenshot only while it is still the file the listing saw:
 * not followed if its name became a link, and checked through the handle
 * itself, so whatever is read through the handle is that file, wherever the
 * name points by then. Undefined when it changed or is gone.
 */
export async function openListed(path: string, listed: FileIdentity) {
  const file = await open(path, constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0)).catch(() => undefined);
  if (!file) return undefined;
  const info = await file.stat({ bigint: true }).catch(() => undefined);
  if (info?.isFile() && sameFile(fileIdentity(info), listed)) return file;
  await file.close();
  return undefined;
}

/**
 * Reads picked screenshots as attachments: data URLs in the form the SDK's
 * attachment reader makes (it only takes paths, which name whatever is there
 * by the time it opens them). Each is read through the handle `openListed`
 * checked, and checked again after, so a name swapped for a link or another
 * file at any point is refused, never followed. `hooks.checked` lets tests
 * act between the check and the read.
 */
export async function readListedScreenshots(
  picks: { path: string; identity: FileIdentity }[],
  hooks: { checked?: (path: string) => Promise<void> } = {},
): Promise<string[]> {
  const output: string[] = [];
  let total = 0;
  for (const { path, identity } of picks) {
    const type = SCREENSHOT_TYPES[extname(path).toLowerCase()];
    if (!type) throw new TypeError("Unsupported attachment extension");
    const file = await openListed(path, identity);
    if (!file) throw new Error(SCREENSHOTS_CHANGED);
    try {
      await hooks.checked?.(path);
      const size = Number(identity.size);
      total += size;
      if (total > ATTACHMENT_BYTES) throw new RangeError("Attachments exceed 20 MiB");
      const bytes = Buffer.alloc(size);
      let offset = 0;
      while (offset < size) {
        const { bytesRead } = await file.read(bytes, offset, size - offset, offset);
        if (!bytesRead) break;
        offset += bytesRead;
      }
      // Cut short, or written to, while it was read.
      if (offset !== size || !sameFile(fileIdentity(await file.stat({ bigint: true })), identity))
        throw new Error(SCREENSHOTS_CHANGED);
      output.push(`data:${type};name=${encodeURIComponent(basename(path))};base64,${bytes.toString("base64")}`);
    } finally {
      await file.close();
    }
  }
  return output;
}
