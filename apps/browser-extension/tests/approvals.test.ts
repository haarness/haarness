import assert from "node:assert/strict";
import test from "node:test";
import { approvalLines, badgeText, parsePattern } from "../src/core/approvals";
import { capturePlan } from "../src/core/capture";
import { createAttribution } from "../src/core/downloads";
import { groupColor, groupTitle, GROUP_COLORS } from "../src/core/groups";
import { disconnectReason, pill, reconnectDelay } from "../src/core/status";

const THREAD = "0192a5f0-7c1e-7d3a-9b1e-4f2a9c3d5e61";

test("Chrome approval patterns read as plain requests", () => {
  assert.deepEqual(parsePattern(`browser_site:chrome-browsing:${THREAD}:https://x.example:8443`), { category: "browsing", origin: "https://x.example:8443" });
  assert.equal(parsePattern("browser_site:https://x.example"), undefined);
  assert.deepEqual(
    approvalLines([`chrome-scripts:${THREAD}:https://a.example`, `chrome-browsing:${THREAD}:https://a.example`, `chrome-browsing:${THREAD}:https://a.example`]),
    [{ category: "browsing", origin: "https://a.example" }, { category: "scripts", origin: "https://a.example" }],
  );
});

test("the badge shows waiting approvals first, else working chats", () => {
  assert.equal(badgeText(2, 1), "!");
  assert.equal(badgeText(3, 0), "3");
  assert.equal(badgeText(0, 0), "");
  assert.equal(badgeText(150, 0), "99");
});

test("group titles and colours follow the chat", () => {
  assert.equal(groupTitle(""), "Haarness");
  assert.equal(groupTitle("  Plan   the trip "), "Plan the trip");
  const long = groupTitle("Compare hosting plans for the new marketing site and the docs");
  assert.ok(long.length <= 40 && long.endsWith("…"), long);
  assert.equal(groupColor(THREAD), groupColor(THREAD));
  assert.ok(GROUP_COLORS.includes(groupColor("another")));
});

test("connection states and retry timing", () => {
  assert.equal(disconnectReason("Specified native messaging host not found."), "not-set-up");
  assert.equal(disconnectReason("Access to the specified native messaging host is forbidden."), "not-set-up");
  assert.equal(disconnectReason("Native host has exited."), "bridge-exited");
  assert.equal(pill({ connection: "connected", paused: true, stopped: true, update: false }), "stopped");
  assert.equal(pill({ connection: "connected", paused: false, stopped: false, update: true }), "update");
  assert.equal(pill({ connection: "not-set-up", paused: true, stopped: false, update: false }), "not-set-up");
  assert.equal(reconnectDelay(true, 9), 2_000);
  assert.equal(reconnectDelay(false, 0), 1_000);
  assert.equal(reconnectDelay(false, 20), 60_000);
});

test("only a download an agent tab began during a command, matched within 5 s, is the agent's", () => {
  let now = 1_000;
  const attribution = createAttribution(() => now);
  attribution.began({ url: "https://a.example/report.pdf", tabId: 7, command: "c1", server: "s", thread: THREAD, name: "report.pdf", resources: [] });
  assert.equal(attribution.claim({ id: 1, url: "https://a.example/other.pdf", startedAt: now }), undefined);
  // Matched through the redirect's final URL.
  assert.equal(attribution.claim({ id: 2, url: "https://cdn.example/x", finalUrl: "https://a.example/report.pdf", startedAt: now })?.command, "c1");
  assert.equal(attribution.claim({ id: 3, url: "https://a.example/report.pdf", startedAt: now }), undefined, "claimed once");
  attribution.began({ url: "https://a.example/late.zip", tabId: 7, command: "c2", server: "s", thread: THREAD, name: "late.zip", resources: [] });
  now += 5_001;
  assert.equal(attribution.claim({ id: 4, url: "https://a.example/late.zip", startedAt: now }), undefined);
});

test("a background tab is captured only in a window the user is not using", () => {
  assert.equal(capturePlan({ active: true, windowId: 1 }, 1), "ready");
  assert.equal(capturePlan({ active: false, windowId: 1 }, 2), "activate");
  assert.equal(capturePlan({ active: false, windowId: 1 }, undefined), "activate");
  assert.equal(capturePlan({ active: false, windowId: 1 }, 1), "refuse");
});

test("a page that fails to load is a navigation failure, with the network error named", async () => {
  const { failure, HostError } = await import("../src/core/errors");
  const outcome = failure({ type: "tabs.open" }, new Error("Chrome could not load https://nope.invalid: net::ERR_NAME_NOT_RESOLVED"));
  assert.equal(outcome.code, "navigation_failed");
  assert.match(outcome.message, /ERR_NAME_NOT_RESOLVED/);
  assert.equal(failure({ type: "click" }, new Error("ERR_FAILED somewhere")).code, "operation_failed");
  assert.deepEqual(failure({ type: "click" }, new HostError("paused")), { type: "failure", code: "paused", message: "The user paused Haarness in Chrome." });
});
