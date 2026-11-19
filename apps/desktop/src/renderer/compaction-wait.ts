import type { CompactionCut, DesktopEvent } from "../shared";

type Events = (listener: (event: DesktopEvent) => void) => () => void;

/** A compaction whose request a relay or proxy cut on its way (see main). */
export const isCompactionCut = (value: unknown): value is CompactionCut =>
  !!value && typeof value === "object" && typeof (value as CompactionCut).compactionCut === "string";

/** Said when the chat changed while a compaction waited for its summary. */
export const CHAT_CHANGED = "The chat changed while it was being summarised";

interface Envelope {
  type?: unknown;
  threadId?: unknown;
  serverInstanceId?: unknown;
  payload?: { data?: { type?: unknown } } | null;
}

/**
 * Listen for `thread`'s compaction to land on `connection`, from before it is
 * asked for, so a summary that lands just as its request is cut is not
 * missed. Anything else written in the chat meanwhile (an item, a turn) means
 * that summary can no longer land; a server of another instance than
 * `instance` means it died with the one before.
 */
export function watchCompaction(
  connection: string,
  thread: string,
  instance: string | undefined,
  events: Events = (listener) => window.haarness.onEvent(listener),
) {
  let outcome: "landed" | "changed" | "restarted" | undefined;
  let settle = () => {};
  const off = events((event) => {
    if (outcome || event.kind !== "server" || event.connection !== connection) return;
    const data = event.data as Envelope | null;
    if (!data) return;
    if (instance && typeof data.serverInstanceId === "string" && data.serverInstanceId !== instance)
      outcome = "restarted";
    else if (data.threadId !== thread) return;
    else if (data.type === "thread.compacted") outcome = "landed";
    else if (data.type === "turn.started") outcome = "changed";
    else if (data.type === "item.created" && data.payload?.data?.type !== "compaction") outcome = "changed";
    else return;
    settle();
  });
  let timer: ReturnType<typeof setTimeout> | undefined;
  const stop = () => {
    clearTimeout(timer);
    off();
  };
  return {
    /**
     * Wait for it to land until `deadline` (epoch ms). Past that, or once the
     * server restarted, fail with `message`; once the chat changed, with
     * `changed`.
     */
    until(deadline: number, message: string, changed: string): Promise<void> {
      return new Promise<void>((resolve, reject) => {
        settle = () => {
          if (outcome === "landed") resolve();
          else reject(new Error(outcome === "changed" ? changed : message));
        };
        if (outcome) return settle();
        timer = setTimeout(() => reject(new Error(message)), Math.max(0, deadline - Date.now()));
      }).finally(stop);
    },
    stop,
  };
}
