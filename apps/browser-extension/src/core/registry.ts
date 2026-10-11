/**
 * Which Chrome tabs belong to which chat.
 *
 * Chats are keyed by (workspace, thread), never by server id: a server
 * restarts with a new id, and its chats keep their tabs. The registry lives in
 * `chrome.storage.session`, so it survives the service worker stopping but
 * not Chrome quitting (a restarted browser never re-adopts old groups).
 */
import { MAX_THREADS, MAX_TITLE, MAX_URL, type Policy, type State, type Tab, type TabID } from "./browser";

export interface TabRecord {
  id: TabID;
  /** Chrome's tab id. */
  tabId: number;
  generation: number;
  url: string;
  title: string;
  loading: boolean;
  canGoBack: boolean;
  canGoForward: boolean;
}

export interface ThreadRecord {
  workspace: string;
  thread: string;
  title: string;
  parent: string | null;
  groupId: number | null;
  tabs: TabRecord[];
  focusedTabID: TabID | null;
  /** Tabs the user took back, so the server can say so. Newest last, at most 32. */
  revoked: TabID[];
  /** Origins this chat's executed commands were approved for. */
  approved: string[];
  /** The latest site rules the server sent for this chat (for popups between commands). */
  policy: Policy | null;
  /** Why the latest tab the user dragged in was not shared, if it was not. */
  refused: "restricted" | "full" | null;
  touchedAt: number;
}

export interface Snapshot {
  threads: ThreadRecord[];
  /** Server instance id last attached for each workspace. */
  instances: Record<string, string>;
}

export interface Storage {
  load(): Promise<Snapshot | undefined>;
  save(snapshot: Snapshot): Promise<void>;
}

export const threadKey = (workspace: string, thread: string) => JSON.stringify([workspace, thread]);

export function createRegistry(storage: Storage, now: () => number = Date.now) {
  const threads = new Map<string, ThreadRecord>();
  const byTab = new Map<number, string>();
  let instances: Record<string, string> = {};
  let timer: ReturnType<typeof setTimeout> | undefined;

  const index = () => {
    byTab.clear();
    for (const [key, entry] of threads) for (const tab of entry.tabs) byTab.set(tab.tabId, key);
  };
  const snapshot = (): Snapshot => ({ threads: [...threads.values()], instances });

  const registry = {
    async load() {
      const saved = await storage.load().catch(() => undefined);
      threads.clear();
      for (const entry of saved?.threads ?? []) {
        if (!entry || typeof entry.thread !== "string" || typeof entry.workspace !== "string") continue;
        threads.set(threadKey(entry.workspace, entry.thread), {
          ...entry,
          revoked: entry.revoked ?? [],
          approved: entry.approved ?? [],
          policy: entry.policy ?? null,
          refused: entry.refused ?? null,
        });
      }
      instances = saved?.instances ?? {};
      index();
    },
    /** Writes soon; several changes in one turn of the event loop are one write. */
    save() {
      clearTimeout(timer);
      timer = setTimeout(() => void storage.save(snapshot()).catch(() => {}), 50);
    },
    async flush() {
      clearTimeout(timer);
      await storage.save(snapshot()).catch(() => {});
    },
    get(key: string) {
      return threads.get(key);
    },
    all() {
      return [...threads.values()];
    },
    ensure(workspace: string, thread: string): ThreadRecord {
      const key = threadKey(workspace, thread);
      let entry = threads.get(key);
      if (!entry) {
        entry = {
          workspace,
          thread,
          title: "",
          parent: null,
          groupId: null,
          tabs: [],
          focusedTabID: null,
          revoked: [],
          approved: [],
          policy: null,
          refused: null,
          touchedAt: now(),
        };
        threads.set(key, entry);
        registry.prune();
      }
      entry.touchedAt = now();
      return entry;
    },
    /** Forgets the oldest chats that hold no tabs once there are too many. */
    prune() {
      if (threads.size <= MAX_THREADS) return;
      const idle = [...threads.entries()]
        .filter(([, entry]) => entry.tabs.length === 0)
        .sort((a, b) => a[1].touchedAt - b[1].touchedAt);
      for (const [key] of idle.slice(0, threads.size - MAX_THREADS)) threads.delete(key);
    },
    owner(tabId: number): { entry: ThreadRecord; tab: TabRecord } | undefined {
      const key = byTab.get(tabId);
      const entry = key === undefined ? undefined : threads.get(key);
      const tab = entry?.tabs.find((tab) => tab.tabId === tabId);
      return entry && tab ? { entry, tab } : undefined;
    },
    byGroup(groupId: number) {
      if (groupId < 0) return;
      return [...threads.values()].find((entry) => entry.groupId === groupId);
    },
    addTab(entry: ThreadRecord, tab: TabRecord) {
      entry.tabs.push(tab);
      byTab.set(tab.tabId, threadKey(entry.workspace, entry.thread));
      entry.focusedTabID ??= tab.id;
      registry.save();
    },
    /** Drops a tab; `revoke` records that the user took it back. */
    removeTab(entry: ThreadRecord, id: TabID, revoke: boolean) {
      const tab = entry.tabs.find((tab) => tab.id === id);
      entry.tabs = entry.tabs.filter((tab) => tab.id !== id);
      if (tab) byTab.delete(tab.tabId);
      if (entry.focusedTabID === id) entry.focusedTabID = entry.tabs[0]?.id ?? null;
      if (revoke) entry.revoked = [...entry.revoked.filter((value) => value !== id), id].slice(-32);
      registry.save();
      return tab;
    },
    /** Chrome swapped a tab for another (prerender activation): same tab, new id. */
    replaceTab(removed: number, added: number) {
      const owned = registry.owner(removed);
      if (!owned) return;
      owned.tab.tabId = added;
      byTab.delete(removed);
      byTab.set(added, threadKey(owned.entry.workspace, owned.entry.thread));
      registry.save();
    },
    /**
     * Records the server instance attached for a workspace. A different instance
     * is a restarted server whose approvals are gone, so this chat's approved
     * origins go too.
     */
    attach(workspace: string, instance: string) {
      if (instances[workspace] === instance) return false;
      const restarted = workspace in instances;
      instances = { ...instances, [workspace]: instance };
      if (restarted) for (const entry of threads.values()) if (entry.workspace === workspace) entry.approved = [];
      registry.save();
      return restarted;
    },
    approve(entry: ThreadRecord, origins: Iterable<string>) {
      const next = new Set(entry.approved);
      for (const origin of origins) next.add(origin);
      if (next.size === entry.approved.length) return;
      entry.approved = [...next].slice(-512);
      registry.save();
    },
  };
  return registry;
}

export type Registry = ReturnType<typeof createRegistry>;

/** A tab as the server accepts it: bounded URL and title, a whole generation. */
export function wireTab(tab: TabRecord): Tab {
  return {
    id: tab.id,
    url: tab.url.length > MAX_URL ? tab.url.slice(0, MAX_URL) : tab.url,
    title: tab.title.slice(0, MAX_TITLE),
    loading: tab.loading,
    canGoBack: tab.canGoBack,
    canGoForward: tab.canGoForward,
    generation: Math.max(0, Math.floor(tab.generation)),
  };
}

export function wireState(entry: ThreadRecord, override?: (tab: TabRecord) => Tab | undefined): State {
  return {
    tabs: entry.tabs.map((tab) => override?.(tab) ?? wireTab(tab)),
    focusedTabID: entry.tabs.some((tab) => tab.id === entry.focusedTabID) ? entry.focusedTabID : (entry.tabs[0]?.id ?? null),
  };
}
