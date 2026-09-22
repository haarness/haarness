import test from "node:test";
import assert from "node:assert/strict";
import type { Item, Thread } from "@haarness/sdk";
import { withThreadKinds } from "../src/thread-kinds";

test("older daemons classify task links without hiding renamed forks or copying ownership", async () => {
  const threads = [
    { id: "root", parentId: null },
    { id: "child", parentId: "root" },
    { id: "fork", parentId: "root", title: "Any title" },
    { id: "fork-child", parentId: "fork", isSubagent: false },
  ] as Thread[];
  let reads = 0;
  const result = await withThreadKinds(threads, async () => {
    reads++;
    return [
      { data: { type: "task_link", child_thread_id: "child" } },
    ] as Item[];
  });
  assert.equal(reads, 1);
  assert.deepEqual(
    result.map((thread) => !!thread.isSubagent),
    [false, true, false, false],
  );
  await withThreadKinds(result, async () => {
    throw new Error("new servers need no transcript scan");
  });
});

test("background activity belongs to its parent, while an active fork stays independent", async () => {
  const { conversationRoot } = await import("../src/thread-kinds");
  const threads = [
    { id: "root" },
    { id: "child", parentId: "root", isSubagent: true },
    { id: "nested", parentId: "child", isSubagent: true },
    { id: "fork", parentId: "root", isSubagent: false },
  ] as Thread[];
  assert.equal(conversationRoot(threads, "nested"), "root");
  assert.equal(conversationRoot(threads, "fork"), "fork");
});

test("an unavailable legacy parent does not prevent the rest of the sidebar loading", async () => {
  const threads = [
    { id: "root" },
    { id: "child", parentId: "missing" },
  ] as Thread[];
  const result = await withThreadKinds(threads, async () => {
    throw new Error("Parent unavailable");
  });
  assert.equal(result.length, 2);
});
