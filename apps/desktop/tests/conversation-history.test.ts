import test from "node:test";
import assert from "node:assert/strict";
import type { Item, ItemData, Thread } from "@haarness/sdk";
import { retainedItems, lastUserMessage, findConversationMessages, unrestoredFiles } from "../src/renderer/conversation-history";
const item = (sequence: number, data: ItemData): Item => ({ id: String(sequence), threadId: "thread", turnId: "turn", sequence, createdAt: "2026-09-11T00:00:00Z", data });
test("undo boundaries hide future messages without destroying redo history", () => {
  const items = [item(1, { type: "user_message", text: "first" }), item(2, { type: "assistant_message", text: "reply" }), item(3, { type: "user_message", text: "second" }), item(4, { type: "user_message", text: "internal", synthetic: true })];
  const thread = { revert: { sequence: 2 } } as Thread;
  assert.deepEqual(retainedItems(items, thread).map(value => value.id), ["1", "2"]);
  assert.equal(lastUserMessage(items, 2)?.id, "1");
  assert.equal(lastUserMessage(items)?.id, "3");
  assert.equal(items.length, 4);
});
test("find is literal, case insensitive and indexes unmounted messages and matching tool output", () => {
  const items = [item(1, { type: "assistant_message", text: "Needle [.*] repeated needle [.*]" }), item(2, { type: "tool_call", name: "read", call_id: "call", arguments: { path: "file.txt" } }), item(3, { type: "tool_result", name: "read", call_id: "call", output: "Needle [.*] in file", is_error: false })];
  const rows = items.slice(0, 2);
  assert.deepEqual(findConversationMessages(rows, items, "needle [.*]").map(hit => hit.id), ["1", "2"]);
  assert.equal(findConversationMessages(rows, items, "thread").length, 0);
  assert.equal(findConversationMessages(rows, items, "  ").length, 0);
  assert.equal(findConversationMessages(rows, items, "[.*]")[0].match, "[.*]");
});
test("an undo names the files it left as they are, and counts the ones it does not name", () => {
  const thread = (revert?: Partial<NonNullable<Thread["revert"]>>) => ({ revert: revert && { sequence: 2, snapshotId: "backup", ...revert } }) as Thread;
  assert.deepEqual(unrestoredFiles(), { names: [], count: 0, more: 0 });
  assert.deepEqual(unrestoredFiles(thread()), { names: [], count: 0, more: 0 });
  // An older server says nothing about them.
  assert.deepEqual(unrestoredFiles(thread({})), { names: [], count: 0, more: 0 });
  assert.deepEqual(unrestoredFiles(thread({ unrestored: [".env", "big.bin"] })), { names: [".env", "big.bin"], count: 2, more: 0 });
  assert.deepEqual(unrestoredFiles(thread({ unrestored: [".env"], unrestoredCount: 51 })), { names: [".env"], count: 51, more: 50 });
  // A count below the names given never hides one.
  assert.deepEqual(unrestoredFiles(thread({ unrestored: ["a", "b"], unrestoredCount: 1 })), { names: ["a", "b"], count: 2, more: 0 });
});
