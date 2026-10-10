import test from "node:test";
import assert from "node:assert/strict";
import type { Item } from "@haarness/sdk";
import {
  toolFailed,
  describeTool,
  errorRecovery,
  permissionCommand,
  shortPath,
  toolChanges,
  toolTarget,
  batchSteps,
  batchLabel,
  toolActivity,
  toolInput,
  toolOutput,
  readable,
  toolResult,
  patchFiles,
  routineEditOutput,
  changeRows,
  changeStats,
} from "../src/renderer/transcript";
import { lines } from "../src/renderer/diff";

test("shell exit status is reflected in tool presentation without interpreting other tools as processes", () => {
  const result = { output: "stderr", is_error: false, metadata: { exit: 7 } };
  assert.equal(toolFailed("bash", result), true);
  assert.equal(
    toolFailed("bash", { ...result, metadata: { exit: 0 } }),
    false,
  );
  assert.equal(toolFailed("read", result), false);
  assert.equal(toolFailed("bash", { ...result, metadata: null }), false);
  assert.equal(toolFailed("read", { ...result, is_error: true }), true);
});

test("a step's verb never claims it is done before it is", () => {
  // A write waiting for approval reads "Writing", a refused one "Tried to write".
  const write = describeTool("write", { path: "/tmp/out.txt" });
  assert.deepEqual(
    [write.verb, write.doing, write.attempt],
    ["Wrote", "Writing", "Tried to write"],
  );
  const click = describeTool("browser_click", { ref: "@e4" });
  assert.deepEqual([click.verb, click.doing, click.attempt], ["Clicked", "Clicking", "Tried to click"]);
  // An unknown tool keeps its own name in every state.
  const custom = describeTool("acme_zap", {});
  assert.deepEqual([custom.verb, custom.doing, custom.attempt], ["acme_zap", "acme_zap", "acme_zap"]);
});

test("named tools get the verb a reader expects", () => {
  assert.deepEqual(describeTool("bash", { command: "npm test" }), {
    icon: "terminal",
    verb: "Ran",
    doing: "Running",
    attempt: "Tried to run",
    target: "npm test",
  });
  assert.equal(describeTool("edit", { path: "src/app.ts" }).verb, "Edited");
  assert.equal(describeTool("read", { path: "src/app.ts" }).verb, "Read");
  assert.equal(describeTool("grep", { pattern: "TODO" }).verb, "Searched");
  // Plan bookkeeping must not be mistaken for a file edit by the "write" match.
  assert.equal(
    describeTool("todowrite", { todos: [] }).verb,
    "Updated the plan",
  );
  assert.equal(describeTool("todoread", {}).icon, "todo");
});

test("tool results stay with their thread, turn and undo boundary when call IDs repeat", () => {
  const item = (
    sequence: number,
    turnId: string,
    data: Item["data"],
    threadId = "thread",
  ): Item => ({
    id: String(sequence),
    sequence,
    turnId,
    threadId,
    data,
    createdAt: "2026-09-07T00:00:00Z",
  });
  const call = {
    type: "tool_call",
    call_id: "repeated",
    name: "bash",
    arguments: { command: "pwd" },
  } as const;
  const result = {
    type: "tool_result",
    call_id: "repeated",
    name: "bash",
    output: "correct",
    is_error: false,
  } as const;
  const items = [
    item(1, "first", call),
    item(2, "first", { ...result, output: "old" }),
    item(3, "next", call),
    item(4, "next", { ...result, output: "other thread" }, "other"),
    item(5, "next", result),
  ];
  assert.equal(toolResult(items, items[2]), items[4]);
  assert.equal(toolResult(items, items[0]), items[1]);
  assert.equal(toolResult(items, items[2], 4), undefined);
  assert.equal(toolResult(items, item(6, "next", call)), undefined);
});

test("tool disclosures use readable inputs while preserving literal commands and source", () => {
  const command = "printf 'one\\ntwo\\n'\nls -la";
  assert.deepEqual(
    toolInput("bash", { command, workdir: "/tmp/example", timeout: 2000 }),
    { label: "Shell", text: "$ " + command, context: "/tmp/example" },
  );
  assert.equal(
    describeTool("FUNCTIONS.Read", { path: "file.ts" }).verb,
    "Read",
  );
  assert.equal(
    describeTool("grep", { pattern: "TODO", path: "src" }).target,
    "TODO in src",
  );
  assert.equal(
    readable({
      query: "example",
      options: { includeArchived: false },
      tags: ["one", "two"],
      metadata: null,
    }),
    "Query: example\nOptions: Include Archived: No\nTags: • one\n• two",
  );
  const source = '{"version":1,"literal":"\\n"}';
  assert.equal(toolOutput("read", source), source);
  assert.equal(toolOutput("bash", source), source);
  assert.equal(
    toolOutput("extension", '{"text":"one\\ntwo","metadata":null}'),
    "Text: one\ntwo",
  );
  assert.equal(toolOutput("extension", "plain output"), "plain output");
});

test("live activity describes a batch by its steps, never by the wrapper", () => {
  const delegations = {
    tool_calls: [
      { tool: "task", arguments: { description: "audit src" } },
      { tool: "task", arguments: { description: "audit deps" } },
      { tool: "task", arguments: { description: "audit config" } },
    ],
  };
  assert.equal(toolActivity("batch", delegations), "Working with agents");
  assert.equal(
    toolActivity("batch", {
      tool_calls: [
        { tool: "read", arguments: { path: "a" } },
        { tool: "grep", arguments: { pattern: "x" } },
        { tool: "read", arguments: { path: "b" } },
        { tool: "list", arguments: { path: "." } },
      ],
    }),
    "Reading files, searching files, listing files",
  );
  // A delegated task's progress carries the server's summary, not arguments.
  assert.equal(toolActivity("batch task,bash"), "Working with agents, running commands");
  assert.equal(toolActivity("batch"), "Working through several steps");
  assert.equal(toolActivity("task"), "Working with an agent");
  assert.equal(toolActivity("bash"), "Running a command");
  assert.equal(describeTool("list", { path: "." }).target, "the project folder");
  assert.equal(describeTool("list", { path: "src" }).target, "src");
});

test("batch presentation pairs ordered outputs and individual failures without JSON envelopes", () => {
  const args = {
    tool_calls: [
      { tool: "functions.read", parameters: { path: "first.ts" } },
      { tool: "read", parameters: { path: "missing.ts" } },
      { tool: "bash", parameters: { command: "npm test" } },
    ],
  };
  const output = JSON.stringify({
    results: [
      { tool: "read", output: "first\nsecond", metadata: null },
      { tool: "read", error: "File does not exist" },
      { tool: "bash", output: "Passed", metadata: { exitCode: 0 } },
    ],
    failures: 1,
  });
  const steps = batchSteps(args, { output, is_error: false });
  assert.equal(batchLabel(steps), "Read files, ran commands");
  assert.equal(steps[0].result?.output, "first\nsecond");
  assert.equal(steps[1].result?.is_error, true);
  assert.equal(steps[2].result?.is_error, false);
  assert.equal(batchSteps(args).filter((step) => step.result).length, 0);
  assert.equal(
    batchSteps(args, { output: "Interrupted", is_error: true })[0].result,
    undefined,
  );
  assert.equal(
    batchSteps(args, {
      output: '{"results":[{"tool":"bash","output":"wrong tool"}]}',
      is_error: false,
    })[0].result,
    undefined,
  );
  assert.deepEqual(batchSteps({ tool_calls: [null] }), []);
  assert.deepEqual(
    batchSteps(args, {
      output: "",
      metadata: JSON.parse(output),
      is_error: false,
    }),
    steps,
  );
});

test("an unknown tool is named rather than described", () => {
  const shape = describeTool("linear__create_issue", { title: "Fix login" });
  assert.equal(shape.verb, "linear__create_issue");
  assert.equal(shape.icon, "tools");
  assert.equal(shape.target, "Fix login");
  // A recognisable family still earns its verb.
  assert.equal(
    describeTool("github_search_code", { query: "x" }).verb,
    "Searched",
  );
});

test("the target is the one argument a reader scans for", () => {
  assert.equal(toolTarget({ command: "ls  -la\n" }), "ls -la");
  assert.equal(
    toolTarget({ unknownKey: "fallback value" }),
    "fallback value",
  );
  assert.equal(toolTarget({ count: 3 }), "");
  assert.equal(toolTarget(undefined), "");
});

test("edits are recognised as changes, other calls are not", () => {
  assert.deepEqual(
    toolChanges("edit", { path: "a.ts", old: "one", new: "two" }),
    [{ path: "a.ts", before: "one", after: "two" }],
  );
  assert.deepEqual(
    toolChanges("edit", {
      file_path: "a.ts",
      old_string: "x",
      new_string: "y",
    }),
    [{ path: "a.ts", before: "x", after: "y" }],
  );
  assert.deepEqual(
    toolChanges("write", { path: "new.ts", content: "hello\n" }),
    [{ path: "new.ts", before: "", after: "hello\n" }],
  );
  assert.equal(
    toolChanges("multiedit", {
      path: "a.ts",
      edits: [
        { old: "1", new: "2" },
        { old: "3", new: "4" },
      ],
    }).length,
    2,
  );
  assert.deepEqual(
    toolChanges("apply_patch", { input: "@@ -1 +1 @@\n-a\n+b\n" }),
    [{ path: "patch", patch: "@@ -1 +1 @@\n-a\n+b\n" }],
  );
  assert.deepEqual(toolChanges("bash", { command: "rm -rf /" }), []);
  assert.deepEqual(toolChanges("todowrite", { todos: [] }), []);
});

test("an edit preview keeps the old snippet's last line as context", () => {
  const [change] = toolChanges("edit", {
    path: "src/store.js",
    old: "const todos = [];",
    new: "// In-memory store; resets on restart.\nconst todos = [];\nlet nextId = 1;",
  });
  assert.deepEqual(
    changeRows(change).map((row) => [row.kind, row.text]),
    [
      ["add", "// In-memory store; resets on restart."],
      ["context", "const todos = [];"],
      ["add", "let nextId = 1;"],
    ],
  );
  assert.deepEqual(changeStats([change]), { additions: 2, deletions: 0 });
  // A replacement is still one line out and one in, newline or not.
  assert.deepEqual(changeStats(toolChanges("edit", { path: "a", old: "one", new: "two\n" })), {
    additions: 1,
    deletions: 1,
  });
  // A whole new file, and counts a tool reports, are taken as they are.
  assert.deepEqual(changeStats(toolChanges("write", { path: "b", content: "x\ny" })), {
    additions: 2,
    deletions: 0,
  });
  assert.deepEqual(changeStats([{ path: "c", before: "a", after: "b", additions: 5, deletions: 3 }]), {
    additions: 5,
    deletions: 3,
  });
  assert.equal(changeStats(toolChanges("edit", { path: "d", old: "same", new: "same" })), undefined);
});

test("a Codex patch envelope reads as the files it edits, and the result's diffs win", () => {
  const input = [
    "*** Begin Patch",
    "*** Update File: src/math.ts",
    "@@ export function average(values: number[]) {",
    "   return total / values.length;",
    " }",
    "+",
    "+export const half = (value: number) => value / 2;",
    "*** Add File: src/new.ts",
    "+export {};",
    "*** Delete File: src/old.ts",
    "*** End Patch",
  ].join("\n");
  assert.deepEqual(
    describeTool("apply_patch", { input }),
    {
      icon: "edit",
      verb: "Edited",
      doing: "Editing",
      attempt: "Tried to edit",
      target: "src/math.ts, src/new.ts +1",
    },
  );
  assert.equal(
    describeTool("apply_patch", { input: input.replace(/\*\*\* (Add|Delete) File: .*\n(\+export \{\};\n)?/g, "") }).target,
    "src/math.ts",
  );
  const files = patchFiles(input);
  assert.deepEqual(
    files.map(({ path, additions, deletions }) => ({ path, additions, deletions })),
    [
      { path: "src/math.ts", additions: 2, deletions: 0 },
      { path: "src/new.ts", additions: 1, deletions: 0 },
      { path: "src/old.ts", additions: 0, deletions: 0 },
    ],
  );
  // The synthesized unified diff parses, so the preview shows real rows.
  assert.deepEqual(
    lines({ file: "src/math.ts", patch: files[0].patch } as never)
      .filter((row) => row.kind === "add")
      .map((row) => row.text),
    ["", "export const half = (value: number) => value / 2;"],
  );
  assert.deepEqual(toolChanges("apply_patch", { input }), files);
  const result = {
    output: "updated src/math.ts",
    is_error: false,
    metadata: {
      diffs: [
        { file: "src/math.ts", before: "a\n", after: "a\nb\n", additions: 1, deletions: 0 },
      ],
    },
  };
  assert.deepEqual(toolChanges("apply_patch", { input }, result), [
    { path: "src/math.ts", patch: undefined, before: "a\n", after: "a\nb\n", additions: 1, deletions: 0 },
  ]);
  // A targeted edit keeps its own concise snippet even when the result reports the whole file.
  assert.deepEqual(toolChanges("edit", { path: "a.json", old: "true", new: "false" }, result), [
    { path: "a.json", before: "true", after: "false" },
  ]);
  // A failed call changed nothing, so its arguments are all there is to show.
  assert.deepEqual(toolChanges("apply_patch", { input }, { ...result, is_error: true }), files);
  assert.deepEqual(patchFiles("@@ -1 +1 @@\n-a\n+b\n"), []);
});

test("routine edit confirmations are dropped under a diff, diagnostics are not", () => {
  assert.equal(routineEditOutput("updated src/math.ts"), true);
  assert.equal(routineEditOutput("Success. Updated the following files:\nM src/math.ts\nA src/new.ts"), true);
  assert.equal(routineEditOutput("Wrote 12 lines to src/new.ts"), true);
  assert.equal(routineEditOutput("updated src/math.ts\nerror TS2304: Cannot find name 'x'."), false);
  assert.equal(routineEditOutput("LSP errors detected: 2"), false);
  assert.equal(routineEditOutput(""), false);
});

test("a permission shows its command, and falls back to the raw payload", () => {
  assert.equal(permissionCommand({ command: "git push" }), "git push");
  assert.equal(permissionCommand({ path: "/tmp/x" }), "/tmp/x");
  assert.equal(permissionCommand({ nothing: 1 }), "");
  assert.equal(permissionCommand(null), "");
});

test("credential failures offer the setting that fixes them", () => {
  assert.deepEqual(errorRecovery("ANTHROPIC_API_KEY is not configured"), {
    label: "Open providers",
    pane: "providers",
  });
  assert.equal(errorRecovery("the model returned a 500"), null);
});

test("a working location is identified by the tail of its path", () => {
  assert.equal(shortPath("/Users/alex/work/acme/api"), "…/acme/api");
  assert.equal(shortPath("/srv/app"), "/srv/app");
  assert.equal(shortPath("/"), "/");
});

test("browser steps read as what happened in the page, never as element refs", () => {
  assert.deepEqual(describeTool("browser_navigate", { url: "https://example.com/" }), { icon: "web", verb: "Opened", doing: "Opening", attempt: "Tried to open", target: "https://example.com/" });
  assert.deepEqual(describeTool("browser_fill", { tabID: "t", ref: "@e4", text: "Jane  Appleseed" }), { icon: "web", verb: "Typed", doing: "Typing", attempt: "Tried to type", target: "“Jane Appleseed”" });
  assert.deepEqual(describeTool("browser_click", { tabID: "t", ref: "@e11" }), { icon: "web", verb: "Clicked", doing: "Clicking", attempt: "Tried to click", target: "" });
  assert.equal(describeTool("browser_check", { ref: "@e10", checked: false }).verb, "Unchecked");
  assert.equal(describeTool("browser_select", { ref: "@e6", values: ["Express"] }).target, "“Express”");
  assert.equal(describeTool("browser_scroll", { deltaY: -400 }).target, "up");
  assert.equal(describeTool("browser_tabs_list", {}).verb, "Listed tabs");
  assert.equal(describeTool("browser_lighthouse", {}).verb, "Browser · lighthouse");
});
