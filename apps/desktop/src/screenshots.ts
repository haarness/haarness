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

/** How soon main must hear of a menu opening, or a choice in it, after the press that did it. */
export const PRESS_SPAN = 2000;
/** How long an open Attach menu waits for its choice. */
export const MENU_SPAN = 15_000;

/**
 * What an input event Electron reports for a window (`input-event`) is to the
 * screenshot picker: a press that chooses (a click, a tap, Enter or Space),
 * ArrowDown (which opens a menu from the keyboard), or a typed character.
 * Main sees these on their way to the page, so a page cannot make one up.
 */
export function inputKind(input: { type: string; key?: string }): "press" | "arrow" | "typing" | undefined {
  if (input.type === "mouseDown" || input.type === "touchEnd" || input.type === "gestureTap") return "press";
  if (input.type !== "keyDown" && input.type !== "rawKeyDown") return undefined;
  if (input.key === "Enter" || input.key === " ") return "press";
  if (input.key === "ArrowDown") return "arrow";
  return input.key && [...input.key].length === 1 ? "typing" : undefined;
}

/**
 * Lists the screenshots folder for the Attach menu only. Opening the menu
 * right after a press gets a token; Attach screenshots then lists once with
 * it, but only right after a later press, within a short time of the menu
 * opening and with nothing typed in between. So the press that sends a
 * message, or any other single press, lists nothing. Main cannot see which
 * element a press lands on: a page that has been taken over can still play
 * the menu's two presses, which is why anything else gets a native question.
 */
export function createScreenshotIntents(now = () => Date.now()) {
  type Kind = NonNullable<ReturnType<typeof inputKind>>;
  type Window = {
    count: number;
    last?: { index: number; at: number; kind: Kind };
    /** The last typed character's place among the window's inputs. */
    typed: number;
    menu?: { token: string; index: number; at: number };
  };
  const windows = new Map<number, Window>();
  const state = (owner: number) => {
    let value = windows.get(owner);
    if (!value) windows.set(owner, (value = { count: 0, typed: 0 }));
    return value;
  };
  return {
    input(owner: number, kind: Kind) {
      const value = state(owner);
      value.last = { index: ++value.count, at: now(), kind };
      if (kind === "typing") value.typed = value.count;
    },
    /** The Attach menu opened: a token for its choice, when a press (or ArrowDown) just opened it. */
    menu(owner: number): string | null {
      const value = state(owner);
      const last = value.last;
      value.menu = undefined;
      if (!last || last.kind === "typing" || now() - last.at > PRESS_SPAN) return null;
      value.menu = { token: randomUUID(), index: last.index, at: now() };
      return value.menu.token;
    },
    /** Whether Attach screenshots was just chosen in the menu `token` came from. Uses the token up. */
    chosen(owner: number, token: unknown) {
      const value = state(owner);
      const { menu, last } = value;
      value.menu = undefined;
      return (
        !!menu &&
        !!last &&
        token === menu.token &&
        now() - menu.at <= MENU_SPAN &&
        last.kind === "press" &&
        last.index > menu.index &&
        now() - last.at <= PRESS_SPAN &&
        value.typed <= menu.index
      );
    },
    forget(owner: number) {
      windows.delete(owner);
    },
  };
}
