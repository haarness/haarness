import { execFile } from "node:child_process";
import { readdir, stat } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";
import { nativeImage } from "electron";
import { imageName, newestImages, parseDefaultsLocation, screenshotFolder } from "../screenshots";

/** Thumbnail width: two device pixels for each of a tile's CSS pixels. */
const THUMBNAIL = 280;

/** The Screenshot app's "Save to" folder, when it names a folder that exists. */
async function macLocation(home: string) {
  if (process.platform !== "darwin") return undefined;
  const stdout = await new Promise<string>((resolve) =>
    execFile(
      "/usr/bin/defaults",
      ["read", "com.apple.screencapture", "location"],
      { timeout: 3000 },
      (error, output) => resolve(error ? "" : String(output)),
    ),
  );
  const location = parseDefaultsLocation(stdout, home);
  if (!location) return undefined;
  return (await stat(location).then((info) => info.isDirectory(), () => false)) ? location : undefined;
}

/** The folder the picker opens, or none on Linux until one is chosen. */
export async function defaultScreenshotFolder(remembered: unknown) {
  // End-to-end runs point the picker at a fixture folder, never the person's own.
  if (process.env.HAARNESS_DESKTOP_TEST === "1" && process.env.HAARNESS_SCREENSHOTS_TEST_DIR)
    return process.env.HAARNESS_SCREENSHOTS_TEST_DIR;
  const home = homedir();
  return screenshotFolder({
    platform: process.platform,
    home,
    location: await macLocation(home),
    remembered,
  });
}

/** The newest images in `folder`. A folder that does not exist (yet) has none. */
export async function listScreenshots(folder: string) {
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
        const info = await stat(join(folder, entry.name)).catch(() => undefined);
        return info ? { name: entry.name, modified: info.mtimeMs, file: info.isFile() } : undefined;
      }),
  );
  return newestImages(entries.filter((entry) => !!entry)).map((entry) => ({
    ...entry,
    path: join(folder, entry.name),
  }));
}

async function thumbnail(path: string) {
  try {
    // Electron reads the image itself where it can (on macOS every type the
    // system knows) and scales it by width, keeping its shape: macOS's own
    // thumbnails come back square. Windows draws the types Electron cannot.
    let image = nativeImage.createFromPath(path);
    if (image.isEmpty() && process.platform === "win32")
      image = await nativeImage.createThumbnailFromPath(path, { width: THUMBNAIL, height: THUMBNAIL });
    if (image.isEmpty()) return "";
    const fitted = image.getSize().width > THUMBNAIL ? image.resize({ width: THUMBNAIL }) : image;
    return fitted.toDataURL();
  } catch {
    return "";
  }
}

/** Thumbnails for `paths`, a few at a time; one that cannot be drawn is empty. */
export async function screenshotThumbnails(paths: string[]) {
  const output: string[] = new Array(paths.length).fill("");
  let next = 0;
  await Promise.all(
    Array.from({ length: Math.min(6, paths.length) }, async () => {
      while (next < paths.length) {
        const index = next++;
        output[index] = await thumbnail(paths[index]);
      }
    }),
  );
  return output;
}
