import { test } from "node:test";
import assert from "node:assert/strict";
import { createProjectQueue } from "../src/main/project-queue";

test("background opens read committed settings while other projects stay usable", async () => {
  const run = createProjectQueue();
  let release!: () => void;
  const paused = new Promise<void>((resolve) => {
    release = resolve;
  });
  let trust = true;
  const changing = run("project", async () => {
    await paused;
    trust = false;
  });
  const background = run("project", async () => trust);
  assert.equal(await run("other-project", async () => "available"), "available");
  release();
  await changing;
  assert.equal(await background, false);
  await assert.rejects(
    run("project", async () => {
      throw new Error("active turn");
    }),
  );
  assert.equal(await run("project", async () => trust), false);
});
