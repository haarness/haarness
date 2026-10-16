import { shortcutLabel } from "./renderer/search";

/**
 * The system-wide shortcut that shows Haarness from any app and hides it
 * again. Off until turned on in Settings: while it is registered, no other
 * app receives the combination (Option+Space types a non-breaking space in
 * many layouts, and iTerm2, Raycast and others offer it too).
 */
export interface GlobalShortcut {
  enabled: boolean;
  /** As Settings records it, such as `Alt+Space` or `Mod+Shift+H`. */
  keys: string;
}
export const defaultGlobalShortcut: GlobalShortcut = {
  enabled: false,
  keys: "Alt+Space",
};

const MODIFIERS: Record<string, string> = {
  mod: "CommandOrControl",
  ctrl: "Control",
  alt: "Alt",
  shift: "Shift",
};
const NAMED = new Set([
  "Space", "Up", "Down", "Left", "Right", "Home", "End", "PageUp", "PageDown",
  ...Array.from({ length: 24 }, (_, index) => "F" + (index + 1)),
]);

/** One key after its modifiers, in canonical spelling, or undefined. */
function key(value: string) {
  if (/^[a-z0-9]$/i.test(value)) return value.toUpperCase();
  if (/^[,.;/\\[\]\-=`']$/.test(value)) return value;
  return [...NAMED].find((name) => name.toLowerCase() === value.toLowerCase());
}

/**
 * `keys` in Electron's accelerator spelling, or undefined when it cannot be
 * a system-wide shortcut: it needs Command, Control or Option (Shift alone
 * would take a capital letter from every app) and exactly one other key.
 */
export function globalAccelerator(keys: string) {
  const parts = keys.split("+").map((part) => part.trim());
  const last = key(parts.pop() ?? "");
  const modifiers = parts.map((part) => MODIFIERS[part.toLowerCase()]);
  if (!last || !modifiers.length || modifiers.some((part) => !part)) return;
  if (new Set(modifiers).size !== modifiers.length) return;
  if (modifiers.every((part) => part === "Shift")) return;
  return [...modifiers, last].join("+");
}

/** Combinations the system keeps for itself on `platform`. */
const RESERVED: Record<string, string[]> = {
  darwin: [
    "Mod+Space", "Ctrl+Space", "Mod+Alt+Space", "Ctrl+Mod+Space", "Mod+Tab",
    "Mod+Shift+3", "Mod+Shift+4", "Mod+Shift+5", "Mod+Q", "Mod+W", "Mod+H",
    "Mod+M", "Mod+Alt+Escape", "Ctrl+Mod+Q", "Ctrl+Mod+F",
  ],
  win32: ["Alt+Tab", "Alt+F4", "Mod+Alt+Delete", "Mod+Escape", "Mod+Shift+Escape"],
  linux: ["Alt+Tab", "Alt+F4", "Mod+Alt+Delete"],
};
const canonical = (keys: string) =>
  keys
    .split("+")
    .map((part) => part.trim().toLowerCase())
    .sort()
    .join("+");

/** Whether `keys` is a combination the system on `platform` keeps. */
export function reservedGlobalShortcut(keys: string, platform: string) {
  const value = canonical(keys);
  return (RESERVED[platform] ?? []).some((item) => canonical(item) === value);
}

/**
 * Why `keys` cannot be the global shortcut on `platform`, in the language
 * `say` translates to, or undefined when it can be.
 */
export function globalShortcutError(
  keys: string,
  platform: string,
  say: (key: string) => string = (key) => key,
) {
  if (!globalAccelerator(keys))
    return say("Use a modifier key and one more key, such as {keys}.").replace(
      "{keys}",
      shortcutLabel(defaultGlobalShortcut.keys, platform),
    );
  if (reservedGlobalShortcut(keys, platform))
    return say("{keys} belongs to the system. Choose another shortcut.").replace(
      "{keys}",
      shortcutLabel(keys, platform),
    );
}
