/**
 * How far each terminal's output has been shown in this window, as the
 * server counts bytes, by `<connection>:<pty>`. It outlives the view, so a
 * view mounted again (the panel reopened, the tab chosen again) knows which
 * replayed bytes were shown before.
 */
const shown = new Map<string, number>();
/** Terminals this window started, whose whole output is new to it. */
const started = new Set<string>();
export const shownOutput = {
  get: (key: string) => shown.get(key) ?? 0,
  set: (key: string, end: number) => shown.set(key, Math.max(end, shown.get(key) ?? 0)),
  started: (key: string) => void started.add(key),
  /**
   * The first replay this window sees of a terminal it did not start (one
   * from before a reload or a relaunch, or from another window) is output
   * the shell wrote long ago: count all of it, up to `end`, as shown.
   */
  adopt: (key: string, end: number) => {
    if (!shown.has(key) && !started.has(key)) shown.set(key, end);
  },
  /** Forget terminals that are gone. */
  keep: (prefix: string, ids: readonly string[]) => {
    for (const key of [...shown.keys(), ...started])
      if (key.startsWith(prefix) && !ids.includes(key.slice(prefix.length))) {
        shown.delete(key);
        started.delete(key);
      }
  },
};

/**
 * Writes what the server sends into xterm and says whether xterm's answers
 * to terminal queries may reach the shell now. A view shown again replays
 * output the shell wrote earlier, and xterm answers the queries in it
 * (device attributes, cursor position, colours) as if they had just
 * arrived: sent on, those answers land at the prompt as typed text, such as
 * `^[[?1;2c`. So answers to bytes this terminal already showed are dropped.
 * Bytes past that point are new to it (a fresh terminal's start, output
 * missed while a connection was down) and a program may be waiting for the
 * answer (fish's device attributes, nvim's background colour), so those
 * answers go through.
 *
 * Gate changes are queued between writes (`write(empty, done)` calls back
 * once xterm has parsed everything before it), since xterm answers while it
 * parses, a moment after the write.
 */
export function terminalStream(input: {
  write: (bytes: Uint8Array, done?: () => void) => void;
  /** How far this terminal's output was already shown. */
  shown: () => number;
  show: (end: number) => void;
}) {
  let offset = 0;
  let open = true;
  let planned = true;
  const plan = (state: boolean) => {
    if (state === planned) return;
    planned = state;
    input.write(new Uint8Array(), () => {
      open = state;
    });
  };
  return {
    /** A replay starts at `from`, as the server counts bytes. */
    replay(from: number) {
      offset = from;
    },
    /** Output from the server, continuing at the current offset. */
    bytes(bytes: Uint8Array) {
      const seen = Math.max(0, Math.min(bytes.length, input.shown() - offset));
      if (seen) {
        plan(false);
        input.write(bytes.subarray(0, seen));
      }
      if (seen < bytes.length) {
        plan(true);
        input.write(bytes.subarray(seen));
      }
      offset += bytes.length;
      input.show(offset);
    },
    /** The server's count after what it sent. */
    cursor(end: number) {
      offset = end;
    },
    /** Bytes of the view's own, not the shell's (a reset, a notice, the
     * modes a truncated replay restores): xterm's answers to them stay out
     * of the shell. */
    quiet(bytes: Uint8Array) {
      plan(false);
      input.write(bytes);
    },
    /** Whether xterm's answers may go to the shell now. */
    open: () => open,
  };
}

/**
 * Input for a terminal whose connection is down (between a dropped socket
 * and the reconnect, or while the server is unreachable): kept in order and
 * sent once it is back, so Ctrl+C typed meanwhile still lands. At most
 * `limit` bytes are kept; past that the oldest input goes first.
 */
export function inputQueue(limit = 64 * 1024) {
  let items: { order: number; bytes: Uint8Array }[] = [];
  let size = 0;
  return {
    add(order: number, bytes: Uint8Array) {
      items.push({ order, bytes });
      items.sort((a, b) => a.order - b.order);
      size += bytes.length;
      while (size > limit && items.length) size -= items.shift()!.bytes.length;
    },
    /** Everything kept, in the order it was typed; the queue is empty after. */
    take() {
      const taken = items.map((item) => item.bytes);
      items = [];
      size = 0;
      return taken;
    },
  };
}

/** The most a terminal sends the PTY in one write; a long paste goes in
 * several, in order. */
export const WRITE_LIMIT = 32 * 1024;

/** `bytes` cut into pieces of at most `limit` bytes. A cut may fall inside
 * a character: the PTY takes a byte stream. */
export function writeChunks(bytes: Uint8Array, limit = WRITE_LIMIT) {
  const chunks: Uint8Array[] = [];
  for (let start = 0; start < bytes.length; start += limit)
    chunks.push(bytes.subarray(start, start + limit));
  return chunks;
}

/** How long a terminal waits before its `attempt`th reconnect (from 0):
 * 250 ms, doubling up to 5 s. */
export function reconnectDelay(attempt: number) {
  return Math.min(5000, 250 * 2 ** Math.min(attempt, 5));
}
