import test from "node:test";
import assert from "node:assert/strict";
import {
  activeMention,
  addContext,
  commandPrompt,
  contextLabel,
  countLines,
  fileContext,
  keptContext,
  mentionAgents,
  pasteSize,
  pastedText,
  pasteFolds,
  plainMessage,
  promptWithContext,
  readContext,
  sentParts,
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
test("only a paste sent at the top of a message reads as one, not one inside a file or a typed fence", () => {
  const block = promptWithContext("", [{ kind: "paste", id: "p", text: "secret-ish\nline2" }]);
  // A file whose content shows a paste block (a TUI /export, a doc of the
  // format) stays one file snapshot: the bubble, Copy and Undo leave it whole.
  const file = fileContext("docs/format.md", "# Format\n\n" + block + "\n\nend of doc");
  const withFile = promptWithContext("Look at this", [file]);
  assert.deepEqual(sentParts(withFile), [{ kind: "text", text: withFile }]);
  assert.equal(plainMessage(withFile), withFile);
  // A real paste beside that file is still found, after it and before it.
  const paste = { kind: "paste" as const, id: "q", text: "real\npaste" };
  for (const items of [[file, paste], [paste, file]]) {
    const sent = promptWithContext("Both", items);
    const parts = sentParts(sent);
    assert.deepEqual(parts.filter((part) => part.kind === "paste"), [{ kind: "paste", text: "real\npaste" }]);
    assert.equal(parts.map((part) => part.text).join(""), plainMessage(sent));
    assert.ok(plainMessage(sent).includes("end of doc") && plainMessage(sent).includes("secret-ish"));
  }
  // Typed into a code fence, with backticks or tildes, it is text.
  for (const fence of ["````md", "~~~"]) {
    const typed = `Example:\n\n${fence}\n${block}\n${fence.slice(0, fence[0] === "~" ? 3 : 4)}\nthanks`;
    assert.deepEqual(sentParts(typed), [{ kind: "text", text: typed }]);
  }
  // Once the typed fence closes, a paste after it counts again.
  const after = "```\ncode\n```\n\n" + block;
  assert.deepEqual(sentParts(after), [{ kind: "text", text: "```\ncode\n```\n\n" }, { kind: "paste", text: "secret-ish\nline2" }]);
  // A paste whose text holds a closing tag and a fake block keeps it all.
  const tricky = "a\n```\nb\n```\n\n</details>\n" + promptWithContext("", [{ kind: "paste", id: "f", text: "fake" }]) + "\nz";
  assert.deepEqual(sentParts(promptWithContext("hi", [{ kind: "paste", id: "t", text: tricky }])), [
    { kind: "text", text: "hi\n\n" },
    { kind: "paste", text: tricky },
  ]);
});
test("a draft keeps what fits in one stored value, and a big paste says its size", () => {
  const file = fileContext("a.ts", "x");
  const small = { kind: "paste" as const, id: "small", text: "small\npaste" };
  const big = { kind: "paste" as const, id: "big", text: "y".repeat(2_200_000) };
  assert.deepEqual(keptContext([file, small]), { kept: [file, small], dropped: [] });
  // The paste that does not fit stays out of storage; everything else is kept.
  assert.deepEqual(keptContext([file, big, small]), { kept: [file, small], dropped: ["big"] });
  assert.equal(pasteSize("z".repeat(99_000), "en"), "");
  assert.equal(pasteSize("z".repeat(240_000), "en"), "240 kB");
  assert.equal(pasteSize("z".repeat(2_400_000), "en"), "2.4 MB");
});
test("picked elements and pastes sit side by side: only non-paste items count toward the limits, and an element block is read whole", () => {
  const picked = elementContext(
    {
      url: "http://localhost:3000/",
      tag: "details#help",
      name: "",
      text: "",
      // Page HTML that holds a closing tag on its own and a paste-shaped block.
      html: "<details>\n<summary>Pasted text · 2 lines</summary>\n\n```\nnot a paste\n```\n\n</details>",
      selector: "#help",
      width: 10,
      height: 10,
    },
    "tab_1",
  );
  const paste = { kind: "paste" as const, id: "p", text: "real\npaste" };
  const sent = promptWithContext("Look", [picked, paste]);
  const parts = sentParts(sent);
  assert.deepEqual(parts.filter((part) => part.kind === "paste"), [{ kind: "paste", text: "real\npaste" }]);
  assert.ok(parts[0].kind === "text" && parts[0].text.includes("not a paste") && parts[0].text.includes("Page element: "));
  assert.equal(plainMessage(sent), sent.replace(/<details>\n<summary>Pasted text · 2 lines<\/summary>\n\n```\nreal\npaste\n```\n\n<\/details>$/, "real\npaste"));
  // Fifty picked items is the bound; pastes beside them never count.
  const many = Array.from({ length: 50 }, (_, index) => ({ kind: "agent" as const, name: `agent-${index}` }));
  let items = many.reduce((list, item) => addContext(list, item), [] as ReturnType<typeof addContext>);
  items = addContext(items, paste);
  assert.equal(items.length, 51);
  assert.throws(() => addContext(items, { kind: "agent", name: "one-more" }), /up to 50 context items/);
});
