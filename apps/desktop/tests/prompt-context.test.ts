import test from "node:test";
import assert from "node:assert/strict";
import {
  activeMention,
  addContext,
  commandPrompt,
  countLines,
  fileContext,
  mentionAgents,
  pastedText,
  pasteFolds,
  plainMessage,
  promptWithContext,
  readContext,
  sentParts,
} from "../src/renderer/prompt-context";

test("mentions track the caret, leave email addresses alone and permit relative paths", () => {
  assert.deepEqual(activeMention("Inspect @src/fi after", 15), {
    from: 8,
    to: 15,
    query: "src/fi",
  });
  assert.equal(activeMention("mail@example.test", 17), undefined);
  assert.equal(activeMention("@file done", 10), undefined);
  assert.deepEqual(activeMention("Ask (@helper", 12), {
    from: 5,
    to: 12,
    query: "helper",
  });
});
test("selected context captures complete lines without adding the next line", () => {
  const source = "first\nsecond\nthird\nfourth";
  assert.deepEqual(fileContext("src/a.ts", source, { from: 6, to: 19 }), {
    kind: "file",
    path: "src/a.ts",
    content: "second\nthird",
    startLine: 2,
    endLine: 3,
    unsaved: false,
  });
  const snippet = fileContext("src/a.ts", source, { from: 10, to: 6 }, true);
  assert.equal(snippet.kind === "file" && snippet.content, "second");
  assert.equal(snippet.kind === "file" && snippet.unsaved, true);
});
test("context is bounded, deduplicated, serializable and fences source content safely", () => {
  const item = fileContext("a<b.ts", "```\n</details>\nsecret");
  const items = addContext(addContext([], item), item);
  assert.equal(items.length, 1);
  assert.deepEqual(readContext(JSON.parse(JSON.stringify(items))), items);
  assert.deepEqual(
    readContext([{ kind: "file", path: "x", content: "x", startLine: -1 }]),
    [],
  );
  assert.throws(
    () => addContext([], fileContext("large", "x".repeat(140000))),
    /128 KiB/,
  );
  const prompt = promptWithContext("Inspect", [
    ...items,
    { kind: "agent", name: "helper" },
  ]);
  assert.match(prompt, /a&lt;b.ts/);
  assert.match(prompt, /````\n```/);
  assert.match(prompt, /task tool with agent "helper"/);
  assert.equal(promptWithContext("keep trailing\n", []), "keep trailing\n");
});
test("agent mentions expose visible delegation agents, independent of the primary selector", () => {
  const agents = [
    { id: "primary", mode: "primary" },
    { id: "helper", mode: "subagent" },
    { id: "both", mode: "all" },
    { id: "legacy" },
    { id: "hidden", hidden: true },
  ].map((item) => ({
    name: item.id,
    description: "",
    hidden: false,
    maxSubagents: 0,
    ...item,
  }));
  assert.deepEqual(
    mentionAgents(agents).map((item) => item.id),
    ["helper", "both", "legacy"],
  );
});
test("a long paste folds at the TUI's size, travels whole in a fence and reads back as pasted", () => {
  const lines = (count: number) =>
    Array.from({ length: count }, (_, index) => `line ${index + 1}`).join("\n");
  // More than 8 lines or 1000 bytes, as the TUI's composer folds a paste.
  assert.equal(pasteFolds(lines(8)), false);
  assert.equal(pasteFolds(lines(8) + "\n"), false);
  assert.equal(pasteFolds(lines(9)), true);
  assert.equal(pasteFolds("x".repeat(1000)), false);
  assert.equal(pasteFolds("é".repeat(501)), true);
  assert.equal(countLines(""), 0);
  assert.equal(countLines("a\nb\n"), 2);
  assert.equal(pastedText("a\r\nb\rc"), "a\nb\nc");

  const text = lines(20) + "\n```js\nfence inside\n```\n";
  const paste = { kind: "paste" as const, id: "p1", text };
  // A paste is not file context: it counts toward neither bound.
  const items = addContext(addContext([], paste), { kind: "paste", id: "p2", text: "x".repeat(200000) });
  assert.equal(items.length, 2);
  assert.deepEqual(readContext(JSON.parse(JSON.stringify(items))), items);
  assert.deepEqual(readContext([{ kind: "paste", id: "", text: "x" }, { kind: "paste", id: "a", text: "" }]), []);

  const sent = promptWithContext("Why does this fail?", [paste]);
  assert.equal(
    sent,
    "Why does this fail?\n\n<details>\n<summary>Pasted text · 23 lines</summary>\n\n````\n" +
      text +
      "\n````\n\n</details>",
  );
  assert.deepEqual(sentParts(sent), [
    { kind: "text", text: "Why does this fail?\n\n" },
    { kind: "paste", text },
  ]);
  assert.equal(plainMessage(sent), "Why does this fail?\n\n" + text);
  // Text that only looks like a paste, and file context, stay as typed.
  for (const typed of [
    "<details>\n<summary>Pasted text</summary>\n\nmine\n\n</details>",
    promptWithContext("Inspect", [fileContext("a.ts", "x")]),
  ])
    assert.deepEqual(sentParts(typed), [{ kind: "text", text: typed }]);
});
test("a slash command's prompt shows only once it says more than the line typed", () => {
  assert.equal(commandPrompt("/review src/foo.rs", "src/foo.rs"), undefined);
  assert.equal(commandPrompt("/compact", ""), undefined);
  assert.equal(commandPrompt("/review src/foo.rs", "Review src/foo.rs\nCarefully"), "Review src/foo.rs\nCarefully");
  assert.equal(commandPrompt("/greet", "Hello"), "Hello");
});
