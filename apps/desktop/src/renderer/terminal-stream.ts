/**
 * How far each terminal's output has been shown in this window, as the
 * server counts bytes, by `<connection>:<pty>`. It outlives the view, so a
 * view mounted again (the panel reopened, the tab chosen again) knows which
 * replayed bytes were shown before.
 */
const shown = new Map<string, number>();
/** Terminals this window started, whose whole output is new to it. */
const started = new Set<string>();
/** Where views were scrolled up to when they went; see `scrollPlaces`. */
const places = new Map<string, ScrollPlace>();
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
    for (const key of [...shown.keys(), ...started, ...places.keys()])
      if (key.startsWith(prefix) && !ids.includes(key.slice(prefix.length))) {
        shown.delete(key);
        started.delete(key);
        places.delete(key);
      }
  },
};

/** Where a view was scrolled up to (`line`, from the top of xterm's
 * buffer), and what it showed then: the output from `start` to `end`, as the
 * server counts it, at `cols` × `rows`. */
export interface ScrollPlace {
  start: number;
  end: number;
  line: number;
  cols: number;
  rows: number;
}
/**
 * The place each terminal's view was scrolled up to when it went (the panel
 * hidden, another tab chosen), by `<connection>:<pty>`, so the view shown
 * again opens there instead of at the bottom (see `restoredLine`).
 */
export const scrollPlaces = {
  /** A view goes: scrolled up to a place, or at the bottom (`undefined`). */
  save(key: string, place: ScrollPlace | undefined) {
    if (place === undefined) places.delete(key);
    else places.set(key, place);
  },
  /** A view is shown again: where it was, once. */
  take(key: string) {
    const place = places.get(key);
    places.delete(key);
    return place;
  },
};
/**
 * The line a view shown again opens at: where it was, while that line holds
 * the same text. A replay that starts elsewhere (older output was dropped
 * meanwhile) or wraps at another width holds other lines. The same output at
 * the same size fills xterm's buffer alike; with more output, or another
 * height, the lines stay where they were only while xterm dropped nothing
 * from the top of its scrollback (`trimmed`).
 */
export function restoredLine(
  saved: ScrollPlace | undefined,
  now: Omit<ScrollPlace, "line"> & { trimmed: boolean },
) {
  if (!saved || saved.start !== now.start || saved.cols !== now.cols) return undefined;
  if (saved.end === now.end && saved.rows === now.rows) return saved.line;
  return now.trimmed ? undefined : saved.line;
}

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

/** What a terminal view says about its connection. */
export type Link = "connected" | "reconnecting" | "disconnected";
/** A drop mended within this long (the server lets go of a view that fell
 * behind a burst of output, and it connects again at once) shows nothing. */
export const RECONNECTING_AFTER = 1000;
/** After this long without its connection, a terminal says it cannot reach
 * its shell, and offers to retry or to open another terminal. */
export const DISCONNECTED_AFTER = 15_000;

/**
 * Follows a terminal's connection for what its view shows. While the server
 * cannot be reached the shell's state is unknown, so the view keeps the
 * terminal and keeps reconnecting in the background; it only says so.
 */
export function linkWatch(input: {
  onState: (link: Link) => void;
  schedule?: (run: () => void, ms: number) => ReturnType<typeof setTimeout>;
  cancel?: (timer: ReturnType<typeof setTimeout>) => void;
}) {
  const schedule = input.schedule ?? ((run, ms) => setTimeout(run, ms));
  const cancel = input.cancel ?? ((timer) => clearTimeout(timer));
  let link: Link = "connected";
  let down = false;
  let timers: ReturnType<typeof setTimeout>[] = [];
  const set = (next: Link) => {
    if (next === link) return;
    link = next;
    input.onState(next);
  };
  const clear = () => {
    for (const timer of timers) cancel(timer);
    timers = [];
  };
  return {
    /** The connection ended, or connecting failed. */
    lost() {
      if (down) return;
      down = true;
      timers = [
        schedule(() => set("reconnecting"), RECONNECTING_AFTER),
        schedule(() => set("disconnected"), DISCONNECTED_AFTER),
      ];
    },
    /** The person asked to try again: it says so at once, and gives up
     * again only after the whole wait. */
    retry() {
      clear();
      down = true;
      set("reconnecting");
      timers = [schedule(() => set("disconnected"), DISCONNECTED_AFTER)];
    },
    /** Connected again, or there is nothing left to connect to (the shell
     * exited, the terminal is gone). */
    connected() {
      clear();
      down = false;
      set("connected");
    },
    dispose: clear,
  };
}

/**
 * When a terminal view tries to connect again after its connection ended:
 * after a delay that grows from 250 ms to 5 s (`reconnectDelay`), and never
 * while another attempt is under way, since each would connect the view and
 * the output would arrive twice. Retry (`now`) tries at once, unless an
 * attempt is under way; then that attempt goes on, and if it fails the next
 * comes after the shortest delay.
 */
export function reconnects(input: {
  /** One attempt: true when the view is connected, or has nothing left to
   * connect to; false, or a failure, to try again later. */
  attempt: () => Promise<boolean>;
  schedule?: (run: () => void, ms: number) => ReturnType<typeof setTimeout>;
  cancel?: (timer: ReturnType<typeof setTimeout>) => void;
}) {
  const schedule = input.schedule ?? ((run, ms) => setTimeout(run, ms));
  const cancel = input.cancel ?? ((timer) => clearTimeout(timer));
  let attempts = 0;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let running = false;
  let stopped = false;
  const run = () => {
    timer = undefined;
    running = true;
    void input
      .attempt()
      .catch(() => false)
      .then((done) => {
        running = false;
        if (!done) later(reconnectDelay(attempts++));
      });
  };
  const later = (delay: number) => {
    if (stopped || running) return;
    if (timer !== undefined) cancel(timer);
    timer = undefined;
    // At once means now: a view shown connects, and takes the keyboard, in
    // the same moment.
    if (delay === 0) run();
    else timer = schedule(run, delay);
  };
  return {
    /** The connection ended: try again after the next delay. */
    after() {
      if (!running) later(reconnectDelay(attempts++));
    },
    /** The first connect, or Retry: at once, with the delays from the start. */
    now() {
      attempts = 0;
      later(0);
    },
    /** Output flows again: a later drop starts from the shortest delay. */
    reset() {
      attempts = 0;
    },
    dispose() {
      stopped = true;
      if (timer !== undefined) cancel(timer);
    },
  };
}
