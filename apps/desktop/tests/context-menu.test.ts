import { test } from "node:test";
import { execFile } from "node:child_process";
import assert from "node:assert/strict";
import type { MenuItemConstructorOptions } from "electron";
import { contextMenuItems, systemSpellings, terminalMenuItems, type ContextMenuPlace } from "../src/main/context-menu";
import { dictionaries, translate } from "../src/localization";

const PAGE = "file:///Applications/Haarness.app/Contents/Resources/app.asar/out/renderer/index.html";
const none = {
  canUndo: false,
  canRedo: false,
  canCut: false,
  canCopy: false,
  canPaste: false,
  canDelete: false,
  canSelectAll: false,
  canEditRichly: false,
};

const place = (extra: Partial<ContextMenuPlace> = {}): ContextMenuPlace => ({
  isEditable: false,
  editFlags: none,
  selectionText: "",
  linkURL: "",
  pageURL: PAGE,
  misspelledWord: "",
  dictionarySuggestions: [],
  ...extra,
});

function menu(value: ContextMenuPlace, language = "en") {
  const done: string[] = [];
  const items = contextMenuItems(value, {
    t: (key) => translate(language, key),
    replace: (word) => done.push("replace " + word),
    learn: (word) => done.push("learn " + word),
    copy: (text) => done.push("copy " + text),
  });
  /** Each item as one line: a separator, a role's name or a label. */
  const lines = items.map((item) =>
    item.type === "separator" ? "-" : item.role ? `${item.label} (${item.role})` : item.label!,
  );
  const click = (label: string) => {
    const item = items.find((entry) => entry.label === label) as MenuItemConstructorOptions;
    (item.click as () => void)();
  };
  return { items, lines, done, click };
}

test("nothing on the plain background, a button or a row", () => {
  assert.deepEqual(menu(place()).lines, []);
  // A drag that selected nothing but spaces is not text to copy.
  assert.deepEqual(menu(place({ selectionText: "  \n", editFlags: { ...none, canCopy: true } })).lines, []);
});

test("a text field offers the edit commands it allows, as Electron's roles", () => {
  const all = { ...none, canCut: true, canCopy: true, canPaste: true, canSelectAll: true };
  assert.deepEqual(menu(place({ isEditable: true, editFlags: all, selectionText: "draft" })).lines, [
    "Cut (cut)",
    "Copy (copy)",
    "Paste (paste)",
    "Select all (selectAll)",
  ]);
  // Nothing selected: nothing to cut or copy.
  assert.deepEqual(menu(place({ isEditable: true, editFlags: { ...none, canPaste: true, canSelectAll: true } })).lines, [
    "Paste (paste)",
    "Select all (selectAll)",
  ]);
  // A password field never gives its text away.
  assert.deepEqual(
    menu(place({ isEditable: true, selectionText: "secret", editFlags: { ...none, canPaste: true, canSelectAll: true } })).lines,
    ["Paste (paste)", "Select all (selectAll)"],
  );
  // A field that allows nothing opens no menu.
  assert.deepEqual(menu(place({ isEditable: true })).lines, []);
});

test("selected text outside a field offers Copy alone", () => {
  assert.deepEqual(menu(place({ selectionText: "reply text", editFlags: { ...none, canCopy: true, canSelectAll: true } })).lines, [
    "Copy (copy)",
  ]);
});

test("a misspelled word offers up to five spellings, then Add to dictionary, above the edit commands", () => {
  const value = menu(
    place({
      isEditable: true,
      misspelledWord: "teh",
      dictionarySuggestions: ["the", "ten", "tea", "tech", "tel", "eh"],
      editFlags: { ...none, canPaste: true, canSelectAll: true },
    }),
  );
  assert.deepEqual(value.lines, ["the", "ten", "tea", "tech", "tel", "Add to dictionary", "-", "Paste (paste)", "Select all (selectAll)"]);
  value.click("ten");
  value.click("Add to dictionary");
  assert.deepEqual(value.done, ["replace ten", "learn teh"]);
  // No spelling to suggest: the word can still be added.
  assert.deepEqual(menu(place({ isEditable: true, misspelledWord: "Haarness" })).lines, ["Add to dictionary"]);
});

test("a web link offers Copy link, which copies its address", () => {
  const value = menu(place({ linkURL: "https://haarness.ai/docs?q=1" }));
  assert.deepEqual(value.lines, ["Copy link"]);
  value.click("Copy link");
  assert.deepEqual(value.done, ["copy https://haarness.ai/docs?q=1"]);
  assert.deepEqual(menu(place({ linkURL: "mailto:sam@example.com" })).lines, ["Copy link"]);
  // Selected words of a link: both.
  assert.deepEqual(
    menu(place({ linkURL: "https://haarness.ai/", selectionText: "Haarness", editFlags: { ...none, canCopy: true } })).lines,
    ["Copy link", "-", "Copy (copy)"],
  );
});

test("a link to a project file, or an address the app refused, has no address to copy", () => {
  // Relative and absolute paths come back resolved against the app's page.
  for (const linkURL of [
    "file:///Applications/Haarness.app/Contents/Resources/app.asar/out/renderer/src/app.ts",
    "file:///Users/sam/project/src/app.ts",
    "unsupported:",
    "not a url",
  ])
    assert.deepEqual(menu(place({ linkURL })).lines, [], linkURL);
  // A development build's page is served over HTTP.
  assert.deepEqual(
    menu(place({ pageURL: "http://localhost:5173/", linkURL: "http://localhost:5173/src/app.ts" })).lines,
    [],
  );
});

test("the menu speaks the interface language", () => {
  assert.equal(dictionaries.ja["Add to dictionary"], "辞書に追加");
  const value = menu(
    place({ isEditable: true, misspelledWord: "teh", selectionText: "teh", editFlags: { ...none, canCut: true, canCopy: true } }),
    "ja",
  );
  assert.deepEqual(value.lines, ["辞書に追加", "-", "切り取り (cut)", "コピー (copy)"]);
});

test("on macOS the system's own spellings are asked for, with the word as an argument", async () => {
  const calls: { file: string; args: string[]; options: { timeout?: number } }[] = [];
  const answer = (result: { error?: Error; stdout?: string }) =>
    ((file: string, args: string[], options: { timeout?: number }, done: (error: Error | null, stdout: string) => void) => {
      calls.push({ file, args, options });
      done(result.error ?? null, result.stdout ?? "");
    }) as unknown as Parameters<typeof systemSpellings>[1];
  assert.deepEqual(await systemSpellings("teh", answer({ stdout: "the\ntbh\nten\ntea\ntej\nfeh\nyeh\n" })), ["the", "tbh", "ten", "tea", "tej"]);
  const [call] = calls;
  assert.equal(call.file, "/usr/bin/osascript");
  assert.deepEqual(call.args.slice(0, 3), ["-l", "JavaScript", "-e"]);
  // The word is the script's argument, never part of its text.
  assert.equal(call.args[4], "teh");
  assert.doesNotMatch(call.args[3], /teh/);
  assert.equal(call.options.timeout, 1000);
  // No answer in time, or none at all: no spellings, and the menu still offers Add to dictionary.
  assert.deepEqual(await systemSpellings("teh", answer({ error: new Error("timed out") })), []);
  assert.deepEqual(await systemSpellings("qzxv", answer({ stdout: "\n" })), []);
  assert.deepEqual(await systemSpellings("-e", answer({ stdout: "e\n" })), []);
  assert.equal(calls.length, 3);
});

test("the system spelling checker answers the script", { skip: process.platform !== "darwin" }, async () => {
  // Given time to start, as a cold machine may need.
  const patient = ((file: string, args: string[], options: object, done: never) =>
    execFile(file, args, { ...options, timeout: 15000 }, done)) as unknown as Parameters<typeof systemSpellings>[1];
  const spellings = await systemSpellings("teh", patient);
  assert.ok(spellings.includes("the"), spellings.join(", "));
  assert.ok(spellings.length <= 5);
});

test("the terminal's menu offers Copy for its selection, then Paste", () => {
  const done: string[] = [];
  const actions = { t: (key: string) => translate("en", key), copy: (text: string) => done.push(text) };
  const lines = (items: MenuItemConstructorOptions[]) => items.map((item) => (item.role ? `${item.label} (${item.role})` : item.label));
  assert.deepEqual(lines(terminalMenuItems("", actions)), ["Paste (paste)"]);
  const items = terminalMenuItems("npm test\n  src/app.ts", actions);
  assert.deepEqual(lines(items), ["Copy", "Paste (paste)"]);
  (items[0].click as () => void)();
  assert.deepEqual(done, ["npm test\n  src/app.ts"]);
});
