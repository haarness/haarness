import { createSignal } from "solid-js";

/** Ticks twice a minute from the first read, so every relative time on
 * screen ("now", "5 minutes ago", "3m") moves on together. */
const [tick, setTick] = createSignal(0);
let ticking = false;

/** What the clock listens to: the page's document, its window and timers. */
export interface ClockHost {
  document?: Pick<Document, "visibilityState" | "addEventListener">;
  window?: Pick<Window, "addEventListener">;
  setInterval: (run: () => void, ms: number) => unknown;
}

/**
 * Runs `advance` every 30 seconds, and at once when the window shows again or
 * takes the focus: Chromium slows a hidden window's timers to once a minute
 * and holds them while the computer sleeps, so the ages on screen would read
 * late until the next tick. Showing again and taking the focus usually come
 * together; one tick serves both.
 */
export function startTicks(
  advance: () => void,
  host: ClockHost = globalThis as unknown as ClockHost,
) {
  let last = Date.now();
  const tick = () => {
    last = Date.now();
    advance();
  };
  const returned = () => {
    if (Math.abs(Date.now() - last) >= 1000) tick();
  };
  host.setInterval(tick, 30_000);
  const page = host.document;
  page?.addEventListener("visibilitychange", () => {
    if (page.visibilityState === "visible") returned();
  });
  host.window?.addEventListener("focus", returned);
}

/**
 * The time now, for a relative time on screen. Reading it subscribes the
 * reader to the shared tick, but the value is always read fresh: a time
 * kept from the last tick (or from startup, before the first one) made a
 * message sent minutes after Desktop opened, or while its window was in the
 * background and its timers slowed, read "in 3 minutes".
 */
export function now() {
  if (!ticking) {
    ticking = true;
    startTicks(() => setTick((count) => count + 1));
  }
  tick();
  return Date.now();
}
