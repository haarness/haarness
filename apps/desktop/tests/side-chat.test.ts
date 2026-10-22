import test from "node:test";
import assert from "node:assert/strict";
import {
  MAX_CHATS,
  MAX_HISTORY,
  MAX_STORED,
  MAX_THREADS,
  type SideChats,
  type SideThread,
  type SideTurn,
  btwQuestion,
  sideAdd,
  sideBusy,
  sideHistory,
  sideRecent,
  sideRemove,
  sideRestore,
  sideStored,
  sideTitle,
} from "../src/renderer/side-chat";

const turn = (question: string, state: SideTurn["state"] = "done", answer = "A: " + question): SideTurn => ({
  question,
  answer,
  state,
});
const thread = (id: string, turns: SideTurn[] = []): SideThread => ({ id, turns, draft: "", time: 0 });

test("a follow-up sends only the finished questions, the newest forty", () => {
  const turns = [turn("one"), turn("failed", "error", ""), turn("stopped", "stopped", "half"), turn("blank", "done", "  "), turn("two")];
  assert.deepEqual(sideHistory(turns), [
    { question: "one", answer: "A: one" },
    { question: "two", answer: "A: two" },
  ]);
  const many = Array.from({ length: MAX_HISTORY + 5 }, (_, at) => turn(`q${at}`));
  const sent = sideHistory(many);
  assert.equal(sent.length, MAX_HISTORY);
  assert.equal(sent[0].question, "q5");
});

test("a tab is named after its first question, shortened", () => {
  assert.equal(sideTitle(thread("a")), undefined);
  assert.equal(sideTitle(thread("a", [turn("  What   changed\nhere? ")])), "What changed here?");
  const long = sideTitle(thread("a", [turn("How does the session list decide what sorts first?")]))!;
  assert.equal(long.length, 32);
  assert.ok(long.endsWith("…"));
});

test("a /btw in the composer is a side question, a bare one opens the side chat", () => {
  assert.equal(btwQuestion("/btw why did it fail?"), "why did it fail?");
  assert.equal(btwQuestion("  /btw\n  and this?\n"), "and this?");
  assert.equal(btwQuestion("/btw"), "");
  assert.equal(btwQuestion("/btw   "), "");
  assert.equal(btwQuestion("/btwx"), undefined);
  assert.equal(btwQuestion("tell me /btw"), undefined);
});

test("new threads come forward and the oldest resting ones give way", () => {
  let chat: SideChats = { threads: [thread("busy", [turn("q", "pending", "")])] };
  for (let at = 0; at < MAX_THREADS + 2; at++) chat = sideAdd(chat, thread(`t${at}`));
  assert.equal(chat.threads.length, MAX_THREADS);
  assert.equal(chat.threads[0].id, "busy", "a thread still answering is never dropped");
  assert.equal(chat.front, `t${MAX_THREADS + 1}`);
  assert.ok(sideBusy(chat.threads[0]));
  assert.ok(!sideBusy(chat.threads[1]));
});

test("closing the thread in front brings its neighbour forward", () => {
  const chat = { threads: [thread("a"), thread("b"), thread("c")], front: "b" };
  assert.deepEqual(sideRemove(chat, "b"), { threads: [thread("a"), thread("c")], front: "c" });
  assert.equal(sideRemove({ threads: [thread("a"), thread("b")], front: "b" }, "b").front, "a");
  assert.equal(sideRemove(chat, "a").front, "b");
  assert.deepEqual(sideRemove({ threads: [thread("a")], front: "a" }, "a"), { threads: [], front: undefined });
});

test("stored side chats come back with a lost answer stopped and anything damaged left out", () => {
  const restored = sideRestore({
    front: "b",
    threads: [
      { id: "a", turns: [turn("q1"), turn("q2", "pending", "half"), { question: 1 }, null], draft: "next", time: 3 },
      { id: "b", turns: [] },
      { turns: [] },
      null,
    ],
  });
  assert.deepEqual(restored, {
    front: "b",
    threads: [
      { id: "a", turns: [turn("q1"), turn("q2", "stopped", "half")], draft: "next", time: 3 },
      { id: "b", turns: [], draft: "", time: 0 },
    ],
  });
  for (const damaged of [null, undefined, "x", { threads: "x" }]) assert.deepEqual(sideRestore(damaged), { threads: [] });
  assert.equal(sideRestore({ front: "gone", threads: [{ id: "a", turns: [] }] }).front, undefined);
});

test("what is stored stays within its size, the oldest questions going first", () => {
  const big = "x".repeat(200_000);
  const chat = {
    front: "new",
    threads: [thread("old", [turn("1", "done", big), turn("2", "done", big), turn("3", "done", big)]), thread("new", [turn("4", "done", big), turn("5", "done", big), turn("6", "done", big)])],
  };
  const stored = sideStored(chat);
  assert.ok(JSON.stringify(stored).length <= MAX_STORED);
  assert.equal(stored.threads.at(-1)!.turns.at(-1)!.question, "6");
  assert.equal(stored.threads[0].turns[0].question, "2");
  assert.equal(chat.threads[0].turns.length, 3, "the shown threads are untouched");
});

test("the most recently used chats keep their side chats", () => {
  const order = Array.from({ length: MAX_CHATS }, (_, at) => `c${at}`);
  const next = sideRecent(order, "new");
  assert.equal(next.order.length, MAX_CHATS);
  assert.equal(next.order[0], "new");
  assert.deepEqual(next.dropped, [`c${MAX_CHATS - 1}`]);
  assert.deepEqual(sideRecent(["a", "b"], "b"), { order: ["b", "a"], dropped: [] });
});
