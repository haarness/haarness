import test from "node:test";
import assert from "node:assert/strict";
import {
  HISTORY_BLOCK,
  HISTORY_SIZE,
  CHAT_MIN,
  DOCK_MIN,
  SIDE_MAX,
  SIDE_MIN,
  sidePlace,
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
});

test("a long thread drops its oldest questions in blocks, so its prefix holds between trims", () => {
  const thread = (count: number, answer = "a") =>
    sideHistory(Array.from({ length: count }, (_, at) => turn(`q${at}`, "done", answer)));
  assert.equal(thread(MAX_HISTORY).length, MAX_HISTORY);
  assert.equal(thread(MAX_HISTORY)[0].question, "q0");
  // Past the limit a whole block goes, and the first question sent then stays put.
  for (const count of [MAX_HISTORY + 1, MAX_HISTORY + 7, MAX_HISTORY + HISTORY_BLOCK])
    assert.equal(thread(count)[0].question, `q${HISTORY_BLOCK}`);
  assert.equal(thread(MAX_HISTORY + HISTORY_BLOCK + 1)[0].question, `q${2 * HISTORY_BLOCK}`);
  // Long answers give way by size too, still a block at a time.
  const long = thread(30, "x".repeat(HISTORY_SIZE / 25));
  assert.equal(long[0].question, `q${HISTORY_BLOCK}`);
  // One answer larger than the whole budget is not sent at all.
  assert.deepEqual(thread(1, "x".repeat(HISTORY_SIZE + 1)), []);
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

test("threads with nothing asked and nothing typed are not stored", () => {
  assert.equal(sideStored({ threads: [thread("a")], front: "a" }), null);
  const kept = sideStored({ threads: [thread("a"), { ...thread("b"), draft: "half a question" }, thread("c", [turn("q")])], front: "a" })!;
  assert.deepEqual(kept.threads.map((item) => item.id), ["b", "c"]);
  assert.equal(kept.front, undefined);
});

test("what is stored stays within its size, the oldest questions going first", () => {
  const big = "x".repeat(50_000);
  const chat = {
    front: "new",
    threads: [thread("old", [turn("1", "done", big), turn("2", "done", big), turn("3", "done", big)]), thread("new", [turn("4", "done", big), turn("5", "done", big), turn("6", "done", big)])],
  };
  const stored = sideStored(chat)!;
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

test("the side chat is a column of its own while it fits, then takes the side panel's place, then the whole workbench", () => {
  // Wide: conversation, side chat and side panel side by side, the side chat as wide as asked.
  assert.deepEqual(sidePlace({ width: 1600, wanted: 380, dock: "column" }), { place: "column", width: 380 });
  // Tighter: the side chat gives way first, down to its narrowest.
  assert.deepEqual(sidePlace({ width: 1180, wanted: 380, dock: "column" }), { place: "column", width: 338 });
  // The conversation and the side panel keep their narrowest, dividers included.
  assert.deepEqual(sidePlace({ width: CHAT_MIN + SIDE_MIN + DOCK_MIN + 2, wanted: 380, dock: "column" }), { place: "column", width: SIDE_MIN });
  assert.equal(sidePlace({ width: CHAT_MIN + SIDE_MIN + DOCK_MIN + 1, wanted: 380, dock: "column" }).place, "instead");
  // No room for all three: in the side panel's place.
  assert.deepEqual(sidePlace({ width: 1000, wanted: 380, dock: "column" }), { place: "instead", width: 380 });
  // Without a side panel it is a column as long as the conversation keeps its measure.
  assert.deepEqual(sidePlace({ width: 900, wanted: 900, dock: "none" }), { place: "column", width: 419 });
  assert.equal(sidePlace({ width: 760, wanted: 380, dock: "none" }).place, "over");
  // Never wider than its widest, never narrower than its narrowest.
  assert.equal(sidePlace({ width: 3000, wanted: 2000, dock: "none" }).width, SIDE_MAX);
  assert.equal(sidePlace({ width: 1600, wanted: 100, dock: "column" }).width, SIDE_MIN);
  // A side panel expanded over the conversation keeps the side chat beside it; one laid over a narrow window is covered.
  assert.deepEqual(sidePlace({ width: 1000, wanted: 380, dock: "expanded" }), { place: "column", width: 380 });
  assert.equal(sidePlace({ width: 1000, wanted: 900, dock: "expanded" }).width, 639);
  assert.equal(sidePlace({ width: 1000, wanted: 380, dock: "overlay" }).place, "over");
});
