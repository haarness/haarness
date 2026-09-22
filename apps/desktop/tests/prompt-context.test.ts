import test from "node:test";
import assert from "node:assert/strict";
import {
  activeMention,
  addContext,
  fileContext,
  mentionAgents,
  promptWithContext,
  readContext,
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
