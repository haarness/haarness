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
  /** `keep`: the reader asked for a tab of its own (Command/Control with the
   * pick, or a middle-click); only chats read it. */
  action: (open?: { keep?: boolean }) => unknown;
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

/** Where `phrase` starts a word of `title`, as "term" does in "Toggle
 * terminal" (and not in "Deterministic"). */
function atWordStart(title: string, phrase: string) {
  for (let at = title.indexOf(phrase); at >= 0; at = title.indexOf(phrase, at + 1))
    if (at === 0 || !/[\p{L}\p{N}]/u.test(title[at - 1])) return true;
  return false;
}

/** Exact titles first, then prefixes, words, aliases and compact fuzzy
 * matches. A match inside a word counts for little, less than an alias:
 * "term" is about "Terminal", hardly about "Deterministic". */
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
  let score = title.startsWith(phrase) ? 500 : atWordStart(title, phrase) ? 350 : 0;
  for (const word of words) {
    if (title.split(/[^\p{L}\p{N}]+/u).some((part) => part.startsWith(word)))
      score += 100;
    else if (title.includes(word)) score += 10;
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

/** Settings and their shortcuts give way to an action that matches as well. */
const settingGroups = new Set(["Settings", "Shortcuts"]);

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
        Number(settingGroups.has(a.entry.group)) - Number(settingGroups.has(b.entry.group)) ||
        a.index - b.index,
    )
    .map(({ entry }) => entry);
}

/** Words that say a search is about keys: whole, and ones a query may stop
 * short of, beside `names` (the interface's own words for shortcuts, in its
 * language). Command and Control read as "mod" (`normalizeSearch`). */
const keyWords = ["key", "keys", "mod", "shift", "alt", "binding"];
const keyStems = ["shortcut", "shortcuts", "keyboard", "hotkey", "keybind", "keybinding"];

/**
 * Whether `query` asks about keys or shortcuts: it names them ("shortcut",
 * "key", a modifier such as ⌘ or Ctrl, or the start of such a word in
 * `names`). Only then does Search list a "Shortcut: …" entry beside the
 * action it belongs to, which shows its key anyway.
 */
export function asksForKeys(query: string, names: readonly string[] = []) {
  const split = (text: string) => normalizeSearch(text).split(/[^\p{L}\p{N}]+/u).filter(Boolean);
  const words = split(query);
  const stems = [...keyStems, ...names.flatMap(split)];
  return words.some(
    (word) =>
      keyWords.includes(word) ||
      (word.length >= 3 && stems.some((stem) => stem.startsWith(word))),
  );
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

const arrows: Record<string, string> = {
  ArrowUp: "↑",
  ArrowDown: "↓",
  ArrowLeft: "←",
  ArrowRight: "→",
};
/**
 * A shortcut the way the platform writes it: key glyphs on macOS (⌘ ⇧ B), key
 * names joined by "+" elsewhere (Ctrl+Shift+B). Show it in a `<kbd dir="ltr">`
 * or through `shortcutText`: a right-to-left line would reorder the glyphs.
 */
export function shortcutLabel(value: string, platform: string) {
  value = value.replace(/\bArrow(Up|Down|Left|Right)\b/g, (name) => arrows[name]);
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
