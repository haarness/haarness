import test from "node:test";
import assert from "node:assert/strict";
import { createAgentCursor, cursorLabel, cursorLabelKeys, cursorScript } from "../src/agent-cursor";
import { createCdpEvents } from "../src/cdp";

test("the cursor runs its own install script in the host, in CSS pixels", async () => {
  const runs: string[] = [];
  const cursor = createAgentCursor({ run: async (code) => void runs.push(code), zoom: () => 2 });
  await cursor.move({ x: 100, y: 50 }, cursorLabel("click", "Buy"));
  assert.ok(runs[0].startsWith(cursorScript + ";"));
  assert.ok(runs[0].endsWith(`globalThis.__haarnessCursor.move(50,25,"Clicking “Buy”",false,null)`));
  // A failing host never fails the action, and captures run with the overlay hidden.
  const broken = createAgentCursor({ run: async () => { throw new Error("navigating"); }, zoom: () => 1 });
  assert.equal(await broken.hidden(async () => "shot"), "shot");
  assert.equal(await createAgentCursor(undefined).hidden(async () => "plain"), "plain");
  assert.ok(cursorLabelKeys.includes("Scrolling down"));
});

test("transport events pass only from the tab and its announced iframes", () => {
  const events = createCdpEvents();
  const seen: (string | undefined)[] = [];
  const off = events.on("Page.frameNavigated", (_params, sessionID) => void seen.push(sessionID));
  events.receive("Page.frameNavigated", {}, "");
  events.receive("Page.frameNavigated", {}, "worker");
  events.receive("Target.attachedToTarget", { sessionId: "frame", targetInfo: { type: "iframe" } });
  events.receive("Target.attachedToTarget", { sessionId: "worker", targetInfo: { type: "service_worker" } });
  events.receive("Page.frameNavigated", {}, "frame");
  events.receive("Page.frameNavigated", {}, "worker");
  events.receive("Target.detachedFromTarget", { sessionId: "frame" });
  events.receive("Page.frameNavigated", {}, "frame");
  off();
  events.receive("Page.frameNavigated", {});
  assert.deepEqual(seen, [undefined, "frame"]);
});
