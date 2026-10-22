import test from "node:test";
import assert from "node:assert/strict";
import {
  activeMention,
  addContext,
  contextLabel,
  fileContext,
  mentionAgents,
  promptWithContext,
  readContext,
} from "../src/renderer/prompt-context";
import { elementContext, elementLabel, shortPage } from "../src/renderer/element-context";

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
test("a picked page element is a chip that reads as page data and survives storage", () => {
  const picked = elementContext(
    {
      url: "http://localhost:3000/settings",
      tag: "button#save.primary",
      name: "Save <changes>",
      text: "",
      html: '<button id="save">Save ```changes```</button>',
      selector: "#save",
      width: 120,
      height: 32,
    },
    "tab_00000000-0000-0000-0000-000000000000",
  );
  // Empty strings are left out; the chip names its tag and accessible name.
  assert.equal("text" in picked, false);
  assert.equal(contextLabel(picked), "button#save.primary “Save <changes>”");
  // Picking it again leaves one chip; it comes back from storage as it went in.
  const items = addContext(addContext([], picked), { ...picked });
  assert.equal(items.length, 1);
  assert.deepEqual(readContext(JSON.parse(JSON.stringify(items))), items);
  for (const broken of [
    { ...picked, tag: "" },
    { ...picked, width: -1 },
    { ...picked, html: 1 },
    { ...picked, name: 7 },
  ])
    assert.deepEqual(readContext([broken]), []);
  const prompt = promptWithContext("Make this bigger", items);
  assert.ok(prompt.startsWith("Make this bigger\n\n<details>\n<summary>Page element: button#save.primary “Save &lt;changes&gt;” (snapshot)</summary>\n\n"));
  assert.match(prompt, /in the Haarness browser \(tab tab_0{8}-/);
  assert.match(prompt, /- Page: http:\/\/localhost:3000\/settings\n- Selector: "#save"\n- Size: 120×32 px\n- Name: "Save <changes>"\n/);
  // The HTML is fenced longer than any backticks inside it.
  assert.match(prompt, /````html\n<button id="save">Save ```changes```<\/button>\n````\n\n<\/details>$/);
  // A long name is cut in the label only.
  const long = { ...picked, name: "x".repeat(100) };
  assert.equal(elementLabel(long), `button#save.primary “${"x".repeat(59)}…”`);
  assert.match(promptWithContext("", [long]), new RegExp(`- Name: "${"x".repeat(100)}"`));
  assert.equal(shortPage("https://example.com/a/b/?q=1"), "example.com/a/b?q=1");
});
