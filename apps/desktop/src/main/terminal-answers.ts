/**
 * Which window answers the queries a terminal's programs send. Every view
 * of a terminal parses its output, and xterm answers what asks for an answer
 * (device attributes, cursor position, colours). With the terminal shown in
 * two windows each would answer, and the program reads one answer while the
 * other lands at the prompt as typed text, such as `^[[?1;2c`. So only one
 * window's answers go on: of the windows connected to the terminal, the one
 * the person used last (typed in, or connected to it most recently). What a
 * person types always goes on, from any window.
 */
export function terminalAnswers() {
  let clock = 0;
  /** By terminal (`<connection>:<pty>`): each window's last use of it. */
  const used = new Map<string, Map<number, number>>();
  return {
    /** `win` connected to `terminal`, or the person typed in it there. */
    use(terminal: string, win: number) {
      const windows = used.get(terminal) ?? new Map<number, number>();
      windows.set(win, ++clock);
      used.set(terminal, windows);
    },
    /** Whether `win`'s answers to `terminal`'s programs go on, given which
     * windows are connected to it now. */
    answers(terminal: string, win: number, connected: (win: number) => boolean) {
      let answerer: number | undefined;
      let latest = -1;
      for (const [candidate, at] of used.get(terminal) ?? [])
        if (at > latest && connected(candidate)) {
          answerer = candidate;
          latest = at;
        }
      return answerer === undefined || answerer === win;
    },
    /** A window closed. */
    forget(win: number) {
      for (const [terminal, windows] of used) {
        windows.delete(win);
        if (!windows.size) used.delete(terminal);
      }
    },
  };
}
