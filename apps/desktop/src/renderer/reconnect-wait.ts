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

/** Whether a reconnect found another run of the server than the one this
 * window read when the project opened: it restarted, and read its
 * configuration (the configured reasoning level among it) afresh, so what the
 * window knows of it is read again. */
export function serverRestarted(
  known: { serverInstanceId: string } | null | undefined,
  fresh: { serverInstanceId: string },
) {
  return !!known && known.serverInstanceId !== fresh.serverInstanceId;
}

/** Signals that arrive together (a wake brings the network and the window
 * back too) count as one when they are this close. */
export const SIGNAL_MERGE_MS = 1000;

/**
 * Retry times for the sidebar's waiting projects after a wake: each a little
 * after the one before, all after the open project's first attempt, so a wake
 * does not send every project's requests at once. `peek` says what the next
 * slot would be without taking it, so a project whose retry is already due
 * sooner keeps it and takes no slot.
 */
export function createWakeSlots() {
  let last = 0;
  const next = (at: number) =>
    Math.max(last, at + RECONNECT_BASE_MS) + WAKE_SPACING_MS;
  return {
    /** The delay from `at` the next slot would have. */
    peek(at: number) {
      return next(at) - at;
    },
    /** Takes the next slot; answers its delay from `at`. */
    take(at: number) {
      last = next(at);
      return last - at;
    },
  };
}
/** The slots the sidebar's projects share. */
export const wakeSlots = createWakeSlots();

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
