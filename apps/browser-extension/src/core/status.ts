/**
 * The connection to Haarness as the user sees it, and how often to retry it.
 */

export type Connection =
  /** Waiting for the bridge's first message. */
  | "connecting"
  /** The bridge is running and speaks this protocol. */
  | "connected"
  /** Chrome cannot start the bridge: setup has not run in this profile, or it was removed. */
  | "not-set-up"
  /** The bridge started and then exited; retrying. */
  | "bridge-exited"
  /** The bridge speaks another protocol version. */
  | "mismatch";

export type Pill = "connected" | "connecting" | "not-set-up" | "paused" | "stopped" | "update" | "mismatch";

/** Chrome's `lastError` text for a closed native port → what it means. */
export function disconnectReason(message: string | undefined): Connection {
  if (!message) return "bridge-exited";
  if (/native messaging host not found/i.test(message)) return "not-set-up";
  // A manifest that does not list this extension, or a host blocked by policy.
  if (/forbidden/i.test(message)) return "not-set-up";
  return "bridge-exited";
}

export function pill(state: { connection: Connection; paused: boolean; stopped: boolean; update: boolean }): Pill {
  if (state.connection === "not-set-up") return "not-set-up";
  if (state.connection === "mismatch") return "mismatch";
  if (state.stopped) return "stopped";
  if (state.paused) return "paused";
  if (state.connection !== "connected") return "connecting";
  return state.update ? "update" : "connected";
}

/**
 * How long to wait before reconnecting. While the popup or welcome page is
 * open the user is watching, so retry every 2 s; otherwise back off to a
 * minute (a one-minute alarm also wakes a stopped worker to try again).
 */
export function reconnectDelay(watching: boolean, attempt: number): number {
  if (watching) return 2_000;
  return Math.min(60_000, 1_000 * 2 ** Math.min(attempt, 6));
}

/** Failed attempts after which the popup suggests that Chrome's policy may block Haarness. */
export const MANAGED_HINT_AFTER = 3;
