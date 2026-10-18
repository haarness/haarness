import { posix, win32 } from "node:path";
import { randomUUID } from "node:crypto";

/**
 * The screenshot picker: which folder holds the person's screenshots, which of
 * its images are newest, and the listing the renderer picks from. The
 * renderer only ever names images by their place in the window's last
 * listing, so it cannot read any other file through the picker.
 */

/** Images one listing offers, newest first. */
export const SCREENSHOT_LIMIT = 48;
/** Images one message can take, as the composer's own attachment limit. */
export const SCREENSHOT_PICK_LIMIT = 20;
/** Image types the attachment reader takes. */
export const SCREENSHOT_EXTENSIONS = new Set([
  ".png",
  ".jpg",
  ".jpeg",
  ".gif",
  ".webp",
  ".heic",
  ".heif",
  ".bmp",
  ".tif",
  ".tiff",
]);

/**
 * The folder macOS saves screenshots to, from `defaults read
 * com.apple.screencapture location`: quotes around a path with spaces go,
 * and `~` is the home folder.
 */
export function parseDefaultsLocation(stdout: string, home?: string): string | undefined {
  const raw = stdout.trim().replace(/^"(.*)"$/, "$1").trim();
  if (!raw) return undefined;
  if (raw === "~" || raw.startsWith("~/")) return home ? home + raw.slice(1) : undefined;
  return raw.startsWith("/") ? raw : undefined;
}

/**
 * The folder the picker opens. macOS: the Screenshot app's "Save to" folder
 * (`location`, when it is a folder that exists), otherwise the Desktop.
 * Windows: Pictures\Screenshots. Linux has no standard folder, so it opens
 * the one the person chose last, or none and asks.
 */
export function screenshotFolder(input: {
  platform: NodeJS.Platform;
  home: string;
  /** macOS's own setting, already checked to be an existing folder. */
  location?: string;
  remembered?: unknown;
}): string | undefined {
  if (input.platform === "darwin") return input.location ?? posix.join(input.home, "Desktop");
  if (input.platform === "win32") return win32.join(input.home, "Pictures", "Screenshots");
  return typeof input.remembered === "string" && input.remembered ? input.remembered : undefined;
}

/** The folder as a person reads it: the home folder as `~`. */
export function displayFolder(folder: string, home: string) {
  if (home && (folder === home || folder.startsWith(home + "/") || folder.startsWith(home + "\\")))
    return "~" + folder.slice(home.length);
  return folder;
}

export type FolderEntry = { name: string; modified: number; file: boolean };

/** A file name the picker offers: an image type the attachment reader takes, not hidden. */
export function imageName(name: string) {
  if (name.startsWith(".")) return false;
  const dot = name.lastIndexOf(".");
  return dot > 0 && SCREENSHOT_EXTENSIONS.has(name.slice(dot).toLowerCase());
}

/** The newest images of a folder listing, without hidden files or other kinds. */
export function newestImages(entries: FolderEntry[], limit = SCREENSHOT_LIMIT): FolderEntry[] {
  return entries
    .filter((entry) => entry.file && imageName(entry.name))
    .sort((a, b) => b.modified - a.modified || a.name.localeCompare(b.name))
    .slice(0, limit);
}

/**
 * The last listing each window showed. A pick names a listing and places in
 * it; a listing the window has since replaced, or a place outside it, is
 * refused.
 */
export function createScreenshotListings() {
  const listings = new Map<number, { id: string; paths: string[] }>();
  return {
    remember(owner: number, paths: string[]) {
      const id = randomUUID();
      listings.set(owner, { id, paths: [...paths] });
      return id;
    },
    resolve(owner: number, id: unknown, indexes: unknown): string[] {
      const listing = listings.get(owner);
      if (!listing || listing.id !== id) throw new Error("The screenshots changed. Open Screenshots again.");
      if (
        !Array.isArray(indexes) ||
        !indexes.length ||
        indexes.length > SCREENSHOT_PICK_LIMIT ||
        new Set(indexes).size !== indexes.length ||
        !indexes.every((index) => Number.isInteger(index) && index >= 0 && index < listing.paths.length)
      )
        throw new Error("Choose up to 20 screenshots from the list.");
      return indexes.map((index) => listing.paths[index]);
    },
    forget(owner: number) {
      listings.delete(owner);
    },
  };
}
