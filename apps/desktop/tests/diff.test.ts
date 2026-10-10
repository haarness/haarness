import test from "node:test";
import assert from "node:assert/strict";
import { changedSpan } from "../src/renderer/diff";

const apply = (before: string, span: ReturnType<typeof changedSpan>) =>
  before.slice(0, span.from) + span.insert + before.slice(span.to);

test("an editor update replaces only the span that changed", () => {
  const before = "a\nb\nc\nd\n";
  assert.deepEqual(changedSpan(before, "a\nB\nc\nd\n"), { from: 2, to: 3, insert: "B" });
  // Insertions and deletions at either end, and texts sharing a prefix and
  // suffix that overlap, still rebuild the new text exactly.
  for (const after of ["", "x", "a\nb\nc\nd\ne\n", "z\na\nb\nc\nd\n", "a\nd\n", "aa", before])
    assert.equal(apply(before, changedSpan(before, after)), after);
  assert.deepEqual(changedSpan("aaa", "aa"), { from: 2, to: 3, insert: "" });
  assert.deepEqual(changedSpan("", "new"), { from: 0, to: 0, insert: "new" });
});
