import { test } from "node:test";
import assert from "node:assert/strict";
import {
  matchesShortcut,
  physicalKey,
  searchEntries,
  shortcutLabel,
  shortcutText,
  type SearchEntry,
} from "../src/renderer/search";
import { defaults, migrateAgentKeys } from "../src/shared";
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
  assert.equal(shortcutLabel("Mod+Shift+L", "win32"), "Ctrl+Shift+L");
  assert.equal(shortcutLabel("Mod+Alt+Z", "darwin"), "⌘ ⌥ Z");
  assert.equal(shortcutLabel("Ctrl+Space", "darwin"), "⌃ Space");
  assert.equal(shortcutLabel("Mod+F", "linux"), "Ctrl+F");
  // Either form finds the shortcut it names.
  assert.equal(searchEntries([entry("new", "New chat", { hint: "Ctrl+Shift+L" })], "⌘ ⇧ L")[0].key, "new");
  assert.equal(shortcutText("Mod+K", "darwin"), "\u2066⌘ K\u2069");
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

  // Ctrl is Control on every platform: Ctrl+Tab steps tabs on macOS and
  // elsewhere, and Command+Tab never matches it.
  for (const platform of ["darwin", "win32", "linux"]) {
    assert.equal(matchesShortcut(key("Tab", { ctrlKey: true }), "Ctrl+Tab", platform), true);
    assert.equal(matchesShortcut(key("Tab", { ctrlKey: true, shiftKey: true }), "Ctrl+Shift+Tab", platform), true);
    assert.equal(matchesShortcut(key("Tab", { ctrlKey: true, shiftKey: true }), "Ctrl+Tab", platform), false);
    assert.equal(matchesShortcut(key("Tab"), "Ctrl+Tab", platform), false);
  }
  assert.equal(matchesShortcut(key("Tab", { metaKey: true }), "Ctrl+Tab", "darwin"), false);
  assert.equal(matchesShortcut(key("Tab", { ctrlKey: true, metaKey: true }), "Ctrl+Tab", "darwin"), false);

  // Recorded chords name the physical key: ⌥⌘Z arrives as "Ω" on macOS.
  const optionZ = { ...key("Ω", { metaKey: true, altKey: true }), code: "KeyZ" };
  assert.equal(matchesShortcut(optionZ, "Mod+Alt+Z", "darwin"), true);
  assert.equal(matchesShortcut(optionZ, "Mod+Alt+X", "darwin"), false);
  assert.equal(matchesShortcut({ ...key("¡", { altKey: true }), code: "Digit1" }, "Alt+1", "darwin"), true);

  // Shift turns "." into ">", so punctuation also matches by physical key.
  const shiftPeriod = { ...key(">", { metaKey: true, shiftKey: true }), code: "Period" };
  assert.equal(matchesShortcut(shiftPeriod, "Mod+Shift+.", "darwin"), true);
  assert.equal(matchesShortcut(shiftPeriod, "Mod+.", "darwin"), false);
  assert.equal(matchesShortcut({ ...key(".", { ctrlKey: true }), code: "Period" }, "Mod+.", "win32"), true);
  assert.equal(physicalKey("Period"), ".");
  assert.equal(physicalKey("KeyE"), "E");
  assert.equal(physicalKey("Tab"), undefined);
});

test("agent keys default to chords so Tab keeps moving focus", () => {
  assert.equal(defaults.shortcuts.agentNext, "Mod+.");
  assert.equal(defaults.shortcuts.agentPrev, "Mod+Shift+.");
  for (const value of Object.values(defaults.shortcuts)) assert.doesNotMatch(value, /^(Shift\+)?Tab$/);
  // Saved preferences still holding the old Tab defaults move to the chords.
  const old = { ...defaults.shortcuts, agentNext: "Tab", agentPrev: "Shift+Tab" };
  assert.deepEqual(migrateAgentKeys(old), defaults.shortcuts);
  // A key someone chose is kept, and so is Tab when its chord is taken.
  const custom = { ...defaults.shortcuts, agentNext: "Ctrl+Space", agentPrev: "Shift+Tab" };
  assert.deepEqual(migrateAgentKeys(custom), { ...custom, agentPrev: "Mod+Shift+." });
  const taken = { ...old, palette: "Mod+." };
  assert.deepEqual(migrateAgentKeys(taken), { ...taken, agentPrev: "Mod+Shift+." });
  assert.equal(migrateAgentKeys(defaults.shortcuts), defaults.shortcuts);
});
