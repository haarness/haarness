import { test } from "node:test";
import assert from "node:assert/strict";
import { createStore } from "solid-js/store";
import { reduceEvent, type Live } from "../src/renderer/sync";
import type { EventEnvelope } from "@haarness/sdk";
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
