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
/** Image types the attachment reader takes, by extension, named as it names them. */
export const SCREENSHOT_TYPES: Record<string, string> = {
  ".png": "image/png",
  ".jpg": "image/jpeg",
  ".jpeg": "image/jpeg",
  ".gif": "image/gif",
  ".webp": "image/webp",
  // The server converts these to PNG or JPEG.
  ".heic": "image/heic",
  ".heif": "image/heif",
  ".bmp": "image/bmp",
  ".tif": "image/tiff",
  ".tiff": "image/tiff",
};
export const SCREENSHOT_EXTENSIONS = new Set(Object.keys(SCREENSHOT_TYPES));

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
export function newestImages<T extends FolderEntry>(entries: T[], limit = SCREENSHOT_LIMIT): T[] {
  return entries
    .filter((entry) => entry.file && imageName(entry.name))
    .sort((a, b) => b.modified - a.modified || a.name.localeCompare(b.name))
    .slice(0, limit);
}

/** Why a pick from a listing that no longer holds is refused (translated in main). */
export const SCREENSHOTS_CHANGED = "The screenshots changed. Open Screenshots again.";

/**
 * The last listing each window showed. A pick names a listing and places in
 * it; a listing the window has since replaced, or a place outside it, is
 * refused.
 */
export function createScreenshotListings<T>() {
  const listings = new Map<number, { id: string; items: T[] }>();
  return {
    remember(owner: number, items: T[]) {
      const id = randomUUID();
      listings.set(owner, { id, items: [...items] });
      return id;
    },
    resolve(owner: number, id: unknown, indexes: unknown): T[] {
      const listing = listings.get(owner);
      if (!listing || listing.id !== id) throw new Error(SCREENSHOTS_CHANGED);
      if (
        !Array.isArray(indexes) ||
        !indexes.length ||
        indexes.length > SCREENSHOT_PICK_LIMIT ||
        new Set(indexes).size !== indexes.length ||
        !indexes.every((index) => Number.isInteger(index) && index >= 0 && index < listing.items.length)
      )
        throw new Error("Choose up to 20 screenshots from the list.");
      return indexes.map((index) => listing.items[index]);
    },
    forget(owner: number) {
      listings.delete(owner);
    },
  };
}

/** How long a click or key press lets its window list screenshots: Chromium's own span for user activation. */
export const ACTIVATION_SPAN = 5000;

/**
 * Whether an input event Electron reports for a window (`input-event`) is the
 * person choosing something: a click, a tap, Enter or Space. Main sees it on
 * its way to the page, so a page cannot make one up.
 */
export function activatingInput(input: { type: string; key?: string }) {
  if (input.type === "mouseDown" || input.type === "touchEnd" || input.type === "gestureTap") return true;
  return (input.type === "keyDown" || input.type === "rawKeyDown") && (input.key === "Enter" || input.key === " ");
}

/**
 * When each window last had such an input. Listing screenshots takes it:
 * one click or key press lists the folder once, and only shortly after.
 */
export function createActivations(span = ACTIVATION_SPAN, now = () => Date.now()) {
  const last = new Map<number, number>();
  return {
    note(owner: number) {
      last.set(owner, now());
    },
    take(owner: number) {
      const at = last.get(owner);
      last.delete(owner);
      return at !== undefined && now() - at <= span;
    },
    forget(owner: number) {
      last.delete(owner);
    },
  };
}
