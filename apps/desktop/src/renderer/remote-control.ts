/**
 * `/remote-control`: continue a chat from a phone or any browser through
 * Haarness Cloud. The server owns the connector and its state (management
 * resource `remote-control`); these helpers read its answer.
 */
export type RemoteAction = "start" | "stop" | "view" | "status";
export type ShareState = "signed_out" | "off" | "connecting" | "on" | "stopped";
export interface RemoteShare {
  state: ShareState;
  url?: string;
  computer?: string;
  chat?: string;
  error?: string;
  /** Rows of `1` (dark) and `0` (light) modules, without the quiet zone. */
  qr: string[];
}

const states: ShareState[] = ["signed_out", "off", "connecting", "on", "stopped"];

/** `/remote-control`, and its short form `/rc`. */
export const remoteCommand = /^\/(remote-control|rc)(?:\s+([\s\S]*))?$/i;
export const remoteAliases = ["rc"];

/** What `/remote-control [status|stop]` asks for, or null when unknown. */
export function remoteAction(argumentsText: string): RemoteAction | null {
  switch (argumentsText.trim().toLowerCase()) {
    case "":
    case "on":
    case "start":
    case "share":
      return "start";
    case "stop":
    case "off":
      return "stop";
    case "status":
      return "view";
    default:
      return null;
  }
}

/** The `data` of a `remote-control` view, checked. */
export function parseShare(view: unknown): RemoteShare {
  const data = (view as { data?: Record<string, unknown> } | null)?.data ?? {};
  const text = (value: unknown) =>
    typeof value === "string" && value ? value : undefined;
  const state = states.includes(data.state as ShareState)
    ? (data.state as ShareState)
    : "off";
  return {
    state,
    url: text(data.url),
    computer: text(data.computer),
    chat: text(data.chat),
    error: text(data.error),
    qr: Array.isArray(data.qr)
      ? data.qr.filter((row): row is string => typeof row === "string" && /^[01]+$/.test(row))
      : [],
  };
}

/** Reachable from Cloud, or about to be. */
export function isLive(share?: RemoteShare | null) {
  return share?.state === "on" || share?.state === "connecting";
}

/**
 * One SVG path for every dark module, a horizontal run at a time, so the code
 * stays crisp at any size and the DOM stays small.
 */
export function qrPath(rows: string[]) {
  let path = "";
  rows.forEach((row, y) => {
    let x = 0;
    while (x < row.length) {
      if (row[x] !== "1") {
        x++;
        continue;
      }
      let end = x;
      while (row[end] === "1") end++;
      path += `M${x} ${y}h${end - x}v1h-${end - x}z`;
      x = end;
    }
  });
  return path;
}

/**
 * Orders what is known about each chat's share. Every status read and every
 * write (a start, stop or view in the dialog) takes the next number, and a
 * read's answer counts only while it is still the latest, so an answer that
 * was overtaken cannot put back an older state.
 */
export function shareOrder() {
  const latest = new Map<string, number>();
  return {
    begin(key: string) {
      const next = (latest.get(key) ?? 0) + 1;
      latest.set(key, next);
      return next;
    },
    latest: (key: string, read: number) => latest.get(key) === read,
  };
}
