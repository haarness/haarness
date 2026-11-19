import { test } from "node:test";
import assert from "node:assert/strict";
import type { DesktopEvent } from "../src/shared";
import { isCompactionCut, watchCompaction } from "../src/renderer/compaction-wait";

/** A stand-in for the window's server events. */
function events() {
  const listeners = new Set<(event: DesktopEvent) => void>();
  return {
    subscribe: (listener: (event: DesktopEvent) => void) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    send: (connection: string, type: string, threadId: string) => {
      for (const listener of [...listeners]) listener({ kind: "server", connection, data: { type, threadId } });
    },
    get count() {
      return listeners.size;
    },
  };
}

test("a cut compaction waits for its own chat's compacted event, and stops listening", async () => {
  const bus = events();
  const watch = watchCompaction("project", "chat", bus.subscribe);
  const waiting = watch.until(Date.now() + 5_000, "Your computer couldn't answer.");
  let done = false;
  void waiting.then(() => (done = true));
  bus.send("project", "thread.compacted", "another chat");
  bus.send("another project", "thread.compacted", "chat");
  bus.send("project", "item.created", "chat");
  await new Promise((resolve) => setTimeout(resolve, 10));
  assert.equal(done, false, "only this chat's compaction ends the wait");
  bus.send("project", "thread.compacted", "chat");
  await waiting;
  assert.equal(bus.count, 0);
});

test("a compaction that landed before its request came back is not missed", async () => {
  const bus = events();
  const watch = watchCompaction("project", "chat", bus.subscribe);
  bus.send("project", "thread.compacted", "chat");
  await watch.until(Date.now() + 5_000, "never said");
  assert.equal(bus.count, 0);
});

test("a cut compaction that never lands says the cut once the wait is over", async () => {
  const bus = events();
  const watch = watchCompaction("project", "chat", bus.subscribe);
  await assert.rejects(watch.until(Date.now() + 20, "Your computer couldn't answer."), {
    message: "Your computer couldn't answer.",
  });
  assert.equal(bus.count, 0);
  assert.equal(isCompactionCut({ compactionCut: "Your computer couldn't answer." }), true);
  assert.equal(isCompactionCut({ id: "item", data: { type: "compaction" } }), false);
});
