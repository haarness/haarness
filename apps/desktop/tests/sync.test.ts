import { test } from "node:test";
import assert from "node:assert/strict";
import { createStore } from "solid-js/store";
import { freshChat, landItems, reduceEvent, type Live } from "../src/renderer/sync";
import type { EventEnvelope, Item } from "@haarness/sdk";
test("replayed durable items replace streamed text without duplicating the conversation", () => {
  const [state, set] = createStore<Live>({
    selected: "thread",
    threads: [],
    items: [],
    turns: [],
    streams: {},
    permissions: [],
    questions: [],
  });
  let eventId = 0;
  const event = (type: string, payload: unknown) =>
    reduceEvent(state, set, {
      eventId: ++eventId,
      type,
      payload,
      threadId: "thread",
      turnId: "turn",
      serverInstanceId: "instance",
    } as EventEnvelope);
  event("turn.text_delta", { delta: "First " });
  event("turn.text_delta", { delta: "reply" });
  assert.equal(state.streams.turn.text, "First reply");
  const item = {
    id: "item",
    threadId: "thread",
    turnId: "turn",
    sequence: 2,
    data: { type: "assistant_message", text: "First reply" },
  };
  event("item.created", item);
  event("item.created", item);
  assert.equal(state.items.length, 1);
  assert.equal(state.streams.turn.text, "");
  event("item.created", {
    ...item,
    id: "user",
    sequence: 1,
    data: { type: "user_message", text: "Request" },
  });
  assert.deepEqual(
    state.items.map((item) => item.id),
    ["user", "item"],
  );
  event("permission.requested", {
    permissionId: "approval",
    threadId: "thread",
  });
  event("permission.requested", {
    permissionId: "approval",
    threadId: "thread",
  });
  assert.equal(state.permissions.length, 1);
  event("permission.resolved", { permissionId: "approval" });
  assert.equal(state.permissions.length, 0);
});

test("a chat created moments ago opens without a history request, unless late or after a reconnect", () => {
  const created = { at: 10_000, stream: 4 };
  assert.equal(freshChat(created, 10_000, 4), true);
  assert.equal(freshChat(created, 12_999, 4), true);
  // The open was superseded and came back later: whatever happened since is loaded.
  assert.equal(freshChat(created, 13_000, 4), false);
  // The event stream reconnected in between and may have missed its first events.
  assert.equal(freshChat(created, 10_500, 5), false);
});

test("a history read keeps what the running turn wrote while it was in flight", () => {
  const item = (id: string, sequence: number, thread = "thread"): Item => ({
    id,
    threadId: thread,
    turnId: "turn",
    sequence,
    createdAt: "2026-10-16T00:00:00Z",
    data: { type: "assistant_message", text: id },
  });
  const event = (type: string, payload: unknown) =>
    ({ eventId: 1, type, payload, serverInstanceId: "instance" }) as EventEnvelope;
  // The read the compaction asked for, and what arrived before it landed:
  // the compaction item it already holds, the turn's next reply, another
  // chat's item and an unrelated event.
  const read = [item("before", 1), item("compaction", 2)];
  const landed = landItems(
    read,
    [
      event("item.created", item("compaction", 2)),
      event("turn.text_delta", { delta: "x" }),
      event("item.created", item("reply", 3)),
      event("item.created", item("elsewhere", 4, "other")),
    ],
    "thread",
  );
  assert.deepEqual(
    landed.map((value) => value.id),
    ["before", "compaction", "reply"],
  );
  // Nothing arrived meanwhile: the read is the history.
  assert.deepEqual(landItems(read, [], "thread"), read);
});
