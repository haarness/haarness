import type { Preferences } from "../shared";

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
