import { test } from "node:test";
import assert from "node:assert/strict";
import {
  matchesShortcut,
  searchEntries,
  shortcutLabel,
  type SearchEntry,
} from "../src/renderer/search";
const entry = (
  key: string,
  name: string,
  extra: Partial<SearchEntry> = {},
): SearchEntry => ({
  key,
  name,
  group: "Commands",
  icon: "search",
  action() {},
  ...extra,
});
test("universal search ranks exact names, aliases, unordered terms and fuzzy title words", () => {
  const entries = [
    entry("explorer", "File explorer", { keywords: "browse folders" }),
    entry("font", "Terminal text size", {
      keywords: "font",
      group: "Settings",
    }),
    entry("terminal", "Terminal"),
  ];
  assert.equal(searchEntries(entries, "terminal")[0].key, "terminal");
  assert.equal(searchEntries(entries, "font terminal")[0].key, "font");
  assert.equal(searchEntries(entries, "explr")[0].key, "explorer");
  assert.equal(searchEntries(entries, "browse folder")[0].key, "explorer");
  assert.equal(searchEntries(entries, "zzqzz").length, 0);
});
test("search finds configured shortcut notation and preserves same-title workspace identities", () => {
  const entries = [
    entry("root:1", "Café", { detail: "Primary project" }),
    entry("tree:1", "Café", { detail: "Feature worktree" }),
    entry("root:1", "Duplicate"),
    entry("new", "New chat", { hint: "⌘ ⇧ L" }),
  ];
  assert.deepEqual(
    searchEntries(entries, "cafe").map((item) => item.key),
    ["root:1", "tree:1"],
  );
  assert.deepEqual(
    searchEntries(entries, "cafe feature").map((item) => item.key),
    ["tree:1"],
  );
  assert.equal(searchEntries(entries, "Ctrl+Shift+L")[0].key, "new");
  assert.equal(shortcutLabel("Mod+Shift+L", "darwin"), "⌘ ⇧ L");
  assert.equal(shortcutLabel("Mod+Shift+L", "win32"), "Ctrl Shift L");
});
test("search does not cut off conversations at eight and excludes nonmatching names", () => {
  const entries = Array.from({ length: 30 }, (_, i) =>
    entry(String(i), "Conversation " + i),
  );
  assert.equal(searchEntries(entries, "conversation").length, 30);
  assert.equal(searchEntries(entries, "Conversation 29")[0].key, "29");
});

test("shortcut matching requires every modifier to agree", () => {
  const key = (
    name: string,
    mods: Partial<Record<"metaKey" | "ctrlKey" | "altKey" | "shiftKey", boolean>> = {},
  ) => ({ key: name, metaKey: false, ctrlKey: false, altKey: false, shiftKey: false, ...mods });

  // A bare key only fires when nothing else is held, so Tab and Shift+Tab
  // stay distinct and neither steals the other's event.
  assert.equal(matchesShortcut(key("Tab"), "Tab", "darwin"), true);
  assert.equal(matchesShortcut(key("Tab", { shiftKey: true }), "Tab", "darwin"), false);
  assert.equal(matchesShortcut(key("Tab", { shiftKey: true }), "Shift+Tab", "darwin"), true);
  assert.equal(matchesShortcut(key("Tab"), "Shift+Tab", "darwin"), false);
  assert.equal(matchesShortcut(key("Tab", { metaKey: true }), "Tab", "darwin"), false);

  // Mod is Command on macOS and Control elsewhere.
  assert.equal(matchesShortcut(key("k", { metaKey: true }), "Mod+K", "darwin"), true);
  assert.equal(matchesShortcut(key("k", { ctrlKey: true }), "Mod+K", "darwin"), false);
  assert.equal(matchesShortcut(key("k", { ctrlKey: true }), "Mod+K", "win32"), true);
  assert.equal(matchesShortcut(key("."), "", "darwin"), false);
});
