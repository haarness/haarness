import test from "node:test";
import assert from "node:assert/strict";
import { removalSurvivesFailedStart } from "../src/main/unlinked-worktree";

const unlinked = new Error(
  "Error: conflict: the worktree no longer links to its repository, so Git cannot check it; if the repository moved, run `git worktree repair` there, or remove the worktree to forget it (its folder stays on disk)",
);

test("only a worktree whose link broke is removed without its server", () => {
  assert.equal(removalSurvivesFailedStart("removeWorkspace", unlinked), true);
  for (const reason of [
    "Haarness server did not answer within 30 seconds",
    "The project's server is busy: 1 open terminal (npm run dev)",
    "the worktree's repository is on a disk that is not connected (/Volumes/SSD/app); connect it to use the worktree, which is kept until then",
  ])
    assert.equal(removalSurvivesFailedStart("removeWorkspace", new Error(reason)), false, reason);
  assert.equal(removalSurvivesFailedStart("resetWorkspace", unlinked), false);
});
