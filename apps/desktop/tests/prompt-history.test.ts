import test from "node:test";
import assert from "node:assert/strict";
import {
  canRecall,
  matchingHistory,
  createHistoryStore,
  historyKey,
  navigateHistory,
  orderHistory,
  persistedHistory,
  prependHistory,
  readHistory,
  type PromptHistoryEntry,
} from "../src/renderer/prompt-history";
const entry = (text: string): PromptHistoryEntry => ({
  text,
  attachments: [],
  context: [],
});

test("conversation history takes priority with workspace fallback and nonconsecutive deduplication", () => {
  const values = [
    { ...entry("other newest"), thread: "b" },
    { ...entry("shared"), thread: "b" },
    { ...entry("mine"), thread: "a" },
    { ...entry("shared"), thread: "a" },
    entry("legacy"),
    { ...entry("mine"), thread: "a" },
  ];
  assert.deepEqual(orderHistory(values, "a").map(item => item.text), ["mine", "shared", "other newest", "legacy"]);
  assert.deepEqual(orderHistory(values, "b").map(item => item.text), ["other newest", "shared", "mine", "legacy"]);
  assert.equal(prependHistory(values, { ...entry("mine"), thread: "a" }).filter(item => item.text === "mine").length, 1);
});

test("failed history hydration keeps submitted prompts in memory and later merges without losing disk history", async () => {
  let failing = true;
  let saved: unknown = [entry("older saved prompt")];
  const store = createHistoryStore(async (_, value) => {
    if (value === undefined && failing) throw Error("disk unavailable");
    if (value !== undefined) saved = value;
    return saved;
  });
  await assert.rejects(store.record("fixture", entry("submitted while unavailable")));
  assert.equal(store.entries("fixture")[0].text, "submitted while unavailable");
  assert.deepEqual(readHistory(saved).map(item => item.text), ["older saved prompt"]);
  failing = false;
  await store.record("fixture", entry("next submission"));
  assert.deepEqual(readHistory(saved).map(item => item.text), ["next submission", "submitted while unavailable", "older saved prompt"]);
});

test("history stays at text boundaries and preserves normal multiline/selection movement", () => {
  assert.equal(canRecall("up", "one\ntwo", 4, 4, -1), false);
  assert.equal(canRecall("up", "one\ntwo", 0, 0, -1), true);
  assert.equal(canRecall("up", "one", 0, 3, 0), false);
  assert.equal(canRecall("down", "one", 0, 0, 0), true);
  const values = [entry("new"), entry("old")];
  assert.equal(navigateHistory(values, -1, "up"), 0);
  assert.equal(navigateHistory(values, 0, "up"), 1);
  assert.equal(navigateHistory(values, 1, "up"), undefined);
  assert.equal(navigateHistory(values, 0, "down"), -1);
  assert.equal(navigateHistory(values, -1, "down"), undefined);
});
test("history deduplicates consecutive submissions and copies complete prompt context", () => {
  const value = entry("inspect");
  value.context.push({
    kind: "file",
    path: "source.ts",
    content: "const x = 1",
  });
  value.attachments.push({
    name: "picture",
    data: "data:image/png;base64,YQ==",
  });
  const values = prependHistory([], value);
  assert.equal(prependHistory(values, value), values);
  value.context.length = 0;
  assert.equal(values[0].context.length, 1);
  assert.equal(prependHistory(values, entry(" ")), values);
  assert.equal(
    prependHistory(
      Array.from({ length: 200 }, (_, i) => entry(String(i))),
      entry("latest"),
    ).length,
    200,
  );
});
test("history keeps 200 prompts, but only the newest 100 bring back attachments or context", () => {
  const withFile = (text: string) => ({
    ...entry(text),
    context: [{ kind: "file" as const, path: "notes.md", content: "notes" }],
  });
  const withPicture = (text: string) => ({
    ...entry(text),
    attachments: [{ name: "picture", data: "data:image/png;base64,YQ==" }],
  });
  // Newest first: the two rich entries sit at places 99 and 100, the plain
  // one beside them at 101.
  let values = [
    ...Array.from({ length: 98 }, (_, i) => entry("recent " + i)),
    withFile("file at 99"),
    withPicture("picture at 100"),
    entry("plain at 101"),
    withFile("file at 102"),
  ];
  values = readHistory(values);
  assert.equal(values.length, 101);
  assert.ok(values.some((item) => item.text === "picture at 100"));
  assert.ok(!values.some((item) => item.text === "file at 102"));
  // One more prompt pushes the picture past the newest 100: it goes, and the
  // plain prompt after it stays.
  values = prependHistory(values, entry("newest"));
  const texts = values.map((item) => item.text);
  assert.ok(texts.includes("file at 99"));
  assert.ok(!texts.includes("picture at 100"));
  assert.ok(texts.includes("plain at 101"));
  assert.equal(values.length, 101);
  // Plain prompts go back as far as 200.
  for (let i = 0; i < 150; i++) values = prependHistory(values, entry("later " + i));
  assert.equal(values.length, 200);
  assert.equal(values.at(-1)?.text, "recent 48");
  assert.ok(values.slice(100).every((item) => !item.attachments.length && !item.context.length));
});
test("stored history rejects malformed values and bounds oversized media without losing in-memory entries", () => {
  assert.deepEqual(readHistory([{ text: "bad" }, entry("valid")]), [
    entry("valid"),
  ]);
  const huge = entry("large image");
  huge.attachments.push({ name: "large", data: "a".repeat(2 * 1024 * 1024) });
  assert.deepEqual(persistedHistory([huge, entry("small")]), [entry("small")]);
  assert.equal(huge.attachments.length, 1);
});
test("history hydrates before appending, serializes writes, and separates workspaces", async () => {
  const saved = new Map<string, unknown>();
  const chat = historyKey("workspace-a");
  const other = historyKey("workspace-b");
  saved.set(chat, [entry("previous")]);
  const store = createHistoryStore(async (key, value) => {
    await new Promise((resolve) => setTimeout(resolve, 2));
    if (value !== undefined) saved.set(key, value);
    return saved.get(key);
  });
  await Promise.all([
    store.record(chat, entry("one")),
    store.record(chat, entry("two")),
    store.record(other, entry("other prompt")),
  ]);
  assert.deepEqual(
    readHistory(saved.get(chat)).map((value) => value.text),
    ["two", "one", "previous"],
  );
  assert.equal(store.entries(other)[0].text, "other prompt");
  assert.deepEqual(store.entries(historyKey("workspace-c")), []);
});


test("prefix recall skips unrelated entries, preserves ordering and leaves unmatched drafts alone", () => {
  const entries = [entry("git diff"), entry("gitk"), entry("npm test"), entry("git status")];
  const matches = matchingHistory(entries, "git ");
  assert.deepEqual(matches.map(entry => entry.text), ["git diff", "git status"]);
  assert.equal(navigateHistory(matches, -1, "up"), 0);
  assert.equal(navigateHistory(matches, 0, "up"), 1);
  assert.equal(navigateHistory(matches, 0, "down"), -1);
  assert.deepEqual(matchingHistory(entries, "unrelated draft"), []);
  assert.equal(navigateHistory(matchingHistory(entries, "unrelated draft"), -1, "up"), undefined);
  assert.deepEqual(matchingHistory(entries, ""), entries);
  assert.deepEqual(matchingHistory([entry("修正 tests"), entry("fix tests")], "修正"), [entry("修正 tests")]);
});
