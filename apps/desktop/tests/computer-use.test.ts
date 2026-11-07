import { test } from "node:test";
import assert from "node:assert/strict";
import type { PendingPermissionInfo, ToolCatalogEntry } from "@haarness/sdk";
import {
  barKeys,
  barWords,
  computerApproval,
  computerUseKeys,
  isComputerPermission,
  macDetail,
  pausedChatIn,
  pausedHere,
  shownText,
  validBundle,
  waitsOnScreen,
} from "../src/renderer/computer-use";
import { describeTool, toolActivity, toolInput } from "../src/renderer/transcript";
import { readToolImage, screenToolImage } from "../src/renderer/project-images";
import { canAutoApprove } from "../src/renderer/approval-mode";
import { toolGroups } from "../src/renderer/settings-lists";
import { dictionaries } from "../src/localization";

const root = "/Users/sam/work/app";
const english = (key: string, values: Record<string, string> = {}) =>
  key.replace(/\{(\w+)\}/g, (match, name) => values[name] ?? match);
const request = (mac: Record<string, unknown>, allowAlways = true): PendingPermissionInfo =>
  ({
    permissionId: "p1",
    permission: "computer",
    operation: "computer:act:t1:com.apple.TextEdit",
    patterns: ["act:t1:com.apple.TextEdit"],
    alwaysPatterns: ["computer:act:t1:com.apple.TextEdit"],
    allowAlways,
    explicit: true,
    detail: { ref: "e4", __mac: mac },
    threadId: "t1",
    rootThreadId: "t1",
    createdAt: new Date().toISOString(),
  }) as PendingPermissionInfo;

test("the bar's words go to the server in the interface language, with %@ where the app goes", () => {
  const words = barWords((key) => english(key));
  assert.equal(words.usingApp, "Haarness is working in %@");
  assert.equal(words.usingMac, "Haarness is working on this Mac");
  assert.deepEqual(Object.keys(words).sort(), Object.keys(barKeys).sort());
  // A translation that lost its placeholder would name no app: English stands in.
  assert.equal(barWords((key) => (key === barKeys.usingApp ? "Haarness arbeitet" : key)).usingApp, "Haarness is working in %@");
  assert.equal(barWords((key) => (key === barKeys.usingApp ? "{app} — Haarness" : key)).usingApp, "%@ — Haarness");
});

test("app rules take bundle identifiers and prefix rules only", () => {
  for (const value of ["com.apple.TextEdit", "com.jetbrains.*", "dev.zed.Zed-Preview", " org.mozilla.firefox "]) assert.ok(validBundle(value), value);
  for (const value of ["", "TextEdit app", "../etc", ".hidden", "com.*.app", "*", "com.apple.TextEdit;rm"]) assert.ok(!validBundle(value), value);
});

test("a computer use card names the app, shows the spot and offers this chat or every chat", () => {
  const preview = "data:image/jpeg;base64,/9j/AAAA";
  const card = computerApproval(
    request({ kind: "act", action: "click", app: "TextEdit", bundle: "com.apple.TextEdit", target: 'button "Save"', label: "Save", summary: 'Click button "Save" in TextEdit', preview }),
    english,
  );
  assert.equal(card.summary, "Click “Save” in TextEdit");
  assert.equal(card.preview, preview);
  assert.equal(card.inChat, "Allow in this chat");
  assert.deepEqual(card.always, { label: "Always allow TextEdit", bundle: "com.apple.TextEdit" });
  assert.match(card.alwaysHint!, /in any chat/);
  assert.deepEqual(card.notes, []);
  // An older server sends only its English line, which stands as it is.
  const older = computerApproval(request({ kind: "act", action: "click", app: "TextEdit", target: 'button "Save"', summary: 'Click button "Save" in TextEdit' }), english);
  assert.equal(older.summary, 'Click button "Save" in TextEdit');
  // Typed text shows on the card, its line breaks kept visible and its length bounded.
  const typed = computerApproval(request({ kind: "act", action: "type", app: "Notes", text: "one\ntwo", summary: "Type into text area in Notes" }), english);
  assert.equal(typed.text, "one ⏎ two");
  assert.equal(shownText("x".repeat(400)).length, 161);
  // Keys show when nothing is typed.
  assert.equal(computerApproval(request({ action: "press", app: "Notes", keys: "Meta+S" }), english).keys, "Meta+S");
});

test("the card's main line is said in the person's language from the request's parts", () => {
  const line = (mac: Record<string, unknown>) => computerApproval(request({ kind: "act", app: "Notes", label: null, ...mac }), english).summary;
  assert.equal(line({ action: "click" }), "Click the marked spot in Notes");
  assert.equal(line({ action: "double-click", label: "Inbox" }), "Double-click “Inbox” in Notes");
  assert.equal(line({ action: "scroll", direction: "up", label: "Rows" }), "Scroll up over “Rows” in Notes");
  assert.equal(line({ action: "press", keys: "Meta+S" }), "Press Meta+S in Notes");
  assert.equal(line({ action: "drag", label: "note.txt", endApp: "Finder" }), "Drag “note.txt” from Notes to Finder");
  assert.equal(line({ action: "drag", label: "note.txt", endApp: "Notes" }), "Drag “note.txt” in Notes");
  assert.equal(line({ kind: "launch", action: "quit" }), "Quit Notes");
  assert.equal(line({ kind: "launch", action: "launch" }), "Open Notes");
  // An action this build does not know keeps the server's words.
  assert.equal(line({ action: "squeeze", summary: "Squeeze it in Notes" }), "Squeeze it in Notes");
  const japanese = dictionaries.ja;
  const translate = (key: string, values?: Record<string, string>) =>
    Object.entries(values ?? {}).reduce((text, [name, value]) => text.replaceAll(`{${name}}`, value), japanese[key] ?? key);
  assert.equal(computerApproval(request({ kind: "act", action: "click", app: "メモ", label: "保存" }), translate).summary, "メモ で 「保存」 をクリック");
});

test("a request that asks every time offers nothing lasting and says why", () => {
  const console = computerApproval(request({ kind: "act", app: "Terminal", console: true, everyTime: true }, false), english);
  assert.equal(console.inChat, null);
  assert.match(console.notes[0], /every click on it needs your approval/);
  const ruled = computerApproval(request({ kind: "act", app: "Mail", everyTime: true }, false), english);
  assert.equal(ruled.inChat, null);
  assert.deepEqual(ruled.notes, ["You set this app to ask every time."]);
  // Even with Always on offer, an every-time request keeps to once.
  assert.equal(computerApproval(request({ kind: "act", app: "Mail", everyTime: true }, true), english).inChat, null);
});

/**
 * The server's own request for a drag from TextEdit, set to act without asking, into Notes (as the runtime test
 * `a_drag_into_another_app_asks_only_about_the_app_not_yet_allowed` pins it): only Notes is asked about.
 */
const dragIntoNotes = (alsoTextEdit = false): PendingPermissionInfo => {
  const chat = "01a1b4a2-8aa6-7801-8836-bd6474929036";
  const patterns = [...(alsoTextEdit ? [`act:${chat}:com.apple.TextEdit`] : []), `act:${chat}:com.apple.Notes`];
  return {
    explicit: true,
    permissionId: "01a1b4a2-8bf3-75b3-b1a3-5dc3ca3e3822",
    operation: `computer:${patterns[0]}`,
    detail: {
      from: { ref: "e1" },
      to: { ref: "e4" },
      __mac: {
        kind: "act",
        action: "drag",
        bundle: "com.apple.TextEdit",
        app: "TextEdit",
        target: 'button "Save"',
        label: "Save",
        direction: null,
        endBundle: "com.apple.Notes",
        endApp: "Notes",
        point: { x: 20, y: 15 },
        preview: "data:image/jpeg;base64,/9j/4AAQSkZJRgABA",
        text: null,
        keys: null,
        console: false,
        everyTime: false,
        running: null,
        asks: patterns,
        summary: 'Drag button "Save" from TextEdit to Notes',
      },
    },
    permission: "computer",
    patterns,
    alwaysPatterns: patterns.map((pattern) => `computer:${pattern}`),
    threadId: chat,
    rootThreadId: chat,
    turnId: "01a1b4a2-8aa7-7421-a3dc-dac4e4688be7",
    callId: "drag",
    agent: "build",
    createdAt: "2026-11-07T06:11:53.971429Z",
  } as PendingPermissionInfo;
};

test("a drag into another app names and allows the app it asks about, never the one it starts in", () => {
  const card = computerApproval(dragIntoNotes(), english);
  assert.equal(card.summary, "Drag “Save” from TextEdit to Notes");
  assert.deepEqual(card.always, { label: "Always allow Notes", bundle: "com.apple.Notes" });
  assert.match(card.alwaysHint!, /using Notes again/);
  assert.equal(card.inChat, "Allow in this chat");
  // Asked about both apps, one rule would leave the other out: nothing lasting, and the chat-wide answer names neither.
  const both = computerApproval(dragIntoNotes(true), english);
  assert.equal(both.always, null);
  assert.equal(both.inChat, "Allow in this chat");
  // Within one app, the app it acts in, as before.
  const within = computerApproval(request({ kind: "act", action: "drag", app: "Notes", bundle: "com.apple.Notes", endApp: "Notes", endBundle: "com.apple.Notes", label: "note" }), english);
  assert.deepEqual(within.always, { label: "Always allow Notes", bundle: "com.apple.Notes" });
});

test("a pause is shown in the chat that holds the screen and in the agents it started, from any answer that names it", () => {
  assert.equal(pausedChatIn({ title: "Computer use", data: { supported: true, paused: true, pausedChat: "root" } }), "root");
  for (const answer of [{ data: { paused: false, pausedChat: null } }, { data: {} }, null, "root"]) assert.equal(pausedChatIn(answer), undefined);
  const threads = [{ id: "root" }, { id: "child", parentId: "root" }, { id: "grandchild", parentId: "child" }, { id: "other" }];
  assert.ok(pausedHere("root", "root", threads));
  assert.ok(pausedHere("root", "grandchild", threads));
  assert.ok(!pausedHere("root", "other", threads));
  assert.ok(!pausedHere(undefined, "root", threads));
  assert.ok(!pausedHere("root", "", threads));
  // A parent loop in bad data ends the walk.
  assert.ok(!pausedHere("root", "a", [{ id: "a", parentId: "b" }, { id: "b", parentId: "a" }]));
});

test("a pause holds the line only while the turn waits on the screen", () => {
  const call = (id: string, name: string, turnId = "t1") =>
    ({ turnId, data: { type: "tool_call", call_id: id, name, arguments: {} } }) as const;
  const done = (id: string, turnId = "t1") =>
    ({ turnId, data: { type: "tool_result", call_id: id, name: "", output: "", is_error: false } }) as const;
  // Typing that the strip holds.
  assert.ok(waitsOnScreen([call("a", "mac_type")], "t1"));
  assert.ok(waitsOnScreen([call("a", "functions.mac_click")], "t1"));
  // It looked earlier and now runs a command, edits or writes: its own work shows.
  assert.ok(!waitsOnScreen([call("a", "mac_snapshot"), done("a"), call("b", "bash")], "t1"));
  assert.ok(!waitsOnScreen([call("a", "mac_snapshot"), done("a")], "t1"));
  assert.ok(!waitsOnScreen([], "t1"));
  // Another turn's step says nothing about this one.
  assert.ok(!waitsOnScreen([call("a", "mac_type", "t0")], "t1"));
  // A step whose approval is still open waits for the person's answer, not for the screen.
  assert.ok(!waitsOnScreen([call("a", "mac_click")], "t1", ["a"]));
  assert.ok(waitsOnScreen([call("a", "mac_click")], "t1", [null, "z"]));
});

test("a whole-screen look says what goes to the model and shows no spot", () => {
  const card = computerApproval(request({ kind: "screen", preview: "data:image/jpeg;base64,AAAA" }), english);
  assert.equal(card.summary, "Look at every app's windows");
  assert.equal(card.preview, null);
  assert.equal(card.inChat, "Allow in this chat");
  assert.match(card.notes[0], /apart from password managers and Haarness/);
});

test("only a picture the server made is shown on a card", () => {
  assert.equal(macDetail({ __mac: { preview: "https://example.test/spot.jpg" } }).preview, null);
  assert.equal(macDetail({ __mac: { preview: "data:image/svg+xml;base64,PHN2Zy8+" } }).preview, null);
  assert.equal(macDetail({ __mac: { preview: "data:image/png;base64,iVBORw0KGgo=" } }).preview, "data:image/png;base64,iVBORw0KGgo=");
  assert.deepEqual(macDetail(undefined).kind, "act");
});

test("computer use requests are a person's to answer, never Desktop's own Auto", () => {
  assert.ok(isComputerPermission(request({})));
  assert.equal(canAutoApprove(request({})), false);
  // Even from a server that forgot to mark it explicit.
  assert.equal(canAutoApprove({ ...request({}), explicit: false, allowAlways: true }), false);
  assert.ok(!isComputerPermission({ permission: "bash", operation: "bash:ls" }));
});

test("computer use rows read like a person's steps, with the element the result names", () => {
  const clicked = describeTool("mac_click", { ref: "e12" }, { app: "TextEdit", mac: { app: "TextEdit", target: 'button "Save"' } });
  assert.equal(clicked.verb, "Clicked");
  assert.equal(clicked.attempt, "Tried to click");
  assert.equal(clicked.target, 'button "Save" in TextEdit');
  assert.equal(clicked.icon, "host");
  // Before the result, a ref alone names nothing a reader knows.
  assert.equal(describeTool("mac_click", { ref: "e12" }).target, "");
  assert.equal(describeTool("mac_click", { ref: "e1", count: 2 }).verb, "Double-clicked");
  assert.equal(describeTool("mac_click", { ref: "e1", button: "right" }).verb, "Right-clicked");
  assert.equal(describeTool("mac_snapshot", { app: "com.apple.TextEdit" }, { app: "TextEdit" }).target, "TextEdit");
  assert.equal(describeTool("mac_snapshot", {}).verb, "Viewed the screen");
  assert.equal(describeTool("mac_apps", { action: "list" }).verb, "Listed apps");
  assert.deepEqual(
    [describeTool("mac_apps", { action: "launch", app: "TextEdit" }).verb, describeTool("mac_apps", { action: "launch", app: "TextEdit" }).target],
    ["Opened", "TextEdit"],
  );
  assert.equal(describeTool("mac_apps", { action: "quit", app: "TextEdit" }).verb, "Quit");
  assert.equal(describeTool("mac_type", { text: "hello" }).target, "“hello”");
  assert.equal(describeTool("mac_press", { key: "Meta+S" }).target, "Meta+S");
  assert.equal(describeTool("mac_scroll", { direction: "up", ref: "e2" }, { app: "Notes" }).target, "up in Notes");
  assert.equal(describeTool("mac_drag", { from: {}, to: {} }).verb, "Dragged");
  assert.equal(describeTool("mac_hover", { ref: "e3" }, { mac: { target: "menu item" } }).verb, "Pointed at");
  assert.equal(toolActivity("mac_click", { ref: "e1" }), "Using this Mac");
  // What Haarness added to resolve a call is not shown as its input.
  assert.doesNotMatch(toolInput("mac_click", { ref: "e1", __mac: { asks: ["x"] } }).text, /Mac|asks/);
});

test("a computer use row shows the screenshot the server saved in the project", () => {
  const done = (attachments: unknown[], is_error = false) => ({ is_error, metadata: { attachments, app: "TextEdit" } });
  const shot = `${root}/.haarness-chat-attachments/mac-0190/screen.jpg`;
  assert.deepEqual(screenToolImage(done([shot]), root), { path: ".haarness-chat-attachments/mac-0190/screen.jpg" });
  assert.deepEqual(readToolImage("mac_snapshot", done([shot]), root), { path: ".haarness-chat-attachments/mac-0190/screen.jpg" });
  assert.deepEqual(readToolImage("mac_click", done([shot]), root), { path: ".haarness-chat-attachments/mac-0190/screen.jpg" });
  assert.equal(readToolImage("mac_click", done([shot], true), root), undefined);
  assert.equal(readToolImage("mac_click", done([]), root), undefined);
  assert.equal(readToolImage("mac_snapshot", done(["/tmp/screen.jpg"]), root), undefined);
  assert.equal(readToolImage("mac_snapshot", done(["data:image/jpeg;base64,AAAA"]), root), undefined);
});

test("Settings › Tools lists the computer use tools as their own part", () => {
  const tool = (id: string, group = "computer", groupTitle = "Computer use"): ToolCatalogEntry => ({
    id,
    source: "native",
    description: "",
    advertised: true,
    enabled: true,
    essential: true,
    estimatedTokens: 10,
    group,
    groupTitle,
    loaded: false,
  });
  const [builtin] = toolGroups([
    ...["snapshot", "apps", "click", "type", "press", "scroll", "drag", "hover"].map((name) => tool("mac_" + name)),
    tool("bash", "core", "Core"),
  ]);
  const part = builtin.groups?.find((group) => group.key === "builtin/computer");
  assert.equal(part?.name, "Computer use");
  assert.equal(part?.tools.length, 8);
});

test("every computer use word Desktop shows is in the catalog", () => {
  assert.deepEqual(
    computerUseKeys.filter((key) => !Object.hasOwn(dictionaries.en, key)),
    [],
  );
});
