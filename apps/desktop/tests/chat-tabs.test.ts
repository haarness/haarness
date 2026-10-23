import test from "node:test";
import assert from "node:assert/strict";
import {
  archivedByReader,
  keptTabs,
  newlyArchived,
  openTab,
  sameTabs,
  savedTabs,
  tabOrderScope,
  tabsKey,
  takeSlot,
  unarchivedChats,
} from "../src/renderer/chat-tabs";

const threads = [
  { id: "one", archived: false },
  { id: "two", archived: false },
  { id: "old", archived: true },
];

test("saved chat tabs reopen in their saved order", () => {
  assert.deepEqual(savedTabs(["two", "one"], threads), ["two", "one"]);
});

test("tabs of archived, missing or repeated chats are not reopened", () => {
  assert.deepEqual(savedTabs(["one", "old", "gone", "one", "two"], threads), ["one", "two"]);
});

test("a damaged or absent record reopens no tabs", () => {
  for (const saved of [null, undefined, "one", { one: true }, [1, null, { id: "one" }]])
    assert.deepEqual(savedTabs(saved, threads), [], JSON.stringify(saved));
});

test("each project remembers its own tabs", () => {
  assert.notEqual(tabsKey("project-a"), tabsKey("project-b"));
  assert.match(tabsKey("project-a"), /^chat-tabs:v1:/);
});

test("a chat archived since the last listing is newly archived", () => {
  const before = unarchivedChats([
    { id: "one", archived: false },
    { id: "two", archived: false },
    { id: "old", archived: true },
  ]);
  assert.deepEqual([...before], ["one", "two"]);
  assert.deepEqual(
    newlyArchived(before, [
      { id: "one", archived: true },
      { id: "two", archived: false },
      { id: "old", archived: true },
    ]),
    ["one"],
  );
});

test("a project's first listing, or a chat first seen archived, archives nothing", () => {
  assert.deepEqual(newlyArchived(new Set(), threads), []);
  assert.deepEqual(newlyArchived(new Set(["one"]), [{ id: "new", archived: true }]), []);
});

test("an agent's own chat, archived when its work ends, keeps its tab", () => {
  const agent = { id: "agent", archived: true, isSubagent: true };
  assert.equal(archivedByReader(agent), false);
  assert.equal(archivedByReader({ id: "chat", archived: true }), true);
  assert.deepEqual(savedTabs(["agent", "one"], [...threads, agent]), ["agent", "one"]);
  const before = unarchivedChats([{ ...agent, archived: false }]);
  assert.deepEqual(newlyArchived(before, [agent]), []);
});

test("a record is rewritten only when it differs from the open tabs", () => {
  assert.equal(sameTabs(["one", "two"], ["one", "two"]), true);
  assert.equal(sameTabs(null, []), true);
  assert.equal(sameTabs(["one", "two"], ["two", "one"]), false);
  assert.equal(sameTabs(["one", "gone"], ["one"]), false);
  assert.equal(sameTabs("one", ["one"]), false);
});

test("a plain open takes the preview tab's place instead of adding a tab", () => {
  // The first preview joins the open tabs; the next one replaces it in place.
  assert.deepEqual(openTab(["kept"], "", "one", "preview"), { tabs: ["kept", "one"], preview: "one" });
  assert.deepEqual(openTab(["one", "kept"], "one", "two", "preview"), { tabs: ["two", "kept"], preview: "two" });
  // Choosing an open tab changes no tab, whatever it is.
  assert.deepEqual(openTab(["two", "kept"], "two", "kept", "preview"), { tabs: ["two", "kept"], preview: "two" });
  assert.deepEqual(openTab(["two", "kept"], "two", "two", undefined), { tabs: ["two", "kept"], preview: "two" });
  // A preview whose tab is gone is no preview any more.
  assert.deepEqual(openTab(["kept"], "gone", "three", "preview"), { tabs: ["kept", "three"], preview: "three" });
});

test("a kept open adds a tab of its own beside the preview, or keeps the preview it names", () => {
  assert.deepEqual(openTab(["one", "kept"], "one", "two", "keep"), { tabs: ["one", "kept", "two"], preview: "one" });
  assert.deepEqual(openTab(["one", "kept"], "one", "one", "keep"), { tabs: ["one", "kept"], preview: "" });
  // Any other open (New chat, a fork, a notification) adds a kept tab too.
  assert.deepEqual(openTab(["one"], "one", "fork", undefined), { tabs: ["one", "fork"], preview: "one" });
});

test("a location remembers every tab but its preview", () => {
  assert.deepEqual(keptTabs(["one", "preview", "two"], "preview"), ["one", "two"]);
  assert.deepEqual(keptTabs(["one", "two"], ""), ["one", "two"]);
});

test("the chat replacing a preview takes the preview's slot in a saved tab order", () => {
  assert.equal(tabOrderScope("project-a"), "list:session-tabs:project-a");
  assert.deepEqual(takeSlot(["b", "preview", "a"], "preview", "next"), ["b", "next", "a"]);
  // No saved order, no slot to take; a chat with a slot of its own keeps it.
  assert.equal(takeSlot(undefined, "preview", "next"), undefined);
  assert.equal(takeSlot(["b", "a"], "preview", "next"), undefined);
  assert.equal(takeSlot(["next", "preview"], "preview", "next"), undefined);
});
