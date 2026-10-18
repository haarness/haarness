// Which browser tabs an agent command is working in, to tell its popups from the person's.

/** How long after an agent's command a popup its page opens is still the agent's (a click's window.open). */
export const POPUP_GRACE_MS = 1_000;

/**
 * The tabs an agent command is running in now, or ended in a moment ago.
 * A command that only waits its turn in the queue counts for nothing: a
 * popup the person opens meanwhile is theirs.
 */
export function createAgentActivity(now: () => number = Date.now, graceMs = POPUP_GRACE_MS) {
  const running = new Map<string, number>();
  const ended = new Map<string, number>();
  return {
    begin(tab: string) {
      running.set(tab, (running.get(tab) ?? 0) + 1);
    },
    end(tab: string) {
      const left = (running.get(tab) ?? 1) - 1;
      if (left > 0) running.set(tab, left);
      else running.delete(tab);
      ended.set(tab, now());
    },
    /** An agent command runs in the tab, or ended there within the grace period. */
    working(tab: string) {
      if (running.has(tab)) return true;
      const at = ended.get(tab);
      if (at === undefined) return false;
      if (now() - at < graceMs) return true;
      ended.delete(tab);
      return false;
    },
  };
}
