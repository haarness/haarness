import type { BigIntStats } from "node:fs";
import { lstat, readdir } from "node:fs/promises";
import { join } from "node:path";
import { SCREENSHOTS_CHANGED, imageName, newestImages } from "../screenshots";

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

/** Whether `path` is still the file a listing saw: not a link, and not replaced or rewritten since. */
export async function unchangedFile(path: string, before: FileIdentity) {
  const info = await lstat(path, { bigint: true }).catch(() => undefined);
  return !!info && info.isFile() && sameFile(fileIdentity(info), before);
}

/**
 * Reads picked screenshots with `read` (the attachment reader), but only files
 * that are still the ones the listing saw: a name swapped for a link to
 * another file, or replaced or rewritten since, is refused. Checked again
 * after the read, so a swap while it was being read is refused too.
 */
export async function readListedScreenshots<T>(
  picks: { path: string; identity: FileIdentity }[],
  read: (paths: string[]) => Promise<T[]>,
): Promise<T[]> {
  const unchanged = async () =>
    (await Promise.all(picks.map((pick) => unchangedFile(pick.path, pick.identity)))).every(Boolean);
  if (!(await unchanged())) throw new Error(SCREENSHOTS_CHANGED);
  const values = await read(picks.map((pick) => pick.path));
  if (!(await unchanged())) throw new Error(SCREENSHOTS_CHANGED);
  return values;
}
