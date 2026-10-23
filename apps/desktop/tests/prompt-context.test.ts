import test from "node:test";
import assert from "node:assert/strict";
import {
  activeMention,
  addContext,
  carriedResource,
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
  resourceMentions,
  resourceToRead,
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
test("a resource mention travels as an address, one per server, and survives a stored draft", () => {
  const docs = { kind: "resource" as const, server: "docs", uri: "docs://setup", name: "setup", description: "How to run it" };
  const wiki = { ...docs, server: "wiki", description: undefined };
  // Two servers with one URI are two chips; the same one again replaces it.
  const items = addContext(addContext(addContext([], docs), wiki), { ...docs });
  assert.deepEqual(items.map(contextLabel), ["@setup", "@setup"]);
  assert.deepEqual(items.map((item) => item.kind === "resource" && item.server), ["wiki", "docs"]);
  // Nothing of it is written into the text: the server reads it at send.
  assert.equal(promptWithContext("Compare", items), "Compare");
  assert.deepEqual(resourceMentions([{ kind: "agent", name: "helper" }, ...items]), [
    { server: "wiki", uri: "docs://setup", name: "setup" },
    { server: "docs", uri: "docs://setup", name: "setup" },
  ]);
  const stored = JSON.parse(JSON.stringify(items));
  assert.deepEqual(readContext(stored), stored);
  assert.deepEqual(readContext([{ kind: "resource", server: "docs", uri: "", name: "x" }]), []);
  assert.deepEqual(readContext([{ kind: "resource", server: "docs", uri: "docs://a", name: "a", mimeType: 3 }]), []);
});
test("what the server read for a mentioned resource is one block of the sent message", () => {
  const block =
    "<details>\n<summary>MCP resource: setup · docs:docs://setup</summary>\n\n````\n```sh\nrun\n```\n<details>\n<summary>Pasted text</summary>\n\n```\nnot a paste\n```\n\n</details>\n````\n\n</details>";
  const paste = promptWithContext("", [{ kind: "paste", id: "p", text: "pasted" }]);
  const unread =
    "<details>\n<summary>MCP resource: wiki:docs://&lt;gone&gt; (not read)</summary>\n\n```\nCould not read MCP resource wiki:docs://<gone>: MCP server wiki is not connected\n```\n\n</details>";
  const sent = `Compare\n\n${block}\n\n${paste}\n\n${unread}`;
  // The paste-shaped text inside the resource stays part of it.
  assert.deepEqual(sentParts(sent), [
    { kind: "text", text: "Compare\n\n" },
    {
      kind: "resource",
      text: block,
      name: "setup",
      address: "docs:docs://setup",
      content:
        "```sh\nrun\n```\n<details>\n<summary>Pasted text</summary>\n\n```\nnot a paste\n```\n\n</details>",
      failed: false,
    },
    { kind: "text", text: "\n\n" },
    { kind: "paste", text: "pasted" },
    { kind: "text", text: "\n\n" },
    {
      kind: "resource",
      text: unread,
      name: "wiki:docs://<gone>",
      address: "wiki:docs://<gone>",
      content: "Could not read MCP resource wiki:docs://<gone>: MCP server wiki is not connected",
      failed: true,
    },
  ]);
  // Copy keeps the blocks as they were sent.
  assert.equal(plainMessage(sent), `Compare\n\n${block}\n\npasted\n\n${unread}`);
});
test("a resource a sent or queued message brings back is a chip that sends what was read, without reading again", () => {
  const block =
    "<details>\n<summary>MCP resource: setup · docs:docs://setup</summary>\n\n```\n# Setup\n```\n\n</details>";
  const unread =
    "<details>\n<summary>MCP resource: wiki:docs://x (not read)</summary>\n\n```\nMCP server wiki is not connected\n```\n\n</details>";
  const parts = sentParts(`Compare\n\n${block}\n\n${unread}`);
  const chips = parts.flatMap((part) => (part.kind === "resource" ? [carriedResource(part)] : []));
  assert.deepEqual(chips, [
    { kind: "resource", server: "docs", uri: "docs://setup", name: "setup", block },
    { kind: "resource", server: "wiki", uri: "docs://x", name: "docs://x", block: unread },
  ]);
  assert.deepEqual(chips.map(contextLabel), ["@setup", "@docs://x"]);
  // Sent again as it was, and not as a mention to read.
  assert.equal(promptWithContext("Compare", chips), `Compare\n\n${block}\n\n${unread}`);
  assert.deepEqual(resourceMentions(chips), []);
  assert.deepEqual(readContext(JSON.parse(JSON.stringify(chips))), chips);
  // What was read counts toward no context limit, as a paste does not.
  const big = { ...chips[0], uri: "docs://big", block: "x".repeat(200 * 1024) };
  assert.doesNotThrow(() => addContext(chips, big));
});
test("a resource that could not be read comes back from Undo as one to read again", () => {
  const unread =
    "<details>\n<summary>MCP resource: logo · wiki:docs://logo (not read)</summary>\n\n```\na rule keeps this resource from the model\n```\n\n</details>";
  const [part] = sentParts(unread);
  assert.equal(part.kind, "resource");
  if (part.kind !== "resource") return;
  const chip = resourceToRead(part);
  assert.deepEqual(chip, { kind: "resource", server: "wiki", uri: "docs://logo", name: "logo" });
  assert.deepEqual(resourceMentions([chip]), [{ server: "wiki", uri: "docs://logo", name: "logo" }]);
});
