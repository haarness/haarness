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

test("relative times move on at once when the window shows again or takes the focus, once for both", async () => {
  const { startTicks } = await import("../src/renderer/clock");
  const realNow = Date.now;
  let time = Date.parse("2026-11-10T12:00:00Z");
  Date.now = () => time;
  try {
    const page = Object.assign(new EventTarget(), { visibilityState: "hidden" as DocumentVisibilityState });
    const view = new EventTarget();
    const timers: (() => void)[] = [];
    let ticks = 0;
    const woke = startTicks(() => ticks++, {
      document: page,
      window: view,
      setInterval: (run) => timers.push(run),
    });
    // A window hidden while its timers wait is not a reason to tick.
    time += 5 * 60_000;
    page.dispatchEvent(new Event("visibilitychange"));
    assert.equal(ticks, 0);
    // Shown again: the ages read now, not when the slowed timer comes round.
    page.visibilityState = "visible";
    page.dispatchEvent(new Event("visibilitychange"));
    assert.equal(ticks, 1);
    // The focus that comes with it changes nothing more.
    view.dispatchEvent(new Event("focus"));
    assert.equal(ticks, 1);
    // A later focus, after the computer slept with the window in front, ticks.
    time += 3 * 60_000;
    view.dispatchEvent(new Event("focus"));
    assert.equal(ticks, 2);
    // And the regular tick still runs.
    assert.equal(timers.length, 1);
    timers[0]();
    assert.equal(ticks, 3);
    // A wake with the window in front all along fires neither event, and
    // ticks through what `startTicks` returns; not twice in one second.
    time += 10 * 60_000;
    woke();
    woke();
    assert.equal(ticks, 4);
  } finally {
    Date.now = realNow;
  }
});
