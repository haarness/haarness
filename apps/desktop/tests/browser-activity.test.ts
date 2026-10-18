import { test } from "node:test";
import assert from "node:assert/strict";
import { createAgentActivity, POPUP_GRACE_MS } from "../src/main/browser-activity";

test("a tab is the agent's while a command runs in it and for a moment after, never while one only waits", () => {
  let now = 1_000;
  const activity = createAgentActivity(() => now);
  // A command queued behind another has not begun: a popup now is the person's.
  assert.equal(activity.working("tab_a"), false);
  activity.begin("tab_a");
  assert.equal(activity.working("tab_a"), true);
  assert.equal(activity.working("tab_b"), false);
  activity.end("tab_a");
  // A click's window.open can land just after its command returned.
  now += POPUP_GRACE_MS - 1;
  assert.equal(activity.working("tab_a"), true);
  now += 1;
  assert.equal(activity.working("tab_a"), false);
});

test("overlapping commands in one tab keep it the agent's until the last ends", () => {
  let now = 0;
  const activity = createAgentActivity(() => now);
  activity.begin("tab_a");
  activity.begin("tab_a");
  activity.end("tab_a");
  now += POPUP_GRACE_MS * 2;
  assert.equal(activity.working("tab_a"), true);
  activity.end("tab_a");
  now += POPUP_GRACE_MS;
  assert.equal(activity.working("tab_a"), false);
});
