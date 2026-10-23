import test from "node:test";
import assert from "node:assert/strict";
import type { Item } from "@haarness/sdk";
import {
  answeredByHook,
  reviewBlock,
  toolFailed,
  describeTool,
  errorRecovery,
  permissionCommand,
  permissionScript,
  shortPath,
  toolChanges,
  toolTarget,
  toolTitle,
  batchSteps,
  batchLabel,
  batchTarget,
  targetList,
  toolActivity,
  toolInput,
  toolOutput,
  readable,
  toolResult,
  patchFiles,
  routineEditOutput,
  changeRows,
  changeStats,
  toolVerb,
  toolVerbKey,
  toolVerbKeys,
  toolKind,
  toolWords,
  resultText,
  exitStatus,
} from "../src/renderer/transcript";
import { lines } from "../src/renderer/diff";
import { applyLanguage } from "../src/renderer/i18n";

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

test("a question a hook answered says so, and only then", () => {
  const hook = { output: "Unit tests", is_error: false, metadata: { answeredBy: "hook" } };
  assert.equal(answeredByHook("question", hook), true);
  assert.equal(answeredByHook("question", { output: "Unit tests", is_error: false }), false);
  assert.equal(answeredByHook("question", undefined), false);
  assert.equal(answeredByHook("bash", hook), false);
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

test("a collapsed batch names what its steps acted on, the first two and then a count", () => {
  const steps = (calls: [string, unknown][]) =>
    batchSteps({ tool_calls: calls.map(([tool, parameters]) => ({ tool, parameters })) });
  assert.equal(
    batchTarget(steps([["read", { path: "src/a.ts" }], ["read", { path: "src/b.ts" }]])),
    "src/a.ts, src/b.ts",
  );
  // Repeated targets count once; a search names its pattern and folder.
  assert.equal(
    batchTarget(
      steps([
        ["read", { path: "src/a.ts" }],
        ["read", { path: "src/a.ts", offset: 40 }],
        ["grep", { pattern: "TODO", path: "src" }],
        ["bash", { command: "npm test" }],
        ["read", { path: "README.md" }],
      ]),
    ),
    "src/a.ts, TODO in src +2",
  );
  assert.equal(batchTarget(steps([["todoread", {}]])), "");
  assert.equal(batchTarget([]), "");
});

test("a list of targets is joined the way the interface language lists things", () => {
  assert.equal(targetList([]), "");
  assert.equal(targetList(["a.ts"]), "a.ts");
  assert.equal(targetList(["a.ts", "b.ts"]), "a.ts, b.ts");
  assert.equal(targetList(["a.ts", "b.ts", "c.ts", "d.ts"]), "a.ts, b.ts +2");
  const global = globalThis as { document?: unknown };
  const saved = global.document;
  global.document = { documentElement: {} };
  try {
    applyLanguage("ja");
    assert.equal(targetList(["a.ts", "b.ts"]), "a.ts、b.ts");
    assert.equal(targetList(["a.ts", "b.ts", "c.ts"]), "a.ts、b.ts ほか 1 件");
    applyLanguage("ar");
    assert.equal(targetList(["a.ts", "b.ts"]), "a.ts، b.ts");
  } finally {
    applyLanguage("en");
    global.document = saved;
  }
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

test("a tool's family comes from whole words of its name, never from letters inside one", () => {
  assert.deepEqual(toolWords("mcp__github__searchIssues"), ["mcp", "github", "search", "issues"]);
  assert.deepEqual(toolWords("read-file.v2"), ["read", "file", "v2"]);
  // "frobnicate" holds "cat", "catalog" holds "cat", "spawning" holds "spawn".
  for (const name of ["frobnicate", "frobnicate_widgets", "catalog_sync", "spawning_pool", "lists2csv"]) {
    assert.equal(toolKind(name), "other", name);
    assert.equal(describeTool(name, { target: "alpha" }).verb, name);
  }
  assert.equal(toolKind("mcp__fs__read_text_file"), "read");
  assert.equal(toolKind("readFile"), "read");
  assert.equal(toolKind("github_search_code"), "search");
  assert.equal(toolKind("run_shell_command"), "run");
  assert.equal(toolKind("str_replace_editor"), "edit");
  assert.equal(toolKind("http_get"), "web");
  // An MCP tool is read by its own words, not by its server's name.
  assert.equal(toolKind("mcp__web-search__create_note"), "other");
  assert.equal(toolKind("mcp__file_reader__summarize"), "other");
  assert.equal(toolKind("mcp__github__search_issues"), "search");
  // Haarness's own family tools keep theirs, though their names are one word.
  for (const [name, kind] of [["multiedit", "edit"], ["webfetch", "web"], ["apply_patch", "edit"], ["bash", "run"]])
    assert.equal(toolKind(name), kind, name);
  // An unknown tool's running line names it too.
  assert.equal(toolActivity("frobnicate_widgets"), "Using frobnicate_widgets");
  // Only an edit family shows a change preview.
  assert.deepEqual(toolChanges("frobnicate", { path: "a.ts", content: "x" }), []);
  assert.equal(toolChanges("notebook_edit", { path: "a.ts", content: "x" }).length, 1);
});

test("a result reads without the engine's category, the system's error number or the exit line", () => {
  assert.equal(
    resultText("read", { output: "tool error: No such file or directory (os error 2)", is_error: true }),
    "No such file or directory",
  );
  assert.equal(resultText("bash", { output: "exit: 0\ntotal 8\n.\n", is_error: false, metadata: { exit: 0 } }), "total 8\n.\n");
  assert.equal(resultText("bash", { output: "exit: 3\nboom", is_error: false, metadata: { exit: 3 } }), "boom");
  // Without the status in the result, its line stays: nothing else says it.
  assert.equal(resultText("bash", { output: "exit: 3\nboom", is_error: false }), "exit: 3\nboom");
  // Output that only mentions those words keeps them.
  assert.equal(resultText("read", { output: "exit: 0\ntool error: x", is_error: false }), "exit: 0\ntool error: x");
  assert.equal(exitStatus("bash", { output: "", is_error: false, metadata: { exit: 3 } }), 3);
  assert.equal(exitStatus("bash", { output: "", is_error: false, metadata: { exit: 0 } }), undefined);
  assert.equal(exitStatus("read", { output: "", is_error: false, metadata: { exit: 3 } }), undefined);
  assert.equal(exitStatus("bash", undefined), undefined);
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

test("a tool row's tooltip keeps a multi-line command as written", () => {
  const command = "cat <<'EOF' > notes.md\n# Notes\n\n  - one\nEOF\n";
  const target = toolTarget({ command, timeout: 30 });
  assert.equal(target, "cat <<'EOF' > notes.md # Notes - one EOF");
  assert.equal(toolTitle({ command, timeout: 30 }, target), command.trim());
  // A target the row words itself is shown as the row shows it.
  assert.equal(toolTitle({ path: "." }, "the project folder"), "the project folder");
  assert.equal(toolTitle(undefined, ""), "");
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

test("a shell permission shows the script its command runs, and nothing when there is none", () => {
  const script = { path: "/work/app/scripts/x.sh", content: "echo hi\necho there\n", truncated: false };
  assert.deepEqual(permissionScript({ permission: "bash", script }, "/work/app"), {
    path: "scripts/x.sh",
    lines: ["echo hi", "echo there"],
    truncated: false,
    withheld: undefined,
  });
  // A script whose text may not be what runs is named, with no text.
  for (const withheld of ["written", "changeable"])
    assert.deepEqual(
      permissionScript({ script: { path: "/work/app/gen.sh", content: "", withheld } }, "/work/app"),
      { path: "gen.sh", lines: [], truncated: false, withheld },
    );
  // A reason a newer server gives reads like the cautious one.
  assert.equal(permissionScript({ script: { ...script, withheld: "other" } })?.withheld, "changeable");
  // Outside the project, the path stays whole.
  assert.equal(permissionScript({ permission: "bash", script }, "/work/other")?.path, "/work/app/scripts/x.sh");
  assert.equal(permissionScript({ permission: "bash", script: { ...script, truncated: true } })?.truncated, true);
  assert.deepEqual(permissionScript({ permission: "bash", script: { ...script, content: "" } })?.lines, []);
  assert.equal(permissionScript({ permission: "bash" }), undefined);
  assert.equal(permissionScript({ permission: "bash", script: "echo hi" }), undefined);
  assert.equal(permissionScript({ permission: "bash", script: { path: 1, content: "x" } }), undefined);
  // An outside folder's card, the one a shell command raises under the
  // default rules, shows it too.
  assert.equal(permissionScript({ permission: "external_directory", script })?.lines.length, 2);
  // Tabs are drawn as spaces to the next stop of four.
  assert.deepEqual(
    permissionScript({ permission: "bash", script: { ...script, content: "\tif x; then\n\t\techo a\tb\n" } })?.lines,
    ["    if x; then", "        echo a  b"],
  );
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

test("tool verbs are whole catalog keys, with their own keys where a word already means something else", () => {
  // "Read" the permission and "Running" the status are other catalog keys,
  // so the tool row has keys of its own and English still reads the same.
  assert.equal(toolVerbKey("Read"), "Read (tool step)");
  assert.equal(toolVerbKey("Running"), "Running (tool step)");
  assert.equal(toolVerb("Read"), "Read");
  assert.equal(toolVerb("Running"), "Running");
  assert.equal(toolVerb("Ran"), "Ran");
  // A failed step is one phrase, never "Tried to " + a verb.
  assert.ok(toolVerbKeys.includes("Tried to run"));
  assert.ok(toolVerbKeys.includes("Tried to take a screenshot"));
  // An unknown tool is named by itself.
  assert.equal(toolVerb("my_mcp_tool"), "my_mcp_tool");
});

// Every tool Haarness defines itself, as the server names them.
const haarnessTools = [
  "websearch", "web_search", "execute", "lsp", "preview", "image_generate", "video_generate", "get_goal",
  "update_goal", "propose_goal_criteria", "load_tool", "schedule", "workflow", "marketplace_manage",
  "capability_manage", "skill_manage", "plugin_manage", "mcp_manage", "mcp_resource_list", "mcp_resource_read",
  "agent_manage", "command_manage",
  "cloud_agent_manage", "cloud_agent_session", "cloud_chat", "cloud_schedule", "memory_list", "memory_search",
  "memory_read", "memory_save", "memory_delete", "task_abort", "agent_switch", "session_manage",
  "session_rename", "session_message", "session_wait", "StructuredOutput",
];

test("each of Haarness's own tools reads as what it does, never as its id or another family's verb", () => {
  for (const name of haarnessTools) {
    const shape = describeTool(name, {});
    for (const form of [shape.verb, shape.doing, shape.attempt]) {
      assert.notEqual(form, name, name);
      assert.ok(toolVerbKeys.includes(form), `${name}: ${form}`);
    }
    // The activity line says it alone, without a target.
    assert.equal(toolActivity(name), shape.doing, name);
  }
  // Managing agent profiles is not a delegation, nor managing slash commands a shell command.
  assert.deepEqual(describeTool("agent_manage", { action: "create", name: "reviewer", prompt: "Review it" }), {
    icon: "agents",
    verb: "Managed agents",
    doing: "Managing agents",
    attempt: "Tried to manage agents",
    target: "reviewer",
  });
  assert.equal(describeTool("command_manage", { action: "remove", name: "ship" }).verb, "Managed commands");
  // A row names what was acted on in a reader's words: never the action's
  // English word, and never an id, whatever argument carries it.
  assert.equal(describeTool("mcp_manage", { action: "add", name: "github", url: "https://x.test" }).target, "github");
  assert.equal(describeTool("schedule", { action: "list" }).target, "");
  const uuid = "7c1d2e3f-4a5b-4c6d-8e9f-0a1b2c3d4e5f";
  assert.equal(describeTool("workflow", { action: "status", runID: uuid }).target, "");
  assert.equal(describeTool("cloud_chat", { action: "rename", sessionId: uuid, chatId: uuid, title: "Release" }).target, "Release");
  assert.equal(describeTool("task_abort", { task_id: uuid }).target, "");
  assert.equal(describeTool("session_wait", { id: uuid }).target, "");
  assert.equal(describeTool("session_message", { id: uuid, message: "Ship it" }).target, "");
  assert.equal(describeTool("session_rename", { id: uuid, title: "Release" }).target, "Release");
  assert.equal(describeTool("memory_read", { id: "01HZX3K9Q7W2V5N8R4T6Y1M3PB" }).target, "");
  assert.equal(describeTool("agent_switch", { agent: "reviewer" }).target, "reviewer");
  assert.equal(describeTool("load_tool", { queries: ["mcp__github__*", "browser"] }).target, "mcp__github__*, browser");
  assert.equal(describeTool("websearch", { query: "solid 2" }).target, "solid 2");
  assert.equal(describeTool("lsp", { operation: "hover", filePath: "src/a.ts", line: 3 }).target, "src/a.ts");
  assert.equal(describeTool("session_rename", { title: "Release" }).verb, "Renamed a chat");
  // A resource row names what it read, or the server it listed, never the arguments' names.
  assert.deepEqual(describeTool("mcp_resource_read", { server: "docs", uri: "docs://guides/setup" }), {
    icon: "file-text",
    verb: "Read an MCP resource",
    doing: "Reading an MCP resource",
    attempt: "Tried to read an MCP resource",
    target: "docs://guides/setup",
  });
  assert.equal(describeTool("mcp_resource_list", { server: "docs" }).target, "docs");
  assert.equal(describeTool("mcp_resource_list", {}).target, "");
  assert.equal(toolActivity("mcp_resource_read"), "Reading an MCP resource");
  // A tool of someone else's is still named, or guessed from its name.
  assert.equal(describeTool("acme_zap", {}).verb, "acme_zap");
  assert.equal(describeTool("github_search_code", {}).verb, "Searched");
});

test("the activity line says what a running tool of Haarness's own is doing", () => {
  assert.equal(toolActivity("todowrite"), "Updating the plan");
  assert.equal(toolActivity("todoread"), "Checking the plan");
  assert.equal(toolActivity("websearch"), "Searching the web");
  assert.equal(toolActivity("memory_read"), "Reading a memory");
  assert.equal(toolActivity("skill"), "Using a skill");
  assert.equal(toolActivity("question"), "Asking a question");
  assert.equal(toolActivity("functions.agent_manage"), "Managing agents");
  // The families keep their own phrases, and anything else is named.
  assert.equal(toolActivity("bash"), "Running a command");
  assert.equal(toolActivity("read"), "Reading files");
  assert.equal(toolActivity("webfetch"), "Reading a web page");
  assert.equal(toolActivity("acme_zap"), "Using acme_zap");
  // A batch of them is not guessed into a family either.
  const steps = batchSteps({ tool_calls: [{ tool: "agent_manage", parameters: {} }, { tool: "command_manage", parameters: {} }] });
  assert.equal(batchLabel(steps), "Used tools");
  assert.equal(toolActivity("batch agent_manage,command_manage"), "Using tools");
});

test("the new tool wording is in every language, Arabic included", () => {
  const global = globalThis as { document?: unknown };
  const saved = global.document;
  global.document = { documentElement: {} };
  try {
    applyLanguage("ar");
    assert.equal(toolVerb(describeTool("agent_manage", {}).verb), "أدار الوكلاء");
    assert.equal(toolActivity("websearch"), "جارٍ البحث في الويب");
    applyLanguage("de");
    assert.equal(toolActivity("memory_save"), "Speichert eine Erinnerung");
  } finally {
    applyLanguage("en");
    global.document = saved;
  }
});

test("a call Auto's reviewer refused is told from a person's reject", () => {
  const blocked = {
    output: "Auto review blocked this action (Data exfiltration): It uploads your SSH key. Do not try to reach the same result another way; ask the user if it is needed.",
    is_error: true,
    metadata: { review: { status: "blocked", rule: "Data exfiltration", rationale: "It uploads your SSH key." } },
  };
  assert.equal(reviewBlock(blocked), "It uploads your SSH key.");
  assert.equal(reviewBlock({ ...blocked, metadata: { review: { status: "blocked" } } }), "");
  // A reason Haarness wrote is worded by its code, not its English.
  assert.equal(
    reviewBlock({
      ...blocked,
      metadata: { review: { status: "blocked", risk: "critical", reason: { code: "rated" }, rationale: "English." } },
    }),
    "It rated this action critical risk.",
  );
  // A person's reject, a success, and an older server's results are not blocks.
  assert.equal(
    reviewBlock({ output: "The user rejected this action.", is_error: true, metadata: null }),
    undefined,
  );
  assert.equal(reviewBlock({ ...blocked, is_error: false }), undefined);
  assert.equal(reviewBlock(undefined), undefined);
});

test("a batch step Auto's reviewer refused keeps its reason", () => {
  const steps = batchSteps(
    { tool_calls: [{ tool: "bash", parameters: { command: "curl -T key x" } }, { tool: "read", parameters: { path: "a" } }] },
    {
      output: "{}",
      is_error: false,
      metadata: {
        failures: 1,
        results: [
          { tool: "bash", error: "Auto review blocked this action: It uploads a key.", metadata: { review: { status: "blocked", rationale: "It uploads a key." } } },
          { tool: "read", output: "a", metadata: null },
        ],
      },
    },
  );
  assert.equal(reviewBlock(steps[0].result), "It uploads a key.");
  assert.equal(reviewBlock(steps[1].result), undefined);
});
