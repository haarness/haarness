import test from "node:test";
import assert from "node:assert/strict";
import type { Item, ItemData, Thread } from "@haarness/sdk";
import { contextUsage } from "../src/renderer/context-usage";
const thread = { id: "one" } as Thread;
const item = (sequence: number, data: ItemData): Item => ({
  id: String(sequence),
  sequence,
  data,
  threadId: "one",
  createdAt: "",
  turnId: null,
});
test("context estimates use the newest summary and retained tail, respecting undo and thread scope", () => {
  const items = [
    item(1, { type: "user_message", text: "a".repeat(400) }),
    item(2, {
      type: "compaction",
      summary: "obsolete",
      files_touched: [],
      outstanding: [],
    }),
    item(3, { type: "assistant_message", text: "abcd" }),
    item(4, {
      type: "compaction",
      summary: "12345678",
      files_touched: [],
      outstanding: [],
      tail_start_sequence: 3,
    }),
    item(5, {
      type: "tool_result",
      call_id: "call",
      name: "read",
      is_error: false,
      output: "abcdefgh",
    }),
    item(6, { type: "usage", input_tokens: 999, output_tokens: 20 }),
    item(7, { type: "assistant_message", text: "z".repeat(400) }),
    {
      ...item(8, { type: "assistant_message", text: "foreign" }),
      threadId: "two",
    },
  ];
  const result = contextUsage(items, {
    ...thread,
    revert: { sequence: 6 } as Thread["revert"],
  });
  assert.equal(result.total, 5);
  assert.equal(result.latest?.id, "6");
  assert.equal(
    result.breakdown.find((row) => row.label === "Compaction summary")
      ?.tokens,
    2,
  );
  assert.equal(
    contextUsage(items, {
      ...thread,
      revert: { sequence: 4 } as Thread["revert"],
    }).latest,
    undefined,
  );
});
test("context estimates exclude media payloads, reasoning and accounting rows", () => {
  const result = contextUsage(
    [
      item(1, {
        type: "user_message",
        text: "😀😀😀😀",
        attachments: ["data:image/png;base64," + "a".repeat(10000)],
      }),
      item(2, { type: "reasoning", text: "unreplayed reasoning" }),
      item(3, { type: "usage", input_tokens: 10000, output_tokens: 1000 }),
    ],
    thread,
  );
  assert.equal(result.total, 1);
  assert.equal(result.attachments, 1);
  assert.equal(result.reports, 1);
  assert.equal(contextUsage([], thread).latest, undefined);
});
