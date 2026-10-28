import test from "node:test";
import assert from "node:assert/strict";
import type { Item, ItemData } from "@haarness/sdk";
import { spelledOutParts, spelledOutPlain, spelledOutText, turnsDirection, withoutEscapes } from "../src/renderer/display-text";
import { describeTool, resultText, toolInput, toolTarget, toolTitle } from "../src/renderer/transcript";
import { findConversationMessages } from "../src/renderer/conversation-history";
import { agentTaskError, type AgentTask } from "../src/renderer/agent-tasks";
import { followRun } from "../src/renderer/command-result";
import type { WorkflowRun } from "@haarness/sdk";

/** `expected` with each `\u{…}` code set apart as plain text sets it: between
 * a left-to-right isolate and its end. */
const isolated = (expected: string) => expected.replace(/\\u\{[0-9A-F]+\}/g, (code) => "\u2066" + code + "\u2069");

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
    "👩\u200d🔬 ❤️ 🇯🇵 👍🏽 🏴\u{e0067}\u{e0062}\u{e0073}\u{e0063}\u{e0074}\u{e007f}",
    "日本語のテキスト 中文 한국어",
    "שלום עולם مرحبا بالعالم",
    "ن\u200cه é̂ 葛\u{e0100}",
    "۝١٢ ؀١٠",
    "a\tb\nc",
    "",
  ]) {
    assert.equal(withoutEscapes(text), text);
    assert.equal(spelledOutText(text), text);
    assert.deepEqual(spelledOutParts(text), [{ text, code: false }]);
  }
});

test("an approval shows what is invisible in it as quiet codes", () => {
  assert.deepEqual(spelledOutParts("cat report\u202efdp.sh"), [
    { text: "cat report", code: false },
    { text: "\\u{202E}", code: true },
    { text: "fdp.sh", code: false },
  ]);
  assert.equal(spelledOutText("cd dist\r\nrm -r *"), isolated("cd dist\\u{D}\nrm -r *"));
  assert.equal(spelledOutText("x\u001b[1Ay"), isolated("x\\u{1B}[1Ay"));
  assert.equal(spelledOutText("p\u200bq\u2067r\ufeffs\u{e0070}"), isolated("p\\u{200B}q\\u{2067}r\\u{FEFF}s\\u{E0070}"));
  assert.equal(spelledOutText("a\u2029b\u00adc"), isolated("a\\u{2029}b\\u{AD}c"));
  // Tags that spell no flag show.
  assert.equal(spelledOutText("🏴\u{e0061}\u{e007f}"), isolated("🏴\\u{E0061}\\u{E007F}"));
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
  const args = { command: "cat report\u202efdp.sh\u200b && cd dist\r\nrm -r cache" };
  const target = toolTarget(args);
  assert.equal(target, isolated("cat report\\u{202E}fdp.sh\\u{200B} && cd dist\\u{D} rm -r cache"));
  assert.equal(toolTitle(args, target), isolated("cat report\\u{202E}fdp.sh\\u{200B} && cd dist\\u{D}\nrm -r cache"));
  // The Shell panel draws the command as written, through its quiet codes.
  assert.deepEqual(toolInput("bash", args), { label: "Shell", text: "$ " + args.command, context: "" });
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

test("an edit on its card shows only what turns a line's direction as a code", () => {
  const line = 'if (role != "user\u202e \u2066// admin\u2069 \u2066") {';
  assert.equal(
    spelledOutParts(line, turnsDirection).map((part) => part.text).join(""),
    'if (role != "user\\u{202E} \\u{2066}// admin\\u{2069} \\u{2066}") {',
  );
  for (const text of ["مرحبا\u200f بالعالم", "a\u200bb\tc", "שלום"])
    assert.deepEqual(spelledOutParts(text, turnsDirection), [{ text, code: false }]);
});

test("every kind of row target shows what would not show as a code", () => {
  const target = (name: string, args: unknown) => describeTool(name, args).target;
  assert.equal(target("grep", { pattern: "todo\u202e", path: "src\u200b" }), isolated("todo\\u{202E} in src\\u{200B}"));
  assert.equal(target("glob", { pattern: "*.js\u2066" }), isolated("*.js\\u{2066}"));
  assert.equal(
    target("apply_patch", { input: "*** Begin Patch\n*** Update File: src/notes\u202etxt.js\n@@\n-a\n+b\n*** End Patch" }),
    isolated("src/notes\\u{202E}txt.js"),
  );
  assert.equal(target("mcp_resource_read", { uri: "file:///a\u202eb" }), isolated("file:///a\\u{202E}b"));
  assert.equal(target("mcp_resource_list", { server: "docs\u200b" }), isolated("docs\\u{200B}"));
  assert.equal(target("websearch", { query: "news\u202e today" }), isolated("news\\u{202E} today"));
  assert.equal(target("load_tool", { queries: ["a\u2067"] }), isolated("a\\u{2067}"));
  // Ordinary targets read as before.
  assert.equal(target("grep", { pattern: "TODO", path: "src" }), "TODO in src");
  assert.equal(target("websearch", { query: "مرحبا بالعالم" }), "مرحبا بالعالم");
});

test("a failed workflow's error reads without the escape sequences its program wrote", () => {
  const run = {
    id: "0190-run",
    workflowName: "nightly",
    status: "failed",
    error: "child stderr: \u001b[31merror:\u001b[0m lint failed\u001b]0;runner\u0007\n",
    time: { created: "2026-10-28T00:00:00Z", updated: "2026-10-28T00:00:01Z" },
  } as unknown as WorkflowRun;
  assert.equal(
    followRun({ title: "/nightly", text: "", run: run.id }, run).text,
    "Workflow nightly failed.\nchild stderr: error: lint failed\nRun ID: 0190-run",
  );
});

test("a code in plain text stays one left-to-right piece beside right-to-left words", () => {
  // Without the isolate, `ملف\u{200F}` in a right-to-left line reads `u{200F}\ملف`.
  assert.equal(spelledOutText("ملف\u200f.txt"), "ملف\u2066\\u{200F}\u2069.txt");
  // Text with nothing to spell out gains nothing.
  assert.equal(spelledOutText("ملف.txt"), "ملف.txt");
});

test("a browser step names what it typed, chose or opened with its codes, as the Chrome card does", () => {
  const target = (action: string, args: unknown) => describeTool("browser_" + action, args).target;
  assert.equal(target("fill", { text: "invoice\u202efdp.exe" }), "“" + isolated("invoice\\u{202E}fdp.exe") + "”");
  assert.equal(target("navigate", { url: "https://example.org/\u200bpay" }), isolated("https://example.org/\\u{200B}pay"));
  assert.equal(target("select", { values: ["a\u2066b"] }), "“" + isolated("a\\u{2066}b") + "”");
  assert.equal(target("fill", { text: "  جواب  نهائي " }), "“جواب نهائي”");
});

test("find in chat previews a call's arguments with their codes and finds a code by its text", () => {
  const item = (sequence: number, data: ItemData): Item => ({
    id: String(sequence),
    threadId: "thread",
    turnId: "turn",
    sequence,
    createdAt: "2026-10-28T00:00:00Z",
    data,
  });
  const items = [item(1, { type: "tool_call", name: "bash", call_id: "call", arguments: { command: "cat x\u202efdp.sh" } })];
  const [hit] = findConversationMessages(items, items, "\\u{202E}");
  assert.equal(hit?.id, "1");
  assert.ok(!/[\u2066-\u2069\u202e]/.test(hit.before + hit.match + hit.after));
  assert.equal(spelledOutPlain("x\u202ey"), "x\\u{202E}y");
});
