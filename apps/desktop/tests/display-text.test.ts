import test from "node:test";
import assert from "node:assert/strict";
import type { Item, ItemData } from "@haarness/sdk";
import { withoutEscapes } from "../src/renderer/display-text";
import { resultText } from "../src/renderer/transcript";
import { findConversationMessages } from "../src/renderer/conversation-history";
import { agentTaskError, type AgentTask } from "../src/renderer/agent-tasks";

test("colours, titles and cursor moves leave only their text", () => {
  for (const [raw, shown] of [
    ["\u001b[32mok\u001b[39m", "ok"],
    ["\u001b[1;38;2;0;128;255mbold\u001b[m", "bold"],
    ["\u001b[?25l\u001b[2K\u001b[1Gready", "ready"],
    ["\u001b]0;build logs\u0007after", "after"],
    ["\u001b]2;tab name\u001b\\after", "after"],
    ["\u001b]8;;https://example.org/docs\u001b\\docs\u001b]8;;\u001b\\ page", "docs page"],
    ["\u001bPq#1;2;9;9;9\u001b\\x", "x"],
    ["\u001b_hidden\u001b\\shown", "shown"],
    ["\u001b(B\u001b7\u001b8\u001b>plain", "plain"],
    ["\u009b1mc1\u009b22m", "c1"],
    ["\u009d2;name\u009cc1 name", "c1 name"],
  ])
    assert.equal(withoutEscapes(raw), shown, JSON.stringify(raw));
});

test("a sequence that removing another completes goes too", () => {
  assert.equal(withoutEscapes("\u001b\u001b[22m[32mok"), "ok");
  assert.equal(withoutEscapes("x\u001b[3\u001b[39m2my"), "xy");
  assert.equal(withoutEscapes("\u001b\u001b]0;t\u0007[1mz"), "z");
  assert.equal(withoutEscapes("\u001b".repeat(500) + "[m".repeat(500)), "");
});

test("tabs, line breaks and the text after a broken escape stay", () => {
  assert.equal(withoutEscapes("a\tb\r\nc\n"), "a\tb\r\nc\n");
  assert.equal(withoutEscapes("end\u001b\nnext"), "end\nnext");
  assert.equal(withoutEscapes("ü\u001bß"), "üß");
  assert.equal(withoutEscapes("y\u001b[38;5"), "y[38;5");
  // A title that never ends hides only the rest of its own line.
  assert.equal(withoutEscapes("a\u001b]0;name\nb"), "a\nb");
  assert.equal(withoutEscapes("a\u001b]0;name"), "a");
  assert.equal(withoutEscapes("ring\u0007 zero\u0000 rub\u007f"), "ring zero rub");
  // Astral characters are read whole.
  assert.equal(withoutEscapes("\u001b[1m😀\u001b[0m 𝒳"), "😀 𝒳");
});

test("what people write reads exactly as before", () => {
  for (const text of [
    "👩‍🔬 ❤️ 🇯🇵 👍🏽 🏴\u{e0067}\u{e0062}\u{e0073}\u{e0063}\u{e0074}\u{e007f}",
    "日本語のテキスト 中文 한국어",
    "שלום עולם مرحبا بالعالم",
    "ن‌ه é̂ 葛\u{e0100}",
    "۝١٢ ؀١٠",
    "a\tb\nc",
    "",
  ]) {
    assert.equal(withoutEscapes(text), text);
  }
});

test("a row shows a command's and a server's text without their escapes", () => {
  const coloured = "exit: 0\n\u001b]0;build logs\u0007\u001b[32mpassed\u001b[39m 12 tests\n";
  assert.equal(resultText("bash", { output: coloured, is_error: false, metadata: { exit: 0 } }), "passed 12 tests\n");
  assert.equal(
    resultText("mcp__wiki__lookup", {
      output: "tool error: \u001b[31mFailed:\u001b[39m page moved",
      is_error: true,
    }),
    "Failed: page moved",
  );
  // An envelope read as JSON is cleaned after it is read.
  assert.equal(
    resultText("mcp__wiki__lookup", { output: JSON.stringify({ note: "\u001b[1mbold\u001b[0m" }), is_error: false }),
    "Note: bold",
  );
});

test("find in chat reads a command's output as its row shows it", () => {
  const item = (sequence: number, data: ItemData): Item => ({
    id: String(sequence),
    threadId: "thread",
    turnId: "turn",
    sequence,
    createdAt: "2026-10-28T00:00:00Z",
    data,
  });
  const items = [
    item(1, { type: "tool_call", name: "bash", call_id: "call", arguments: { command: "npm test" } }),
    item(2, {
      type: "tool_result",
      name: "bash",
      call_id: "call",
      output: "\u001b[32mpassed\u001b[39m 12 tests",
      is_error: false,
    }),
  ];
  const [hit] = findConversationMessages(items.slice(0, 1), items, "passed 12");
  assert.equal(hit?.id, "1");
  assert.equal(hit.match, "passed 12");
  assert.equal(findConversationMessages(items.slice(0, 1), items, "[32m").length, 0);
});

test("a failed agent's error reads without its escape sequences", () => {
  const task = (output: string, is_error: boolean) =>
    ({
      key: "turn:task",
      item: { id: "1", threadId: "thread", turnId: "turn", sequence: 1, createdAt: "2026-10-28T00:00:00Z" },
      result: { type: "tool_result", name: "task", call_id: "task", output, is_error },
    }) as unknown as AgentTask;
  assert.equal(agentTaskError(task("\u001b[31merror:\u001b[0m the model refused", true)), "error: the model refused");
  assert.equal(agentTaskError(task("\u001b[1mdone\u001b[0m", false)), "");
});
