import { test } from "node:test";
import assert from "node:assert/strict";
import {
  asksForKeys,
  matchesShortcut,
  physicalKey,
  searchEntries,
  shortcutField,
  shortcutFromField,
  shortcutLabel,
  shortcutText,
  type SearchEntry,
} from "../src/renderer/search";
import {
  defaults,
  defaultShortcuts,
  migrateAgentKeys,
  migrateTabKeys,
  shortcutAccepted,
  shortcutKeys,
  shortcutsClash,
} from "../src/shared";
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

test("a match inside a word ranks far below one at a word's start", () => {
  const entries = [
    entry("provider", "Deterministic test provider", { group: "Settings" }),
    entry("toggle", "Toggle terminal"),
    entry("size", "Terminal text size", { group: "Settings" }),
  ];
  assert.deepEqual(searchEntries(entries, "term").map((item) => item.key), ["size", "toggle", "provider"]);
  // Still found, but far behind: below even a match in an entry's keywords.
  const withAlias = [...entries, entry("alias", "Console", { keywords: "terminal shell" })];
  assert.deepEqual(searchEntries(withAlias, "term").slice(-2).map((item) => item.key), ["alias", "provider"]);
});

test("in scripts written without spaces a match anywhere in a title starts a word", () => {
  const settings = (names: string[]) => names.map((name, index) => entry(String(index), name, { group: "Settings" }));
  // Chinese: 浏览器 in 智能体浏览器控制 ranks with the titles that begin
  // with it, above a setting found only by its keywords.
  const zh = [
    entry("alias", "显示完整网址", { group: "Settings", keywords: "浏览器 网址" }),
    ...settings(["智能体浏览器控制", "浏览器设置范围", "浏览器"]),
  ];
  assert.deepEqual(searchEntries(zh, "浏览器").map((item) => item.name), ["浏览器", "浏览器设置范围", "智能体浏览器控制", "显示完整网址"]);
  // Japanese, after a particle: エージェントのブラウザ制御.
  const ja = [
    entry("alias", "完全な URL を表示", { group: "Settings", keywords: "ブラウザ" }),
    ...settings(["エージェントのブラウザ制御", "ブラウザ設定の適用範囲", "ブラウザ"]),
  ];
  assert.deepEqual(searchEntries(ja, "ブラウザ").map((item) => item.name), ["ブラウザ", "ブラウザ設定の適用範囲", "エージェントのブラウザ制御", "完全な URL を表示"]);
  // Thai: กิจกรรมเครื่องมือแบบย่อ ranks above a tool found by its description.
  const th = [
    entry("tool", "web_fetch", { group: "Settings", keywords: "เครื่องมือ" }),
    ...settings(["กิจกรรมเครื่องมือแบบย่อ", "เครื่องมือ"]),
  ];
  assert.deepEqual(searchEntries(th, "เครื่องมือ").map((item) => item.name), ["เครื่องมือ", "กิจกรรมเครื่องมือแบบย่อ", "web_fetch"]);
  // A Latin word still has to begin where the query does.
  assert.deepEqual(searchEntries(settings(["Deterministic", "Terminal"]), "term").map((item) => item.name), ["Terminal", "Deterministic"]);
});

test("an action outranks a setting or shortcut that matches as well", () => {
  const entries = [
    entry("setting", "Open project", { group: "Settings" }),
    entry("shortcut", "Open project", { group: "Shortcuts" }),
    entry("command", "Open project"),
  ];
  assert.deepEqual(searchEntries(entries, "open").map((item) => item.key), ["command", "setting", "shortcut"]);
  // A command that cannot run still gives way to one that can.
  const disabled = [entry("setting", "Open project", { group: "Settings" }), entry("command", "Open project", { disabled: true })];
  assert.deepEqual(searchEntries(disabled, "open").map((item) => item.key), ["setting", "command"]);
});

test("a search is about keys when it names them, a modifier, or the interface's word for shortcuts", () => {
  for (const query of ["shortcut new", "Shortcut: New chat", "short", "keyboard", "key", "Ctrl+N", "⌘ ⇧ L", "cmd", "alt", "hotk"])
    assert.equal(asksForKeys(query), true, query);
  for (const query of ["", "new chat", "terminal", "model", "keep", "theme"])
    assert.equal(asksForKeys(query), false, query);
  assert.equal(asksForKeys("tasten", ["Tastenkombinationen"]), true);
  assert.equal(asksForKeys("ta", ["Tastenkombinationen"]), false);
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

test("the tab step defaults give way to keys people already chose", () => {
  // Off macOS Mod is Control: an agent key saved as Mod+Tab is Ctrl+Tab.
  assert.equal(shortcutKeys("Mod+Tab", "win32"), shortcutKeys("Ctrl+Tab", "win32"));
  assert.notEqual(shortcutKeys("Mod+Tab", "darwin"), shortcutKeys("Ctrl+Tab", "darwin"));
  assert.equal(shortcutKeys("Shift+Ctrl+Tab", "linux"), shortcutKeys("ctrl+shift+tab", "linux"));
  const saved = { ...defaults.shortcuts, agentNext: "Mod+Tab", agentPrev: "Mod+Shift+Tab" };
  const withoutTabs = (value: Record<string, string>) =>
    Object.fromEntries(Object.entries(value).filter(([name]) => !name.startsWith("tab")));
  const before = withoutTabs(saved);
  // Saved before the tab steps existed: the defaults that clash are unset,
  // so the agent keys keep working and saving Settings is not refused.
  const migrated = migrateTabKeys(before, saved, "win32");
  assert.equal(migrated.tabNext, "");
  assert.equal(migrated.tabPrev, "");
  assert.equal(shortcutsClash(saved, "win32"), true);
  assert.equal(shortcutsClash(migrated, "win32"), false);
  // On macOS those agent keys are Command+Tab: no clash, nothing changes.
  assert.equal(migrateTabKeys(before, saved, "darwin"), saved);
  // A custom Ctrl+Tab for another action, on any platform.
  const custom = { ...defaults.shortcuts, agentNext: "Ctrl+Tab" };
  const chosen = withoutTabs(custom);
  assert.equal(migrateTabKeys(chosen, custom, "darwin").tabNext, "");
  assert.equal(migrateTabKeys(chosen, custom, "darwin").tabPrev, "Ctrl+Shift+Tab");
  // A tab key saved on purpose stays, and the defaults never clash.
  assert.equal(migrateTabKeys(custom, custom, "darwin"), custom);
  assert.equal(migrateTabKeys(undefined, defaults.shortcuts, "win32"), defaults.shortcuts);
  for (const platform of ["darwin", "win32", "linux"])
    assert.equal(shortcutsClash(defaults.shortcuts, platform), false);
  // Unset keys are no clash.
  assert.equal(shortcutsClash({ a: "", b: "" }, "darwin"), false);
});

test("the chat, sidebar and prompt keys clash with nothing on any platform", () => {
  for (const platform of ["darwin", "win32", "linux"]) {
    const shortcuts = defaultShortcuts(platform);
    assert.equal(shortcutsClash(shortcuts, platform), false, platform);
    // The file editor's "Add selection to context" is Mod+Shift+L, which off
    // macOS is the Control+Shift+L Focus sidebar takes on macOS.
    assert.notEqual(
      shortcutKeys(shortcuts.focusSidebar, platform),
      shortcutKeys("Mod+Shift+L", platform),
      platform,
    );
    // Control+Alt is AltGr on many keyboards, which types letters.
    assert.doesNotMatch(shortcuts.focusSidebar, /Ctrl\+Alt/);
  }
  assert.equal(defaultShortcuts("darwin").focusSidebar, "Ctrl+Shift+L");
  assert.equal(defaultShortcuts("linux").focusSidebar, "Alt+Shift+L");
  assert.equal(shortcutLabel("Alt+ArrowUp", "darwin"), "⌥ ↑");
  assert.equal(shortcutLabel("Mod+Shift+ArrowDown", "win32"), "Ctrl+Shift+↓");
  assert.equal(matchesShortcut({ key: "ArrowDown", metaKey: false, ctrlKey: false, altKey: true, shiftKey: false }, "Alt+ArrowDown", "linux"), true);
  assert.equal(matchesShortcut({ key: "ArrowDown", metaKey: true, ctrlKey: false, altKey: false, shiftKey: true }, "Mod+Shift+ArrowDown", "darwin"), true);
  assert.equal(matchesShortcut({ key: "ArrowDown", metaKey: true, ctrlKey: false, altKey: false, shiftKey: false }, "Mod+Shift+ArrowDown", "darwin"), false);
  // Saved before these keys existed: one already taken by another action
  // is left unset, and the rest arrive.
  const saved = { ...defaults.shortcuts, agentNext: "Alt+ArrowDown" };
  const before = Object.fromEntries(
    Object.entries(saved).filter(([name]) => !name.startsWith("chat") && !name.startsWith("prompt") && name !== "focusSidebar"),
  );
  const migrated = migrateTabKeys(before, saved, "darwin");
  assert.equal(migrated.chatNext, "");
  assert.equal(migrated.chatPrev, "Alt+ArrowUp");
  assert.equal(shortcutsClash(migrated, "darwin"), false);
});

test("Pick an element arrives unset when its key is already one the person has, and Settings still saves", () => {
  const before = (shortcuts: Record<string, string>) =>
    Object.fromEntries(Object.entries(shortcuts).filter(([name]) => name !== "pick"));
  // An optional key the person moved to ⇧⌘C before Pick existed.
  const moved = { ...defaultShortcuts("darwin"), chatPrev: "Mod+Shift+C" };
  const fromOptional = migrateTabKeys(before(moved), moved, "darwin");
  assert.equal(fromOptional.pick, "");
  assert.equal(fromOptional.chatPrev, "Mod+Shift+C");
  assert.equal(shortcutsClash(fromOptional, "darwin"), false);
  // A menu key, and off macOS an agent key, on Ctrl+Shift+C.
  const files = { ...defaultShortcuts("darwin"), files: "Mod+Shift+C" };
  assert.equal(migrateTabKeys(before(files), files, "darwin").pick, "");
  const agent = { ...defaultShortcuts("linux"), agentNext: "Ctrl+Shift+C" };
  assert.equal(migrateTabKeys(before(agent), agent, "linux").pick, "");
  // The person's own show-and-hide shortcut.
  const fresh = defaultShortcuts("darwin");
  assert.equal(migrateTabKeys(before(fresh), fresh, "darwin", "Mod+Shift+C").pick, "");
  assert.equal(migrateTabKeys(before(fresh), fresh, "darwin", "Alt+Space").pick, "Mod+Shift+C");
  // An optional default nobody chose does not keep Pick out.
  assert.equal(migrateTabKeys(before(fresh), fresh, "darwin").pick, "Mod+Shift+C");
  // Pick is a menu accelerator: a modifier and one key, or unset.
  assert.equal(shortcutAccepted("pick", "Mod+Shift+C"), true);
  assert.equal(shortcutAccepted("pick", ""), true);
  assert.equal(shortcutAccepted("pick", "Tab"), false);
  assert.equal(shortcutAccepted("pick", "Mod+ArrowUp"), false);
  assert.equal(shortcutAccepted("files", ""), false);
  assert.equal(shortcutAccepted("tabNext", "Ctrl+Tab"), true);
  assert.equal(shortcutAccepted("agentNext", "Tab"), true);
  for (const [name, value] of Object.entries(defaultShortcuts("darwin")))
    assert.equal(shortcutAccepted(name, value), true, name);
});

test("shortcut fields write Ctrl off macOS and save it as Mod, the same key there", () => {
  assert.equal(shortcutField("Mod+Alt+Shift+F8", "linux"), "Ctrl+Alt+Shift+F8");
  assert.equal(shortcutField("Mod+Alt+Shift+F8", "darwin"), "Mod+Alt+Shift+F8");
  assert.equal(shortcutField("Ctrl+Tab", "darwin"), "Ctrl+Tab");
  assert.equal(shortcutFromField("Ctrl+Shift+E", "win32"), "Mod+Shift+E");
  assert.equal(shortcutFromField("ctrl+k", "linux"), "Mod+k");
  assert.equal(shortcutFromField("Ctrl+Tab", "darwin"), "Ctrl+Tab");
});

test("the approvals key is Mod+Shift+A, clashes with nothing, and gives way to a key someone chose", () => {
  assert.equal(defaults.shortcuts.approvalNext, "Mod+Shift+A");
  assert.equal(shortcutAccepted("approvalNext", "Mod+Shift+A"), true);
  // It may be left unset, and set to a named key, as the agent keys may.
  assert.equal(shortcutAccepted("approvalNext", ""), true);
  assert.equal(shortcutAccepted("approvalNext", "Shift+Tab"), true);
  for (const platform of ["darwin", "win32", "linux"])
    assert.equal(shortcutsClash(defaultShortcuts(platform), platform), false, platform);
  // Saved before it existed, with Mod+Shift+A on another action: that stays,
  // and the new key is unset so Settings still saves.
  const saved = { ...defaults.shortcuts, palette: "Mod+Shift+A" };
  const before = Object.fromEntries(Object.entries(saved).filter(([name]) => name !== "approvalNext"));
  const migrated = migrateTabKeys(before, saved, "darwin");
  assert.equal(migrated.approvalNext, "");
  assert.equal(migrated.palette, "Mod+Shift+A");
  assert.equal(shortcutsClash(migrated, "darwin"), false);
  // It matches by the key, whatever Shift makes of the letter.
  assert.equal(matchesShortcut({ key: "A", code: "KeyA", metaKey: true, ctrlKey: false, altKey: false, shiftKey: true }, "Mod+Shift+A", "darwin"), true);
  assert.equal(matchesShortcut({ key: "A", code: "KeyA", metaKey: false, ctrlKey: true, altKey: false, shiftKey: true }, "Mod+Shift+A", "win32"), true);
  assert.equal(matchesShortcut({ key: "a", code: "KeyA", metaKey: true, ctrlKey: false, altKey: false, shiftKey: false }, "Mod+Shift+A", "darwin"), false);
});
