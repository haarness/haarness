import type { CompactionCut, DesktopEvent } from "../shared";

type Events = (listener: (event: DesktopEvent) => void) => () => void;

/** A compaction whose request a relay or proxy cut on its way (see main). */
export const isCompactionCut = (value: unknown): value is CompactionCut =>
  !!value && typeof value === "object" && typeof (value as CompactionCut).compactionCut === "string";

/**
 * Listen for `thread`'s compaction to land on `connection`, from before it is
 * asked for, so a summary that lands just as its request is cut is not missed.
 */
export function watchCompaction(
  connection: string,
  thread: string,
  events: Events = (listener) => window.haarness.onEvent(listener),
) {
  let landed = false;
  let arrived = () => {};
  const off = events((event) => {
    if (event.kind !== "server" || event.connection !== connection) return;
    const data = event.data as { type?: unknown; threadId?: unknown } | null;
    if (data?.type !== "thread.compacted" || data.threadId !== thread) return;
    landed = true;
    arrived();
  });
  let timer: ReturnType<typeof setTimeout> | undefined;
  const stop = () => {
    clearTimeout(timer);
    off();
  };
  return {
    /** Wait for it to land until `deadline` (epoch ms); past that, fail with `message`. */
    until(deadline: number, message: string): Promise<void> {
      return new Promise<void>((resolve, reject) => {
        if (landed) return resolve();
        arrived = resolve;
        timer = setTimeout(() => reject(new Error(message)), Math.max(0, deadline - Date.now()));
      }).finally(stop);
    },
    stop,
  };
}
