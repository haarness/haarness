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
