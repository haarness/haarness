import test from "node:test";
import { cursorLabelKeys } from "@haarness/browser-driver/agent-cursor";
import { errorTextKeys } from "../src/renderer/error-text";
import { activityKeys, toolVerbKey, toolVerbKeys } from "../src/renderer/transcript";
import assert from "node:assert/strict";
import { readdir, readFile } from "node:fs/promises";
import { defaults } from "../src/shared";
import { languages, dictionaries, displayLanguage, parseLanguage, translate } from "../src/localization";
import { contextKinds } from "../src/renderer/context-usage";
import { themes } from "../src/themes";
import { settingsControls, settingsSections, settingsTitles, shortcutTitles } from "../src/renderer/settings-catalog";

test("English is the explicit default and invalid preferences recover to English", () => {
  assert.equal(defaults.language, "en");
  for (const value of [undefined, null, "", "xx", "constructor", "__proto__", 1])
    assert.equal(displayLanguage(value), "en");
  assert.equal(parseLanguage("ja_JP.UTF-8"), "ja");
  assert.equal(parseLanguage("zh_Hant_TW"), "zh-TW");
  assert.equal(parseLanguage("zh-HK"), "zh-TW");
  assert.equal(parseLanguage("zh-Hans"), "zh-CN");
  assert.equal(parseLanguage("pt-PT"), "pt-BR");
  assert.equal(parseLanguage("nn-NO"), "nb");
});

test("all 17 shared catalogs cover the same interface keys", () => {
  assert.equal(languages.length, 17);
  for (const { code, label, direction } of languages) {
    assert.equal(parseLanguage(code), code);
    assert.ok(label.length);
    assert.equal(direction, code === "ar" ? "rtl" : "ltr");
    assert.deepEqual(Object.keys(dictionaries[code]).sort(), Object.keys(dictionaries.en).sort(), code);
    for (const [key, text] of Object.entries(dictionaries[code])) {
      assert.ok(text.trim(), `${code}: ${key}`);
      assert.equal(translate(code, key), text);
    }
    assert.equal(translate(code, "untranslated diagnostic"), "untranslated diagnostic");
    assert.equal(translate(code, "__proto__"), "__proto__");
  }
  assert.equal(translate("ja", "Settings"), "設定");
  assert.notEqual(translate("zh-CN", "Language"), translate("zh-TW", "Language"));
});


test("translations preserve every interpolation field in every language", () => {
  const keys = Object.keys(dictionaries.en).filter((key) => /\{\w+\}/.test(key));
  assert.ok(keys.length > 10);
  for (const key of keys) {
    const fields = key.match(/\{\w+\}/g)!.sort();
    for (const { code } of languages)
      assert.deepEqual(dictionaries[code][key].match(/\{\w+\}/g)?.sort(), fields, `${code}: ${key}`);
  }
});

// A literal that never reaches a catalog shows English in all 17 languages, so
// the renderer's own t() keys are checked here rather than found in the UI.
// Native menus and dialogs are translated in the main process through `say()`
// (and the menu's own `t()`), so their literals get the same check.
test("every main-process dialog and menu key is in the shared catalog", async () => {
  const missing = new Set<string>();
  for (const name of ["index.ts", "browser-data.ts", "store.ts", "notifications.ts"]) {
    const source = await readFile(new URL(`../src/main/${name}`, import.meta.url), "utf8");
    for (const [, key] of source.matchAll(/\b(?:say|t)\(\s*"((?:[^"\\]|\\.)*)"/g))
      if (!Object.hasOwn(dictionaries.en, key)) missing.add(`${name}: ${key}`);
    for (const [, key] of source.matchAll(/\btranslate\([^,]+,\s*"((?:[^"\\]|\\.)*)"/g))
      if (!Object.hasOwn(dictionaries.en, key)) missing.add(`${name}: ${key}`);
  }
  assert.deepEqual([...missing], []);
});

test("every renderer t() key is in the shared catalog", async () => {
  const directory = new URL("../src/renderer/", import.meta.url);
  const names = (await readdir(directory)).filter((name) => /\.tsx?$/.test(name));
  const missing = new Set<string>();
  for (const name of names) {
    const source = await readFile(new URL(name, directory), "utf8");
    for (const [, key] of source.matchAll(/\bt\(\s*"((?:[^"\\]|\\.)*)"/g))
      if (!Object.hasOwn(dictionaries.en, key)) missing.add(`${name}: ${key}`);
    // SettingRow translates the literal title and description it is handed.
    for (const [row] of source.matchAll(/<(?:Row|SettingRow)\b[^>]*>/g))
      for (const [, key] of row.matchAll(/\b(?:title|description)="([^"]*)"/g))
        if (!Object.hasOwn(dictionaries.en, key)) missing.add(`${name}: ${key}`);
    // Menu items, select options and form choices are labelled from literals
    // that their render site or the shared form dialog translates.
    for (const [, key] of source.matchAll(/\blabel:\s*"((?:[^"\\]|\\.)*)"/g))
      if (!Object.hasOwn(dictionaries.en, key)) missing.add(`${name}: ${key}`);
  }
  assert.deepEqual([...missing], []);
});

// Text set on a DOM node by hand never passes a render site that translates
// it (the fenced-code Copy button showed English in every language).
test("renderer text set imperatively goes through t()", async () => {
  const directory = new URL("../src/renderer/", import.meta.url);
  const names = (await readdir(directory)).filter((name) => /\.tsx?$/.test(name));
  const literal = /(?:setAttribute\(\s*"(?:aria-label|aria-description|title|placeholder)",\s*|\.(?:title|textContent|innerText|placeholder|ariaLabel)\s*=\s*)"[^"]*[A-Za-z][^"]*"/g;
  const found: string[] = [];
  for (const name of names) {
    const source = await readFile(new URL(name, directory), "utf8");
    for (const [match] of source.matchAll(literal)) found.push(`${name}: ${match}`);
  }
  assert.deepEqual(found, []);
});

// `t(key).replace("{name}", value)` reads `$&`, `$'` and `$$` in the value as
// replacement patterns, so a provider named "A $& B" would show the key's
// text; `t(key, { name })` inserts values as they are.
test("translated sentences take their values through t(), not a string replace", async () => {
  const directory = new URL("../src/renderer/", import.meta.url);
  const names = (await readdir(directory)).filter((name) => /\.tsx?$/.test(name));
  const found: string[] = [];
  for (const name of names) {
    const source = await readFile(new URL(name, directory), "utf8");
    for (const [call] of source.matchAll(/\bt\(\s*"(?:[^"\\]|\\.)*"\s*\)\s*\.replace(?:All)?\(\s*"\{\w+\}"\s*,\s*(?!\()[^)]*/g))
      found.push(`${name}: ${call}`);
  }
  assert.deepEqual(found, []);
});

// Keys the renderer reaches through a variable, which the scan above cannot
// see: search-result groups, the panel launcher, the starter cards, and the
// labels that shared modules hand back for the renderer to translate.
test("keys translated through a variable are in the catalog", () => {
  const dynamic = [
    "Chats", "Archived chats", "Projects", "Worktrees", "Files", "Settings",
    "Commands", "Shortcuts", "Chat actions", "Project panels",
    "Editor actions", "Review actions", "Terminal actions", "Terminals",
    "Open files", "Slash commands", "Window", "Worktree actions",
    // Keyboard shortcut names and notes, from `shortcutTitles` and
    // `shortcutDescriptions`.
    "Next agent", "Previous agent", "While the message box has focus",
    "Browser", "Review", "Terminal", "Context",
    "Browse and test websites with Haarness", "Browse and edit project files",
    "Inspect changes and compare diffs", "Run commands in this project",
    "Inspect chat tokens, cache and context",
    "Explore code", "Explore and understand code",
    "Explain the architecture of this project",
    "Build a feature", "Build a new feature, app, or tool",
    "Help me build a new feature: ",
    "Review changes", "Review code and suggest changes",
    "Review the current changes",
    "Fix a problem", "Fix issues and failures",
    "Help me investigate and fix this issue: ",
    "This computer", "Project folder", "Worktree",
    // Settings copy for agents, commands and skills, from `assetCopy`.
    "Agents are assistants with their own instructions, model and tools. Switch between them in the composer.",
    "No agents yet", "Create one for a role you repeat, like a reviewer or a release helper.",
    "Commands are saved prompts you run with / in any chat.",
    "No commands yet", "Save a prompt you use often, then run it with /name.",
    "Skills are instructions and files an agent loads when a task calls for them.",
    "No skills yet", "Teach Haarness a workflow once, and it will use it whenever it fits.",
    // Cloud session and subscription states in the account dialog.
    "Preparing…", "Starting…", "Running", "Stopping…", "Stopped", "Failed", "Deleting…",
    "Active", "Trial", "Payment due", "Canceled", "Incomplete", "Paused",
    // Server switcher labels and Servers settings descriptions.
    "Haarness Cloud", "Cloud sessions", "Local projects",
    "Cloud sessions in your Haarness account", "Local projects on this computer",
    // The palette's count picks its singular or plural key.
    "{count} result",
    // Refusals of a screenshot pick, translated in the main process.
    "The screenshots changed. Open Screenshots again.",
    "Choose up to 20 screenshots from the list.",
    // Browser settings row, handed to SettingRow.
    "Show the agent’s cursor",
    "See where Haarness points, clicks and types while it controls the browser. Turning it off makes browser actions a little faster.",
    // The agent cursor's labels, translated in the main process.
    ...cursorLabelKeys,
    // Plain copy that stands in for transport and port failures.
    ...errorTextKeys,
    "signed in", "API key", "from the environment",
    // Custom-provider API formats, kept as English keys in the `protocols`
    // constant and translated at the render site.
    "OpenAI Chat Completions", "OpenAI Responses", "Anthropic Messages",
    "Google Gemini", "Azure OpenAI", "Google Vertex AI", "Amazon Bedrock",
    // Browser permission decisions, from the `decisions` constant.
    "Always allow", "Requires approval", "Block",
    // Browser setting rows: SettingRow translates the title and description
    // it is handed, so these never appear as literals inside t().
    "Agent browser control",
    "Allow Haarness to inspect and control this project’s browser from Desktop or an attached TUI. Manual browsing remains available.",
    "Web link destination",
    "Where web links in chats open. Applies to this desktop.",
    "Local URL destination",
    "Where localhost and loopback links open. Haarness uses the connected server’s network.",
    "Show full URL",
    "Show paths, queries, and fragments in the address bar. Editing always shows the complete URL.",
    "Browsing data",
    "Clear this project’s tabs, cookies, site storage, cache, history, and retained downloads on this desktop. Saved files and chat attachments remain.",
    "Browsing history",
    "View, search, reopen, and remove pages visited in this project’s browser.",
    "Ask where to save downloads",
    "Show a save dialog when a browser download completes. Otherwise, save it later from download history.",
    "Download history",
    "Manage the last 200 retained downloads for this project. Removing an entry removes its retained copy, but keeps files saved separately.",
    // Per-cell names for the browser permission table, chosen by field.
    "Default browsing", "Default downloads", "Default uploads",
    "{origin} browsing", "{origin} downloads", "{origin} uploads",
    // Goal controls and their confirmations, from the `goalActions` and
    // `goalConfirmations` constants in Manager.tsx.
    "Pause", "Resume", "Complete", "Block", "Clear",
    "Clear goal?", "Complete goal?", "Block goal?",
    // Insight breakdown headings, from the `insightGroups` constant.
    "Models", "Projects", "Tools", "Days",
    // Workflow-run and goal statuses, translated from the server value.
    "running", "completed", "failed", "cancelled", "paused", "interrupted",
    "active", "blocked", "usage_limited", "budget_limited", "complete",
    "cleared",
    // Context panel rows, from `contextKinds` and the usage `metrics`.
    ...contextKinds,
    "Input tokens", "Output tokens", "Cache read tokens", "Cache write tokens",
    // Notification switches, from their title/description tuples.
    "Permission notifications", "Notify when approval is needed while the app is in the background.",
    "Question notifications", "Notify when input is needed while the app is in the background.",
    // Custom definition removal, from `assetCopy`.
    "Remove this custom agent from this project?", "Remove this custom agent from all projects?",
    "Remove this custom command from this project?", "Remove this custom command from all projects?",
    "Remove this custom skill from this project?", "Remove this custom skill from all projects?",
    // MCP and profile catalog menus, labelled from their item lists.
    "Enable", "Disable", "Remove", "Sign out",
    "List profiles", "Upload profile", "View profile", "Download profile", "Restore profile", "Delete profile",
    // Fixed select choices: the form dialog translates their labels and keeps the values.
    "Global", "Project", "both", "global", "local",
    "Keep (refuse reset if dirty)", "Discard tracked changes", "Discard tracked and untracked changes",
    // Plural pairs and side-specific names chosen inside one t() call.
    "{count} file", "{count} files", "Reset {name}", "Remove {name}",
    "Comment on original line {line}", "Comment on updated line {line}",
    "Archive chat {title}", "Restore chat {title}",
    // Review file status words from the server's diff.
    "added", "deleted", "modified",
    // Reasoning level names from `reasoning.ts`, and the sidebar's waiting badges from state.tsx.
    "Model default", "None", "Minimal", "Low", "Medium", "High", "Extra high", "Max",
    "Needs approval", "Needs an answer",
    // Theme names and descriptions from `themes.ts`, shown on the swatch.
    ...themes.map((theme) => theme.name),
    ...themes.map((theme) => theme.description),
    // Tool rows, the activity line and batch labels, from `transcript.ts`.
    ...toolVerbKeys.map(toolVerbKey),
    ...activityKeys,
    "Working through several steps", "Using {tool}",
    "Shell", "File", "Files", "Search", "Web page", "Code", "Tool",
    // Built-in server commands, whose descriptions Desktop translates.
    "Browse and manage durable workflows", "Set or manage a persistent autonomous goal",
    // Settings › General switches, from their title/description tuples.
    "Task notifications", "Enable desktop notifications. Choose which alerts appear below.",
    "Compact tool activity", "Keep commands and tool output folded until you open them.",
    "Show reasoning", "Display the reasoning supplied by your selected model.",
    "Wrap editor lines", "Keep long lines within the editor width.",
    // Desktop notification titles, chosen by event in `notifications.ts`.
    "Task finished", "Task failed", "Approval needed", "Answer needed",
    // Plain readings of an "always" rule's catch-all, from `everyRequest`.
    "Every command", "Every file edit", "Every file read", "Every web page", "Every workflow",
    // Helper labels and built-in slash command descriptions.
    "Open providers", "Use an API key",
    // `historyCommands` (conversation-controls.ts imports the app state, so its
    // three descriptions are listed here).
    "Undo the last turn and restore its prompt", "Restore the next undone turn",
    "Summarize this chat with its model",
    // Settings navigation and search titles.
    ...Object.values(settingsTitles),
    ...settingsSections.map(([section]) => section),
    ...settingsControls.map(([, title]) => title),
    ...Object.values(shortcutTitles),
  ];
  assert.deepEqual(
    dynamic.filter((key) => !Object.hasOwn(dictionaries.en, key)),
    [],
  );
});
