import { createSignal } from "solid-js";

/** Ticks twice a minute from the first read, so every relative time on
 * screen ("now", "5 minutes ago", "3m") moves on together. */
const [tick, setTick] = createSignal(0);
let ticking = false;

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
    setInterval(() => setTick((count) => count + 1), 30_000);
  }
  tick();
  return Date.now();
}
