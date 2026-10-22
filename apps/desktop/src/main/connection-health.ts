/**
 * Noticing a server connection that died without closing, and trying again
 * at once when trying again can work.
 *
 * After the machine sleeps, or its network changes, a connection can stay
 * open on this side while nothing reaches it any more: an event stream then
 * waits forever, and a terminal takes keys that go nowhere. A healthy one is
 * never touched: event streams hear a keepalive every 15 s, and a terminal
 * answers a ping at once.
 */

/** Silence after which an event stream counts as lost: three keepalives. */
export const EVENT_IDLE_MS = 45_000;
/** How often an open terminal socket is asked whether it still answers. */
export const TERMINAL_PING_EVERY_MS = 30_000;
/** How long a terminal's answer may take before its socket counts as lost:
 * long enough for a slow remote behind a burst of output. */
export const TERMINAL_PING_TIMEOUT_MS = 15_000;

/** `fallback`, or under end-to-end tests the positive number `name` holds. */
function limit(env: NodeJS.ProcessEnv, name: string, fallback: number) {
  if (env.HAARNESS_DESKTOP_TEST === "1") {
    const value = Number(env[name]);
    if (Number.isFinite(value) && value > 0) return value;
  }
  return fallback;
}

/** The event stream silence limit; end-to-end tests may shorten it. */
export function eventIdleMs(env: NodeJS.ProcessEnv = process.env) {
  return limit(env, "HAARNESS_DESKTOP_EVENT_IDLE_MS", EVENT_IDLE_MS);
}

/** How long a terminal's answer may take. An end-to-end test may shorten it
 * for itself alone, through `haarnessTerminalPingTimeoutMs`. */
export function terminalPingTimeoutMs(env: NodeJS.ProcessEnv = process.env) {
  const set = (globalThis as { haarnessTerminalPingTimeoutMs?: unknown })
    .haarnessTerminalPingTimeoutMs;
  return env.HAARNESS_DESKTOP_TEST === "1" && typeof set === "number" && set > 0
    ? set
    : TERMINAL_PING_TIMEOUT_MS;
}

/**
 * The waits of connections that failed and try again after a backoff (up to
 * 30 s). A wake (the machine woke, the network came back) ends every wait
 * under way, so they try at once; a connection that is not waiting is
 * healthy, or trying already, and is left alone.
 */
export function retryWaits(
  schedule: (run: () => void, ms: number) => ReturnType<typeof setTimeout> = setTimeout,
  cancel: (timer: ReturnType<typeof setTimeout>) => void = clearTimeout,
) {
  const waits = new Map<symbol, { timer: ReturnType<typeof setTimeout>; run: () => void }>();
  return {
    /** Runs `run` after `ms`, or at the next wake; returns what cancels it. */
    wait(run: () => void, ms: number) {
      const id = Symbol();
      const timer = schedule(() => {
        waits.delete(id);
        run();
      }, ms);
      waits.set(id, { timer, run });
      return () => {
        cancel(timer);
        waits.delete(id);
      };
    },
    wake() {
      const due = [...waits.values()];
      waits.clear();
      for (const wait of due) {
        cancel(wait.timer);
        wait.run();
      }
    },
    get waiting() {
      return waits.size;
    },
  };
}

/** Calls `back` each time `online` turns true after it was false, looking
 * every `every` ms. Returns what stops looking. */
export function onlineAgain(
  online: () => boolean,
  back: () => void,
  every = 2_000,
) {
  let was = online();
  const timer = setInterval(() => {
    const now = online();
    if (now && !was) back();
    was = now;
  }, every);
  timer.unref?.();
  return () => clearInterval(timer);
}
