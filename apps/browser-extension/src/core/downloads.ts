/**
 * Which Chrome downloads belong to an agent.
 *
 * Only a download that an agent tab began while one of its commands ran, or
 * within 10 s after it (a server answers a click with an attachment after the
 * click returned), is the agent's (CDP `Page.downloadWillBegin`), and only once
 * Chrome's own download item for the same URL appears within 5 s. Anything
 * else the user downloads is never listed, read or announced.
 */

export interface Started {
  url: string;
  at: number;
  tabId: number;
  /** The agent command during (or just after) which the page started it. */
  command: string;
  server: string;
  thread: string;
  name: string;
  /** The origins the bytes come from: the tab's top document and the download URL. */
  resources: string[];
}

export interface Item {
  id: number;
  url: string;
  finalUrl?: string;
  startedAt: number;
}

export const MATCH_WINDOW_MS = 5_000;

export function createAttribution(now: () => number = Date.now) {
  let started: Started[] = [];
  const fresh = () => {
    const cutoff = now() - MATCH_WINDOW_MS;
    started = started.filter((entry) => entry.at >= cutoff);
  };
  return {
    began(entry: Omit<Started, "at">) {
      fresh();
      started.push({ ...entry, at: now() });
    },
    /** Claims the agent download a new Chrome item corresponds to, if any. */
    claim(item: Item): Started | undefined {
      fresh();
      const index = started.findIndex(
        (entry) => entry.url === item.url || (item.finalUrl !== undefined && entry.url === item.finalUrl),
      );
      if (index < 0) return;
      const [match] = started.splice(index, 1);
      return match;
    },
    pending() {
      fresh();
      return started.length;
    },
  };
}
