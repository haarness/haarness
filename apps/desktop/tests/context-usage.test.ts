import test from "node:test";
import assert from "node:assert/strict";
import type { Item, ItemData, Thread } from "@haarness/sdk";
import {
  contextUsage,
  costNumber,
  paceSeconds,
  paceSpeed,
  percentNumber,
  requestPace,
  tokenNumber,
} from "../src/renderer/context-usage";
import { applyLanguage } from "../src/renderer/i18n";
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

test("a timed request reports its first-token time, and its speed only over half a second of streaming", () => {
  const usage = (fields: Record<string, number>): ItemData => ({
    type: "usage",
    input_tokens: 1000,
    output_tokens: 170,
    cache_read_tokens: 0,
    cache_write_tokens: 0,
    cost_usd: null,
    ...fields,
  });
  assert.deepEqual(requestPace(usage({ first_token_ms: 1240, stream_ms: 2000 })), {
    firstTokenMs: 1240,
    tokensPerSecond: 85,
  });
  // What streamed, not the provider's count with its hidden reasoning.
  assert.deepEqual(
    requestPace(usage({ output_tokens: 4000, first_token_ms: 1240, stream_ms: 2000, stream_tokens: 120 })),
    { firstTokenMs: 1240, tokensPerSecond: 60 },
  );
  // One burst is no rate, and a stopped or older record has no timing.
  assert.deepEqual(requestPace(usage({ first_token_ms: 300, stream_ms: 120 })), {
    firstTokenMs: 300,
    tokensPerSecond: undefined,
  });
  assert.equal(requestPace(usage({})), undefined);
  assert.equal(requestPace({ type: "user_message", text: "hi" }), undefined);
  assert.equal(paceSeconds(1240, "en"), "1.2s");
  assert.equal(paceSeconds(9960, "en"), "10s");
  // The decimal mark and the unit follow the reader's language.
  assert.match(paceSeconds(1240, "de"), /^1,2\s?\S/);
  assert.equal(paceSpeed(85.4, "en"), "85");
  assert.equal(paceSpeed(9.94, "en"), "9.9");
});

test("context counts, shares and costs are written in the reader's language", () => {
  assert.equal(tokenNumber(1234567, "en"), "1,234,567");
  assert.equal(tokenNumber(1234567, "de"), "1.234.567");
  assert.equal(tokenNumber(1234567, "fr"), "1\u202f234\u202f567");
  assert.equal(percentNumber(12.345, "en"), "12.3");
  assert.equal(percentNumber(12.345, "de"), "12,3");
  assert.equal(costNumber(0.025, "en"), "$0.0250");
  assert.equal(costNumber(0.025, "de"), "0,0250\u00a0$");
  // An unknown count says so in the interface language too.
  const global = globalThis as { document?: unknown };
  const saved = global.document;
  global.document = { documentElement: {} };
  try {
    assert.equal(tokenNumber(undefined, "en"), "Not available");
    applyLanguage("de");
    assert.equal(tokenNumber(null, "de"), "Nicht verfügbar");
  } finally {
    applyLanguage("en");
    global.document = saved;
  }
});
