import type { EventEnvelope, ServerInfo } from "@haarness/sdk";
import type { Bridge, Connection, DesktopEvent } from "../shared";
import {
  SIGNAL_MERGE_MS,
  createReconnectWait,
  type createWakeSlots,
  isWake,
  wakeSlots,
} from "./reconnect-wait";

/**
 * The connection of one expanded sidebar project that is not the open one:
 * a snapshot of its chats, turns and requests, then its event stream from the
 * snapshot's cursor, connected again after a drop. The wait before connecting
 * again follows `createReconnectWait`, timed from when main says the stream
 * opened (its response arrived), not from when it was asked for.
 *
 * A wake, the network coming back or the window showing again (`hint`) brings
 * a waiting project's attempt forward to its slot behind the open project
 * (`wakeSlots`), or supersedes an attempt after a drop still in flight (never
 * the project's first load); signals that arrive
 * together count once, and a retry already due sooner is kept. A connected
 * project is left alone, except that a wake subscribes it again from its last
 * event, quietly: its stream may have died while the computer slept, and the
 * snapshot it shows is still current.
 */
export function createOverviewLink(input: {
  id: string;
  bridge: Pick<Bridge, "api" | "subscribe" | "unsubscribe">;
  connect: (id: string) => Promise<Connection | null>;
  /** Reads the project's snapshot; the answer shows it, called only while
   * this read is still the latest. */
  snapshot: (info: ServerInfo) => Promise<() => void>;
  /** One event of the stream, in order and after the snapshot it follows. */
  apply: (event: EventEnvelope) => void;
  loading: () => void;
  loaded: () => void;
  failed: (message: string) => void;
  now?: () => number;
  schedule?: (run: () => void, ms: number) => () => void;
  slots?: ReturnType<typeof createWakeSlots>;
}) {
  const now = input.now ?? Date.now;
  const schedule =
    input.schedule ??
    ((run: () => void, ms: number) => {
      const timer = setTimeout(run, ms);
      return () => clearTimeout(timer);
    });
  const slots = input.slots ?? wakeSlots;
  const wait = createReconnectWait(now);
  let disposed = false,
    subscription = "",
    instance = "",
    cursor = 0,
    operation = 0,
    hinted = Number.NEGATIVE_INFINITY;
  /** A retry waits for its timer, due at `due`. */
  let pending: { cancel: () => void; due: number } | undefined;
  /** Events that arrived while a subscription was being asked for. */
  let early: DesktopEvent[] | undefined;
  /** A quiet subscription is being asked for. */
  let renewing = false;
  /** The project lost its connection (or failed to load) and is loading
   * again; not its first load. */
  let recovering = false;
  const api = <T>(method: string) => input.bridge.api<T>(input.id, method, []);
  const stop = async () => {
    const previous = subscription;
    subscription = "";
    if (previous) await input.bridge.unsubscribe(previous);
  };
  const cancel = () => {
    pending?.cancel();
    pending = undefined;
  };
  /** Loads again after `wait`'s wait, or after `delay`. One wait at a time:
   * the end of a stream and the server's resync ask for the same retry. */
  const retry = (delay?: number) => {
    if (disposed || (pending && delay === undefined)) return;
    recovering = true;
    cancel();
    const ms = delay ?? wait.next();
    const entry = {
      due: now() + ms,
      cancel: schedule(() => {
        if (pending === entry) pending = undefined;
        void load();
      }, ms),
    };
    pending = entry;
  };
  /** The stream ended. */
  const dropped = () => {
    wait.ended();
    retry();
  };
  /** Takes the events that arrived while `key` was asked for. */
  const settle = (key: string, buffered: DesktopEvent[]) => {
    subscription = key;
    for (const event of buffered) receive(event);
  };
  async function load() {
    input.loading();
    const current = ++operation;
    const valid = () => !disposed && operation === current;
    try {
      await stop();
      if (!valid()) return;
      const connection = await input.connect(input.id);
      if (!connection || !valid()) return;
      const info = connection.info;
      const show = await input.snapshot(info);
      if (!valid()) return;
      show();
      instance = info.serverInstanceId;
      cursor = info.eventCursor ?? 0;
      // Replay starts before the REST snapshot, closing the snapshot/SSE gap.
      const buffered: DesktopEvent[] = [];
      early = buffered;
      const key = await input.bridge.subscribe(input.id, cursor);
      if (early === buffered) early = undefined;
      if (!valid()) {
        await input.bridge.unsubscribe(key);
        return;
      }
      // Connected: a retry a signal asked for meanwhile is not needed.
      cancel();
      recovering = false;
      input.loaded();
      settle(key, buffered);
    } catch (error) {
      early = undefined;
      if (!valid()) return;
      input.failed(error instanceof Error ? error.message : String(error));
      retry();
    }
  }
  /** Subscribes a connected project again from its last event, keeping what
   * it shows: no snapshot, no loading state. Another server instance, or a
   * server out of reach, loads again as a drop would. */
  async function renew() {
    if (renewing) return;
    renewing = true;
    const current = ++operation;
    // A drop meanwhile has a full load of its own waiting.
    const valid = () => !disposed && operation === current && !pending;
    try {
      const info = await api<ServerInfo>("info");
      if (!valid()) return;
      if (info.serverInstanceId !== instance) {
        dropped();
        return;
      }
      const previous = subscription;
      const buffered: DesktopEvent[] = [];
      early = buffered;
      const key = await input.bridge.subscribe(input.id, cursor);
      if (early === buffered) early = undefined;
      if (!valid()) {
        await input.bridge.unsubscribe(key);
        return;
      }
      settle(key, buffered);
      if (previous) await input.bridge.unsubscribe(previous);
    } catch {
      early = undefined;
      if (valid()) dropped();
    } finally {
      renewing = false;
    }
  }
  function receive(event: DesktopEvent) {
    if (isWake(event)) {
      hint(true);
      return;
    }
    if (
      disposed ||
      event.connection !== input.id ||
      !["server", "connection"].includes(event.kind)
    )
      return;
    if (early) {
      early.push(event);
      return;
    }
    if (!subscription || event.id !== subscription) return;
    if (event.kind === "connection") {
      if (event.data === "opened") wait.opened();
      else dropped();
      return;
    }
    const value = event.data as EventEnvelope | { type: "resync_required" };
    if (
      value.type === "resync_required" ||
      !("serverInstanceId" in value) ||
      value.serverInstanceId !== instance
    ) {
      dropped();
      return;
    }
    if (value.eventId <= cursor) return;
    cursor = value.eventId;
    input.apply(value);
  }
  /** A wake (`wake`), the network coming back or the window showing again. */
  function hint(wake: boolean) {
    if (disposed) return;
    const connected = !!subscription && !pending;
    if (connected) {
      if (wake) void renew();
      return;
    }
    // A first load in flight is left to finish, as the open project's is.
    if (!recovering) return;
    const at = now();
    if (at - hinted < SIGNAL_MERGE_MS) return;
    hinted = at;
    if (pending && pending.due - at <= slots.peek(at)) return;
    wait.reset();
    // An attempt still in flight is superseded by the one this starts.
    retry(slots.take(at));
  }
  return {
    receive,
    hint,
    start() {
      void load();
    },
    dispose() {
      disposed = true;
      operation++;
      cancel();
      void stop().catch(() => {});
    },
  };
}
