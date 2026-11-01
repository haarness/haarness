import test from "node:test";
import assert from "node:assert/strict";
import { relativeTime } from "../src/renderer/relative-time";

test("a relative time reads the time now, however long since the clock last ticked", async () => {
  // The shared tick is a real interval in Desktop; here nothing ticks, as in
  // a window whose timers the system slowed or before the first tick.
  const realInterval = globalThis.setInterval;
  const realNow = Date.now;
  globalThis.setInterval = (() => 0) as unknown as typeof setInterval;
  try {
    const { now } = await import("../src/renderer/clock");
    const opened = realNow();
    now();
    const later = opened + 3 * 60_000;
    Date.now = () => later;
    assert.equal(now(), later);
    // A message sent then reads "now", not "in 3 minutes".
    assert.equal(relativeTime(later, now(), "en"), "now");
  } finally {
    globalThis.setInterval = realInterval;
    Date.now = realNow;
  }
});

test("a time from a clock ahead of this one reads as now", () => {
  const now = Date.parse("2026-11-01T12:00:00Z");
  assert.equal(relativeTime(now + 3 * 60_000, now, "en"), "now");
  assert.equal(relativeTime(now - 3 * 60_000, now, "en"), "3 minutes ago");
});
