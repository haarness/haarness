import test from "node:test";
import assert from "node:assert/strict";
import type { Item, ItemData } from "@haarness/sdk";
import { literalParts, literalText, withoutEscapes } from "../src/renderer/display-text";
import { resultText, toolInput, toolTarget, toolTitle } from "../src/renderer/transcript";
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
  ])
    assert.equal(withoutEscapes(raw), shown, JSON.stringify(raw));
});

test("a C1 control goes on its own and hides nothing", () => {
  for (const [raw, shown] of [
    ["She wrote â\u0080\u009csoonâ\u0080\u009d and stopped.", "She wrote âsoonâ and stopped."],
    ["Ð\u009fÑ\u0080Ð¸ the rest", "ÐÑÐ¸ the rest"],
    ["ç\u009b® list\nnext", "ç® list\nnext"],
    ["a\u0090b\u0098c\u009ed\u009fe\u009cf", "abcdef"],
    ["\u009b31mtext", "31mtext"],
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
    assert.equal(literalText(text), text);
    assert.deepEqual(literalParts(text), [{ text, code: false }]);
  }
});

test("an approval shows what is invisible in it as quiet codes", () => {
  assert.deepEqual(literalParts("cat report‮fdp.sh"), [
    { text: "cat report", code: false },
    { text: "\\u{202E}", code: true },
    { text: "fdp.sh", code: false },
  ]);
  assert.equal(literalText("cd dist\r\nrm -r *"), "cd dist\\u{D}\nrm -r *");
  assert.equal(literalText("x\u001b[1Ay"), "x\\u{1B}[1Ay");
  assert.equal(literalText("p​q⁧r﻿s\u{e0070}"), "p\\u{200B}q\\u{2067}r\\u{FEFF}s\\u{E0070}");
  assert.equal(literalText("a b­c"), "a\\u{2029}b\\u{AD}c");
  // Tags that spell no flag show.
  assert.equal(literalText("🏴\u{e0061}\u{e007f}"), "🏴\\u{E0061}\\u{E007F}");
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

test("a row and its Shell panel show a command's invisible characters as codes, on one line", () => {
  const args = { command: "cat report‮fdp.sh​ && cd dist\r\nrm -r cache" };
  const target = toolTarget(args);
  assert.equal(target, "cat report\\u{202E}fdp.sh\\u{200B} && cd dist\\u{D} rm -r cache");
  assert.equal(toolTitle(args, target), "cat report\\u{202E}fdp.sh\\u{200B} && cd dist\\u{D}\nrm -r cache");
  assert.equal(toolInput("bash", args).text, "$ cat report\\u{202E}fdp.sh\\u{200B} && cd dist\\u{D}\nrm -r cache");
  // An ordinary command is unchanged.
  const plain = { command: "npm test -- --grep \"日本\"\n  && git status" };
  assert.equal(toolTarget(plain), "npm test -- --grep \"日本\" && git status");
  assert.equal(toolTitle(plain, toolTarget(plain)), plain.command);
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
