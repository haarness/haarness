import type { IconName } from "./icons";

export interface SearchEntry {
  key: string;
  group: string;
  name: string;
  icon: IconName;
  provider?: string;
  detail?: string;
  keywords?: string;
  hint?: string;
  disabled?: boolean;
  destination?: "settings";
  action: () => unknown;
}

function normalizeSearch(text: string) {
  return text
    .normalize("NFKD")
    .replace(/\p{M}/gu, "")
    .toLowerCase()
    .replace(/⌘|command|cmd|control|ctrl/g, "mod")
    .replace(/⇧/g, "shift")
    .replace(/⌥/g, "alt")
    .replace(/[+_-]/g, " ")
    .replace(/\s+/g, " ")
    .trim();
}

/** Exact titles first, then prefixes, words, aliases and compact fuzzy matches. */
function searchScore(entry: SearchEntry, query: string) {
  const title = normalizeSearch(entry.name);
  const words = normalizeSearch(query).split(" ").filter(Boolean);
  if (!words.length) return 0;
  const phrase = words.join(" ");
  if (title === phrase) return 1000;
  const content = normalizeSearch(
    [entry.name, entry.detail, entry.keywords, entry.hint, entry.group]
      .filter(Boolean)
      .join(" "),
  );
  let score = title.startsWith(phrase)
    ? 500
    : title.includes(phrase)
      ? 350
      : 0;
  for (const word of words) {
    if (title.split(/[^\p{L}\p{N}]+/u).some((part) => part.startsWith(word)))
      score += 100;
    else if (title.includes(word)) score += 60;
    else if (content.includes(word)) score += 20;
    else {
      // Keep fuzzy matching within a title word so unrelated long descriptions
      // cannot match almost any query. Short queries must match literally.
      if (
        word.length < 3 ||
        !title.split(/[^\p{L}\p{N}]+/u).some((part) => {
          let index = 0;
          for (const char of part) if (char === word[index]) index++;
          return index === word.length;
        })
      )
        return -1;
      score += 5;
    }
  }
  return score;
}

export function searchEntries(entries: SearchEntry[], query: string) {
  const seen = new Set<string>();
  return entries
    .filter((entry) => {
      if (seen.has(entry.key)) return false;
      seen.add(entry.key);
      return true;
    })
    .map((entry, index) => ({
      entry,
      index,
      score: searchScore(entry, query),
    }))
    .filter(({ score }) => score >= 0)
    .sort(
      (a, b) =>
        b.score - a.score ||
        Number(!!a.entry.disabled) - Number(!!b.entry.disabled) ||
        a.index - b.index,
    )
    .map(({ entry }) => entry);
}

/**
 * Does a key event match a shortcut such as `Mod+K`, `Shift+Tab` or `Tab`?
 * `Mod` is Command on macOS and Control elsewhere, and `Ctrl` is Control
 * everywhere (so `Ctrl+Tab` means the same keys on every platform). Every
 * modifier must match exactly, so `Tab` does not fire when Shift is held.
 */
export function matchesShortcut(
  event: Pick<
    KeyboardEvent,
    "key" | "metaKey" | "ctrlKey" | "altKey" | "shiftKey"
  > & { code?: string },
  value: string,
  platform: string,
) {
  const parts = value
    .toLowerCase()
    .split("+")
    .map((part) => part.trim())
    .filter(Boolean);
  const key = parts.pop();
  if (!key) return false;
  const wants = (name: string) => parts.includes(name);
  const mac = platform === "darwin";
  // Off macOS, Mod and Ctrl are one key.
  const primary = mac ? event.metaKey : event.ctrlKey;
  if ((wants("mod") || (!mac && wants("ctrl"))) !== primary) return false;
  if (wants("shift") !== event.shiftKey) return false;
  if (wants("alt") !== event.altKey) return false;
  // The other of Command and Control is still free to check.
  if (mac ? wants("ctrl") !== event.ctrlKey : wants("meta") !== event.metaKey)
    return false;
  const pressed = event.key.toLowerCase();
  // The recorder stores keys by physical key (`event.code`): with Option held
  // macOS reports ⌥Z as "Ω", and Shift turns "." into ">".
  const physical = physicalKey(event.code)?.toLowerCase();
  return pressed === key || physical === key || (key === "space" && event.key === " ");
}

const punctuation: Record<string, string> = {
  Period: ".", Comma: ",", Slash: "/", Semicolon: ";", Backslash: "\\",
  BracketLeft: "[", BracketRight: "]", Minus: "-", Equal: "=", Backquote: "`",
};
/** The shortcut key for a physical key (`event.code`): a letter, a digit or a
 * punctuation key, whatever Shift or Option turn it into. */
export function physicalKey(code = "") {
  return /^(?:Key|Digit)(.)$/.exec(code)?.[1] ?? punctuation[code];
}

/**
 * A shortcut the way the platform writes it: key glyphs on macOS (⌘ ⇧ B), key
 * names joined by "+" elsewhere (Ctrl+Shift+B). Show it in a `<kbd dir="ltr">`
 * or through `shortcutText`: a right-to-left line would reorder the glyphs.
 */
export function shortcutLabel(value: string, platform: string) {
  if (platform !== "darwin") return value.replace(/\bMod\b/g, "Ctrl");
  return value
    .replace(/\bMod\b/g, "⌘")
    .replace(/\bShift\b/g, "⇧")
    .replace(/\bAlt\b/g, "⌥")
    .replace(/\bCtrl\b/g, "⌃")
    .replaceAll("+", " ");
}
/** A shortcut as a field shows it for editing: Mod is written Ctrl off
 * macOS, where the two are one key (the field still saves Mod). */
export function shortcutField(value: string, platform: string) {
  return platform === "darwin" ? value : value.replace(/\bMod\b/g, "Ctrl");
}
/** What a field's text saves as: off macOS, Ctrl is Mod, so a combination
 * typed either way is the same shortcut to the duplicate check. */
export function shortcutFromField(value: string, platform: string) {
  return platform === "darwin" ? value : value.replace(/\bctrl\b/gi, "Mod");
}
/** A shortcut inside running text such as a tooltip, isolated left to right. */
export function shortcutText(value: string, platform: string) {
  return "\u2066" + shortcutLabel(value, platform) + "\u2069";
}
