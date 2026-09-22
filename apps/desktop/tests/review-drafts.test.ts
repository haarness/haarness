import test from "node:test";
import assert from "node:assert/strict";
import { createReviewStorage, type ReviewDraft } from "../src/review-drafts";

test("review delivery survives failed persistence and retries without duplicating composer text", async () => {
  const values: Record<string, unknown> = {};
  let failing = false;
  let disk: Record<string, unknown> = {};
  const storage = createReviewStorage(values, async () => {
    if (failing) throw Error("disk unavailable");
    disk = structuredClone(values);
  });
  const draft: ReviewDraft = { id: "first", text: "review", target: { connection: "project", thread: "conversation" } };
  await storage({ action: "save", scope: "source", anchor: "line", draft });
  const request = { action: "deliver" as const, scope: "source", anchor: "line", draft, current: "existing draft", note: "review note" };
  failing = true;
  await assert.rejects(storage(request));
  assert.equal((await storage({ action: "load", scope: "source" })).drafts.line.id, "first");
  failing = false;
  assert.equal((await storage(request)).text, "existing draft\n\nreview note");
  const restarted = createReviewStorage(disk, async () => {});
  assert.equal((await restarted(request)).text, "existing draft\n\nreview note");
  assert.deepEqual((await restarted({ action: "load", scope: "source" })).drafts, {});
});

test("late delivery cannot clear a newer draft or move the receipt to another conversation", async () => {
  const values: Record<string, unknown> = {};
  const storage = createReviewStorage(values, async () => {});
  const old = { id: "old", text: "old comment", target: { connection: "project", thread: "first" } };
  const newer = { id: "new", text: "newer comment" };
  await storage({ action: "save", scope: "first", anchor: "same line", draft: newer });
  await storage({ action: "pin", scope: "first", anchor: "same line", draft: old });
  const request = { action: "deliver" as const, scope: "first", anchor: "same line", draft: old, note: old.text, current: "" };
  await storage(request);
  await storage({ action: "remove", scope: "first", anchor: "same line", id: old.id });
  assert.equal((await storage({ action: "load", scope: "first" })).drafts["same line"].id, "new");
  assert.deepEqual((await storage({ action: "load", scope: "other" })).drafts, {});
  await assert.rejects(storage({ ...request, draft: { ...old, target: { connection: "other", thread: "second" } } }));
});

test("typing after a failed write is merged with the pending comment on retry", async () => {
  const values: Record<string, unknown> = {};
  let failing = false;
  const storage = createReviewStorage(values, async () => { if (failing) throw Error("disk full"); });
  const draft = { id: "pending", text: "comment", target: { connection: "p", thread: "t" } };
  await storage({ action: "save", scope: "source", anchor: "line", draft });
  const request = { action: "deliver" as const, scope: "source", anchor: "line", draft, current: "before", note: "comment" };
  failing = true;
  await assert.rejects(storage(request));
  values["draft:p:t"] = "edited after failure";
  failing = false;
  const result = await storage({ ...request, current: "edited after failure" });
  assert.equal(result.text, "edited after failure\n\ncomment");
  assert.equal((await storage({ ...request, current: result.text! })).text, result.text);
});
