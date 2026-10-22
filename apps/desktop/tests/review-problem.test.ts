import { test } from "node:test";
import assert from "node:assert/strict";
import { reviewProblem } from "../src/renderer/review-problem";

test("review failures Review explains itself are told apart from the server's other text", () => {
  assert.equal(
    reviewProblem("tool error: Git diff failed: workspace is not inside a Git worktree: /tmp/x"),
    "repository",
  );
  assert.equal(
    reviewProblem(
      "tool error: Git diff failed: this clone has not downloaded the files the review needs, and the review does not download them while project configuration is not trusted; run git backfill (Git 2.49 or later), then try again",
    ),
    "download",
  );
  assert.equal(reviewProblem("tool error: Git diff failed: read Git patch: boom"), undefined);
});
