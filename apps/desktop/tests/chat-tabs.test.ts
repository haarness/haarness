import test from "node:test";
import assert from "node:assert/strict";
import { archivedByReader, newlyArchived, sameTabs, savedTabs, tabsKey, unarchivedChats } from "../src/renderer/chat-tabs";

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
