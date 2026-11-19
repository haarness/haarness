import type { Preferences } from "../shared";
import { matchesShortcut } from "./search";

export type OptionAsMeta = Preferences["terminalOptionAsMeta"];

/**
 * Whether Option sends Meta in the terminal: ESC and the key, which shells
 * and editors read as Alt+B, Alt+F or Alt+. — rather than the character the
 * keyboard layout composes with Option (∫, ƒ, ≥, or @ and [ on German and
 * other layouts). "left" keeps the right Option for those characters, as
 * Terminal.app, iTerm2 and Ghostty offer.
 *
 * xterm only has one switch for both keys (`macOptionIsMeta`), read as it
 * handles each key, so the view sets it from `meta(event, mode)` just before.
 * Only macOS composes characters with Option; elsewhere Alt is always Meta.
 */
export function optionKeys() {
  let left = false;
  let right = false;
  return {
    meta(
      event: Pick<KeyboardEvent, "type" | "code" | "altKey">,
      mode: OptionAsMeta,
    ): boolean {
      if (event.code === "AltLeft") left = event.type === "keydown";
      if (event.code === "AltRight") right = event.type === "keydown";
      // A release missed while the window was in the background.
      if (!event.altKey && event.code !== "AltLeft" && event.code !== "AltRight")
        left = right = false;
      if (mode === "both") return true;
      if (mode === "off") return false;
      return !(right && !left);
    },
    /** The window lost the keyboard: no key is held any more. */
    reset() {
      left = right = false;
    },
  };
}

/**
 * Whether a key pressed with the keyboard in the terminal clears it: ⌘K on
 * macOS, as in Terminal, iTerm2 and VS Code. Elsewhere Ctrl+K belongs to the
 * shell, which cuts the line after the cursor, so there the terminal's
 * right-click menu has Clear and no key does.
 */
export function clearsTerminal(
  event: Pick<KeyboardEvent, "key" | "metaKey" | "ctrlKey" | "altKey" | "shiftKey"> & { code?: string },
  platform: string,
) {
  return platform === "darwin" && matchesShortcut(event, "Mod+K", platform);
}
