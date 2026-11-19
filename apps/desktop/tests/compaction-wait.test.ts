import { test } from "node:test";
import assert from "node:assert/strict";
import type { DesktopEvent } from "../src/shared";
import { CHAT_CHANGED, isCompactionCut, stoppedOnPurpose, watchCompaction } from "../src/renderer/compaction-wait";
import { errorText } from "../src/renderer/error-text";

/** A stand-in for the window's server events. */
function events() {
  const listeners = new Set<(event: DesktopEvent) => void>();
  return {
    subscribe: (listener: (event: DesktopEvent) => void) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    send: (connection: string, data: Record<string, unknown>) => {
      for (const listener of [...listeners])
        listener({ kind: "server", connection, data: { serverInstanceId: "first", ...data } });
    },
    get count() {
      return listeners.size;
    },
  };
}

const compacted = (threadId: string) => ({ type: "thread.compacted", threadId });

test("a cut compaction waits for its own chat's compacted event, and stops listening", async () => {
  const bus = events();
  const watch = watchCompaction("project", "chat", "first", bus.subscribe);
  const waiting = watch.until(Date.now() + 5_000, "Your computer couldn't answer.", CHAT_CHANGED);
  let done = false;
  void waiting.then(() => (done = true));
  bus.send("project", compacted("another chat"));
  bus.send("another project", compacted("chat"));
  bus.send("project", { type: "item.created", threadId: "chat", payload: { data: { type: "compaction" } } });
  bus.send("project", { type: "thread.updated", threadId: "chat" });
  await new Promise((resolve) => setTimeout(resolve, 10));
  assert.equal(done, false, "only this chat's compaction ends the wait");
  bus.send("project", compacted("chat"));
  await waiting;
  assert.equal(bus.count, 0);
});

test("a compaction that landed before its request came back is not missed", async () => {
  const bus = events();
  const watch = watchCompaction("project", "chat", "first", bus.subscribe);
  bus.send("project", compacted("chat"));
  await watch.until(Date.now() + 5_000, "never said", CHAT_CHANGED);
  assert.equal(bus.count, 0);
});

test("a cut compaction that never lands says the cut once the wait is over", async () => {
  const bus = events();
  const watch = watchCompaction("project", "chat", "first", bus.subscribe);
  await assert.rejects(watch.until(Date.now() + 20, "Your computer couldn't answer.", CHAT_CHANGED), {
    message: "Your computer couldn't answer.",
  });
  assert.equal(bus.count, 0);
  assert.equal(isCompactionCut({ compactionCut: "Your computer couldn't answer." }), true);
  assert.equal(isCompactionCut({ id: "item", data: { type: "compaction" } }), false);
});

test("the wait ends at once when the chat changes or the server restarts", async () => {
  for (const [data, said] of [
    [{ type: "item.created", threadId: "chat", payload: { data: { type: "user_message" } } }, CHAT_CHANGED],
    [{ type: "turn.started", threadId: "chat" }, CHAT_CHANGED],
    [{ type: "thread.updated", threadId: "another chat", serverInstanceId: "second" }, "Your computer couldn't answer."],
  ] as const) {
    const bus = events();
    const watch = watchCompaction("project", "chat", "first", bus.subscribe);
    const waiting = watch.until(Date.now() + 5_000, "Your computer couldn't answer.", CHAT_CHANGED);
    bus.send("project", data);
    await assert.rejects(waiting, { message: said });
    assert.equal(bus.count, 0);
  }
});

test("the server's words about a compaction are said in the reader's language", () => {
  assert.equal(errorText(new Error("conflict: this chat is already being compacted")).message, "This chat is already being compacted");
  assert.equal(
    errorText(new Error("conflict: thread changed while the compaction summary was generated; retry compaction")).message,
    CHAT_CHANGED,
  );
  assert.equal(errorText(new Error("The summary didn't come back in time")).message, "The summary didn't come back in time");
  assert.equal(errorText(new Error("The summary didn't come back in time")).known, true);
});

test("a compaction someone stopped is no failure", () => {
  assert.equal(stoppedOnPurpose(new Error("conflict: compaction stopped before its summary was written")), true);
  assert.equal(stoppedOnPurpose(new Error("conflict: thread changed while the compaction summary was generated; retry compaction")), false);
  assert.equal(stoppedOnPurpose(new Error("provider error: overloaded")), false);
});
