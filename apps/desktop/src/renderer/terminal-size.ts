/** A terminal's size in cells, as xterm reports it and the PTY takes it. */
export type TerminalSize = { cols: number; rows: number };

/**
 * The size still to push after xterm takes `next`, given the size the PTY
 * was last given. A size the PTY already has leaves nothing to push, so a
 * resize undone before the push timer fires sends nothing, and a stale size
 * cannot be sent after it.
 */
export function pendingSize(pushed: TerminalSize | undefined, next: TerminalSize) {
  return pushed?.cols === next.cols && pushed?.rows === next.rows ? undefined : next;
}

/**
 * Pushes the sizes xterm takes to the PTY once they settle (`delay` after the
 * last one), skipping a size the PTY already has. A push that fails is
 * forgotten, so asking for that size again (as a reconnect does) sends it
 * instead of skipping it as already pushed. `now` pushes at once, for the
 * size a view connects with.
 */
export function sizePusher(
  send: (size: TerminalSize) => Promise<unknown>,
  delay = 100,
) {
  let pushed: TerminalSize | undefined;
  let pending: TerminalSize | undefined;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let disposed = false;
  const push = (size: TerminalSize) => {
    pushed = size;
    return send(size).then(
      () => {},
      () => {
        if (pushed === size) pushed = undefined;
      },
    );
  };
  return {
    schedule(size: TerminalSize) {
      pending = pendingSize(pushed, size);
      clearTimeout(timer);
      if (!pending || disposed) return;
      timer = setTimeout(() => {
        const size = pending;
        pending = undefined;
        if (!size || disposed) return;
        void push(size);
      }, delay);
    },
    /** Push `size` without waiting for it to settle, in place of any push
     * still waiting; settles once the PTY took it (or the push failed). */
    now(size: TerminalSize): Promise<void> {
      clearTimeout(timer);
      pending = undefined;
      const next = pendingSize(pushed, size);
      return next && !disposed ? push(next) : Promise.resolve();
    },
    dispose() {
      disposed = true;
      clearTimeout(timer);
    },
  };
}
