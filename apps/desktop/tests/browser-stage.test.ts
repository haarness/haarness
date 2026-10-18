import { test } from "node:test";
import assert from "node:assert/strict";
import { agentReveal, awayChats, putAway, type BrowserStage } from "../src/renderer/browser-stage";

const stage = (patch: Partial<BrowserStage> = {}): BrowserStage => ({
  chat: "local:/work\u0000chat-1",
  shown: false,
  hidden: true,
  launcher: false,
  tab: true,
  ...patch,
});
const shown = stage({ shown: true, hidden: false });

test("hiding the side panel or closing the Browser's tab while it shows puts the Browser away", () => {
  assert.equal(putAway(shown, stage({ hidden: true })), true);
  // Its tab closed and the side panel fell back to another panel or the launcher.
  assert.equal(putAway(shown, stage({ hidden: false, tab: false })), true);
  assert.equal(putAway(shown, stage({ hidden: false, launcher: true, tab: false })), true);
});

test("the Browser on screen is never put away, even with no page to show", () => {
  assert.equal(putAway(stage(), shown), false);
  assert.equal(putAway(undefined, shown), false);
});

test("another panel, the launcher, another chat or a panel hidden while the Browser was not showing leave it as it was", () => {
  // Review picked in the side panel: the Browser still has its tab there.
  assert.equal(putAway(shown, stage({ hidden: false })), undefined);
  // + pressed to pick another panel.
  assert.equal(putAway(shown, stage({ hidden: false, launcher: true })), undefined);
  // Another chat, whose side panel may be hidden.
  assert.equal(putAway(shown, stage({ chat: "local:/work\u0000chat-2" })), undefined);
  // The side panel hidden while Review showed.
  assert.equal(putAway(stage({ hidden: false }), stage()), undefined);
  assert.equal(putAway(undefined, stage()), undefined);
});

test("an agent's page opens a hidden side panel, or its launcher, unless the Browser was put away", () => {
  assert.equal(agentReveal(stage(), false), "show");
  assert.equal(agentReveal(stage({ hidden: false, launcher: true }), false), "show");
  assert.equal(agentReveal(stage(), true), "notify");
  assert.equal(agentReveal(stage({ hidden: false, launcher: true }), true), "notify");
});

test("an agent's page never takes the side panel from another panel and does nothing while the Browser shows", () => {
  assert.equal(agentReveal(stage({ hidden: false }), false), "notify");
  assert.equal(agentReveal(stage({ hidden: false }), true), "notify");
  assert.equal(agentReveal(shown, false), "none");
  assert.equal(agentReveal(shown, true), "none");
});

test("each chat keeps its own put-away Browser", () => {
  const other = "local:/work\u0000chat-2";
  let away: ReadonlySet<string> = new Set();
  away = awayChats(away, shown, stage({ hidden: true }));
  assert.deepEqual([...away], [shown.chat]);
  // Another chat, its side panel still hidden: its pages may still open the Browser.
  away = awayChats(away, stage(), stage({ chat: other }));
  assert.equal(away.has(other), false);
  assert.equal(agentReveal(stage({ chat: other }), away.has(other)), "show");
  // That chat's Browser shows; the first chat's stays put away.
  away = awayChats(away, stage({ chat: other }), stage({ chat: other, shown: true, hidden: false }));
  assert.deepEqual([...away], [shown.chat]);
  // Back in the first chat, its Browser on screen again: no longer away.
  away = awayChats(away, stage({ chat: other, shown: true, hidden: false }), shown);
  assert.deepEqual([...away], []);
});
