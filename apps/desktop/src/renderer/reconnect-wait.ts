import type { DesktopEvent } from "../shared";

/** The first wait before a project connects again. */
export const RECONNECT_BASE_MS = 500;
/** The longest wait. */
export const RECONNECT_MAX_MS = 30_000;
/** An event stream that stayed open this long worked: the next wait is the
 * first one again. */
export const STABLE_STREAM_MS = 10_000;
/** After a wake, sidebar projects that were waiting to reconnect retry this
 * far apart, after the open project. */
export const WAKE_SPACING_MS = 250;

/**
 * The wait before a project connects again after a connection failed or its
 * event stream ended. It doubles with each attempt, up to 30 s, and starts
 * over only once a stream has stayed open for 10 s: opening a stream is no
 * proof that it works, and a server that refuses the stream, or drops it as
 * soon as it opens, while it answers everything else would otherwise have
 * every chat and the open history read again twice a second. Waking the
 * computer, the network coming back and the window showing again start the
 * wait over (`reset`), since what kept the connection down may be gone.
 */
export function createReconnectWait(now: () => number = Date.now) {
  let attempt = 0;
  let opened: number | undefined;
  return {
    /** The wait before the next attempt, which this call counts. */
    next() {
      const delay = Math.min(
        RECONNECT_MAX_MS,
        RECONNECT_BASE_MS * 2 ** Math.min(attempt, 6),
      );
      attempt++;
      return delay;
    },
    /** The event stream is open. */
    opened() {
      opened = now();
    },
    /** The event stream ended; saying so again changes nothing. */
    ended() {
      if (opened !== undefined && now() - opened >= STABLE_STREAM_MS)
        attempt = 0;
      opened = undefined;
    },
    /** The next wait is the first one again. */
    reset() {
      attempt = 0;
    },
  };
}

let wakeSlot = 0;
/** How long one of the sidebar's waiting projects waits after a wake: each a
 * little after the one before, all after the open project's first attempt,
 * so a wake does not send every project's requests at once. */
export function wakeDelay(at: number = Date.now()) {
  wakeSlot = Math.max(wakeSlot, at + RECONNECT_BASE_MS) + WAKE_SPACING_MS;
  return wakeSlot - at;
}

/** The computer woke from sleep (Desktop's main process says so to every
 * window). */
export function isWake(event: DesktopEvent) {
  return (
    event.kind === "connection" && !event.connection && event.data === "resume"
  );
}

/** Calls `run` when the network comes back or the window shows again. */
export function onReconnectHint(run: () => void) {
  const shown = () => {
    if (document.visibilityState === "visible") run();
  };
  window.addEventListener("online", run);
  document.addEventListener("visibilitychange", shown);
  return () => {
    window.removeEventListener("online", run);
    document.removeEventListener("visibilitychange", shown);
  };
}
