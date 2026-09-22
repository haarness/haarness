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

export function normalizeSearch(text: string) {
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
export function searchScore(entry: SearchEntry, query: string) {
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
 * `Mod` is Command on macOS and Control elsewhere. Every modifier must match
 * exactly, so `Tab` does not fire when Shift is held.
 */
export function matchesShortcut(
  event: Pick<
    KeyboardEvent,
    "key" | "metaKey" | "ctrlKey" | "altKey" | "shiftKey"
  >,
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
  const primary = platform === "darwin" ? event.metaKey : event.ctrlKey;
  if (wants("mod") !== primary) return false;
  if (wants("shift") !== event.shiftKey) return false;
  if (wants("alt") !== event.altKey) return false;
  // With Mod claiming one of them, only the other is still free to check.
  if (!wants("mod")) {
    if (wants("ctrl") !== event.ctrlKey) return false;
    if (wants("meta") !== event.metaKey) return false;
  }
  const pressed = event.key.toLowerCase();
  return pressed === key || (key === "space" && event.key === " ");
}

export function shortcutLabel(value: string, platform: string) {
  return value
    .replace(/\bMod\b/g, platform === "darwin" ? "⌘" : "Ctrl")
    .replace(/\bShift\b/g, platform === "darwin" ? "⇧" : "Shift")
    .replaceAll("+", " ");
}
