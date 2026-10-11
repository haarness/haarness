/**
 * A small in-memory Chrome for unit tests: tabs, windows, tab groups, storage,
 * a native port, and a debugger whose pages load, redirect and honour CDP
 * `Fetch` interception. Install it as `globalThis.chrome` before importing the
 * Chrome platform.
 */

type Listener = (...args: any[]) => void;

export class FakeEvent<T extends Listener = Listener> {
  private listeners = new Set<T>();
  addListener(listener: T) {
    this.listeners.add(listener);
  }
  removeListener(listener: T) {
    this.listeners.delete(listener);
  }
  hasListener(listener: T) {
    return this.listeners.has(listener);
  }
  emit(...args: Parameters<T>) {
    for (const listener of [...this.listeners]) listener(...args);
  }
}

export interface FakeTab {
  id: number;
  windowId: number;
  index: number;
  url: string;
  title: string;
  status: "loading" | "complete";
  active: boolean;
  groupId: number;
  openerTabId?: number;
  incognito: boolean;
  pinned: boolean;
  highlighted: boolean;
  discarded: boolean;
  autoDiscardable: boolean;
  selected: boolean;
  frozen: boolean;
  /** The top document's loader; a new document gets a new one. */
  loaderId: string;
}

interface Paused {
  resolve(action: "continue" | "fail"): void;
}

export class FakePort {
  readonly onMessage = new FakeEvent<(message: unknown) => void>();
  readonly onDisconnect = new FakeEvent<() => void>();
  readonly sent: any[] = [];
  connected = true;
  constructor(private readonly chrome: FakeChrome) {}
  postMessage(message: unknown) {
    if (!this.connected) throw new Error("Attempting to use a disconnected port object");
    this.sent.push(JSON.parse(JSON.stringify(message)));
  }
  disconnect() {
    this.connected = false;
  }
  /** The bridge sends a message to the extension. */
  deliver(message: unknown) {
    this.onMessage.emit(JSON.parse(JSON.stringify(message)));
  }
  /** Chrome closes the port, as when the host exits. */
  close(error = "Native host has exited.") {
    this.connected = false;
    this.chrome.lastErrorMessage = error;
    this.onDisconnect.emit();
    this.chrome.lastErrorMessage = undefined;
  }
}

export class FakeChrome {
  tabsById = new Map<number, FakeTab>();
  groupsById = new Map<number, { id: number; windowId: number; title?: string; color: string; collapsed: boolean }>();
  windowIds: number[] = [1];
  focused = 1;
  nextTab = 10;
  nextGroup = 100;
  nextWindow = 2;
  stores = { session: new Map<string, unknown>(), local: new Map<string, unknown>() };
  ports: FakePort[] = [];
  hostInstalled = true;
  lastErrorMessage: string | undefined;
  attached = new Set<number>();
  fetching = new Map<number, { patterns: unknown[] }>();
  paused = new Map<string, Paused>();
  /** URL → where the site redirects it. */
  redirects = new Map<string, string>();
  /** Page URL → the frames and resources it loads once committed. */
  subresources = new Map<string, { url: string; resourceType: string; frameId?: string }[]>();
  /** Subresources the extension failed through Fetch, in order. */
  refused: string[] = [];
  titles = new Map<string, string>();
  badge = "";
  reloads = 0;
  /** Debugger detaches the extension asked for, by tab. */
  detaches: number[] = [];
  private request = 0;
  private loaders = 0;

  readonly chrome = this.build();

  get port() {
    return this.ports.at(-1)!;
  }

  private build() {
    const self = this;
    const ev = <T extends Listener>() => new FakeEvent<T>();
    const copy = (tab: FakeTab) => ({ ...tab }) as unknown as chrome.tabs.Tab;
    const tabs = {
      onCreated: ev(),
      onUpdated: ev(),
      onRemoved: ev(),
      onReplaced: ev(),
      onZoomChange: ev(),
      async get(id: number) {
        const tab = self.tabsById.get(id);
        if (!tab) throw new Error(`No tab with id: ${id}.`);
        return copy(tab);
      },
      async query(query: { groupId?: number; windowId?: number; active?: boolean; url?: string } = {}) {
        return [...self.tabsById.values()]
          .filter(
            (tab) =>
              (query.groupId === undefined || tab.groupId === query.groupId) &&
              (query.windowId === undefined || tab.windowId === query.windowId) &&
              (query.active === undefined || tab.active === query.active) &&
              (query.url === undefined || tab.url === query.url),
          )
          .map(copy);
      },
      async create(options: { windowId?: number; url?: string; active?: boolean; openerTabId?: number }) {
        return copy(self.createTab(options));
      },
      async update(id: number, change: { active?: boolean; url?: string }) {
        const tab = self.tabsById.get(id);
        if (!tab) throw new Error(`No tab with id: ${id}.`);
        if (change.active) self.activate(tab);
        if (change.url) self.commit(tab, change.url);
        return copy(tab);
      },
      async remove(ids: number | number[]) {
        for (const id of [ids].flat()) self.removeTab(id);
      },
      async group(options: { tabIds: number | number[]; groupId?: number }) {
        // A round trip to the browser, during which other events arrive.
        await new Promise((resolve) => setTimeout(resolve));
        const ids = [options.tabIds].flat();
        let groupId = options.groupId;
        const first = self.tabsById.get(ids[0])!;
        if (groupId === undefined) {
          groupId = self.nextGroup++;
          self.groupsById.set(groupId, { id: groupId, windowId: first.windowId, color: "grey", collapsed: false });
        } else if (!self.groupsById.has(groupId)) throw new Error(`No group with id: ${groupId}.`);
        const group = self.groupsById.get(groupId)!;
        for (const id of ids) {
          const tab = self.tabsById.get(id)!;
          const previous = tab.groupId;
          tab.windowId = group.windowId;
          tab.groupId = groupId;
          tabs.onUpdated.emit(id, { groupId }, copy(tab));
          self.dropEmptyGroup(previous);
        }
        return groupId;
      },
      async ungroup(ids: number | number[]) {
        for (const id of [ids].flat()) {
          const tab = self.tabsById.get(id);
          if (!tab || tab.groupId < 0) continue;
          const previous = tab.groupId;
          tab.groupId = -1;
          tabs.onUpdated.emit(id, { groupId: -1 }, copy(tab));
          self.dropEmptyGroup(previous);
        }
      },
      async goBack() {},
      async goForward() {},
      async getZoom() {
        return 1;
      },
    };
    const tabGroups = {
      async get(id: number) {
        const group = self.groupsById.get(id);
        if (!group) throw new Error(`No group with id: ${id}.`);
        return { ...group };
      },
      async update(id: number, properties: Record<string, unknown>) {
        const group = self.groupsById.get(id);
        if (!group) throw new Error(`No group with id: ${id}.`);
        Object.assign(group, properties);
        return { ...group };
      },
      async query() {
        return [...self.groupsById.values()].map((group) => ({ ...group }));
      },
    };
    const windows = {
      WINDOW_ID_NONE: -1,
      onFocusChanged: ev(),
      async getLastFocused() {
        return { id: self.focused, focused: true, incognito: false, type: "normal" };
      },
      async getAll() {
        return self.windowIds.map((id) => ({ id, focused: id === self.focused, incognito: false, type: "normal" }));
      },
      async create(options: { url?: string; focused?: boolean }) {
        const id = self.nextWindow++;
        self.windowIds.push(id);
        if (options.focused) self.focused = id;
        const tab = self.createTab({ windowId: id, url: options.url, active: true });
        return { id, tabs: [copy(tab)] };
      },
      async update(id: number, change: { focused?: boolean }) {
        if (change.focused) self.focused = id;
        return { id };
      },
    };
    const area = (store: Map<string, unknown>) => ({
      async get(key: string) {
        return store.has(key) ? { [key]: structuredClone(store.get(key)) } : {};
      },
      async set(values: Record<string, unknown>) {
        for (const [key, value] of Object.entries(values)) store.set(key, structuredClone(value));
      },
    });
    const runtime = {
      id: "accdjhmcgljbadpeanajllmcembhkomn",
      get lastError() {
        return self.lastErrorMessage === undefined ? undefined : { message: self.lastErrorMessage };
      },
      connectNative() {
        const port = new FakePort(self);
        self.ports.push(port);
        if (!self.hostInstalled) queueMicrotask(() => port.close("Specified native messaging host not found."));
        return port;
      },
      getManifest: () => ({ version: "0.1.1.1", version_name: "0.1.1-preview.1" }),
      getURL: (path: string) => `chrome-extension://accdjhmcgljbadpeanajllmcembhkomn/${path}`,
      reload: () => void self.reloads++,
    };
    const action = {
      setBadgeText: async ({ text }: { text: string }) => void (self.badge = text),
      setBadgeBackgroundColor: async () => {},
      setBadgeTextColor: async () => {},
      setTitle: async () => {},
      getUserSettings: async () => ({ isOnToolbar: false }),
    };
    const debuggerApi = {
      onEvent: ev(),
      onDetach: ev(),
      async attach({ tabId }: { tabId: number }) {
        const tab = self.tabsById.get(tabId);
        if (!tab) throw new Error(`No tab with given id ${tabId}.`);
        if (!/^(https?:|about:)/.test(tab.url)) throw new Error(`Cannot access a ${tab.url.split(":")[0]}:// URL`);
        if (self.attached.has(tabId)) throw new Error("Another debugger is already attached to the tab with id: " + tabId);
        self.attached.add(tabId);
      },
      async detach({ tabId }: { tabId: number }) {
        self.detaches.push(tabId);
        if (!self.attached.delete(tabId)) throw new Error("Debugger is not attached to the tab with id: " + tabId);
        self.fetching.delete(tabId);
      },
      async sendCommand({ tabId }: { tabId: number; sessionId?: string }, method: string, params: any = {}) {
        if (!self.attached.has(tabId)) throw new Error("Debugger is not attached to the tab with id: " + tabId);
        return self.cdp(tabId, method, params);
      },
    };
    const downloads = { onCreated: ev(), onChanged: ev(), search: async () => [] };
    return {
      tabs,
      tabGroups,
      windows,
      storage: { session: area(this.stores.session), local: area(this.stores.local) },
      runtime,
      action,
      alarms: { onAlarm: ev(), create: async () => {}, clear: async () => true },
      commands: { onCommand: ev(), getAll: async () => [{ name: "stop-all", shortcut: "⌥⇧S" }] },
      debugger: debuggerApi,
      downloads,
    };
  }

  /**
   * `openerTabId` is what Chrome reports; `source` is the tab that really
   * opened it (whose group it joins) when that differs, as for a page in a
   * background tab, where Chrome names the window's active tab as opener.
   */
  createTab(options: { windowId?: number; url?: string; active?: boolean; openerTabId?: number; source?: number }) {
    const windowId = options.windowId ?? this.focused;
    const tab: FakeTab = {
      id: this.nextTab++,
      windowId,
      index: [...this.tabsById.values()].filter((other) => other.windowId === windowId).length,
      url: options.url ?? "about:blank",
      title: this.titles.get(options.url ?? "") ?? "",
      status: "complete",
      active: false,
      groupId: -1,
      openerTabId: options.openerTabId,
      incognito: false,
      pinned: false,
      highlighted: false,
      discarded: false,
      autoDiscardable: true,
      selected: false,
      frozen: false,
      loaderId: `loader-${++this.loaders}`,
    };
    this.tabsById.set(tab.id, tab);
    if (options.active) this.activate(tab);
    // An opener's group adopts the new tab, as Chrome does for links opened from a grouped tab.
    const from = options.source ?? options.openerTabId;
    const opener = from === undefined ? undefined : this.tabsById.get(from);
    if (opener && opener.groupId >= 0) tab.groupId = opener.groupId;
    this.chrome.tabs.onCreated.emit({ ...tab });
    // Chrome then reports the URL the tab was created with, as it starts loading it.
    setTimeout(() => {
      if (this.tabsById.get(tab.id) === tab) this.chrome.tabs.onUpdated.emit(tab.id, { url: tab.url, status: "loading" }, { ...tab });
    });
    return tab;
  }

  activate(tab: FakeTab) {
    for (const other of this.tabsById.values()) if (other.windowId === tab.windowId) other.active = other === tab;
  }

  removeTab(id: number) {
    const tab = this.tabsById.get(id);
    if (!tab) return;
    this.tabsById.delete(id);
    this.attached.delete(id);
    this.chrome.tabs.onRemoved.emit(id, { windowId: tab.windowId, isWindowClosing: false });
    this.dropEmptyGroup(tab.groupId);
  }

  dropEmptyGroup(groupId: number) {
    if (groupId >= 0 && ![...this.tabsById.values()].some((tab) => tab.groupId === groupId)) this.groupsById.delete(groupId);
  }

  /** A new document commits in the tab, as Chrome reports it: loading, then complete. */
  commit(tab: FakeTab, url: string) {
    tab.url = url;
    tab.title = this.titles.get(url) ?? "";
    tab.loaderId = `loader-${++this.loaders}`;
    tab.status = "loading";
    this.chrome.tabs.onUpdated.emit(tab.id, { url, status: "loading" }, { ...tab });
    tab.status = "complete";
    this.chrome.tabs.onUpdated.emit(tab.id, { status: "complete", title: tab.title }, { ...tab });
  }

  private event(tabId: number, method: string, params: unknown) {
    this.chrome.debugger.onEvent.emit({ tabId }, method, params);
  }

  /** The CDP methods the extension uses, as far as the tests need them. */
  async cdp(tabId: number, method: string, params: any): Promise<unknown> {
    const tab = this.tabsById.get(tabId)!;
    switch (method) {
      case "Page.getFrameTree":
        return { frameTree: { frame: { id: `frame-${tabId}`, url: tab.url, loaderId: tab.loaderId, securityOrigin: "", mimeType: "text/html" } } };
      case "Fetch.enable":
        this.fetching.set(tabId, { patterns: params.patterns });
        return {};
      case "Fetch.disable":
        this.fetching.delete(tabId);
        return {};
      case "Fetch.continueRequest":
      case "Fetch.failRequest": {
        const paused = this.paused.get(params.requestId);
        this.paused.delete(params.requestId);
        paused?.resolve(method === "Fetch.continueRequest" ? "continue" : "fail");
        return {};
      }
      case "Page.navigate":
        return this.navigate(tab, params.url);
      case "Page.getNavigationHistory":
        return { currentIndex: 0, entries: [{ id: 1, url: tab.url, userTypedURL: tab.url, title: tab.title, transitionType: "typed" }] };
      default:
        return {};
    }
  }

  /** Follows redirects hop by hop, pausing each document request while Fetch is on. */
  private async navigate(tab: FakeTab, start: string) {
    let url = start;
    for (let hop = 0; hop < 10; hop++) {
      if (this.fetching.has(tab.id)) {
        const requestId = `request-${++this.request}`;
        const decision = await new Promise<"continue" | "fail">((resolve) => {
          this.paused.set(requestId, { resolve });
          this.event(tab.id, "Fetch.requestPaused", {
            requestId,
            request: { url, method: "GET", headers: {} },
            frameId: `frame-${tab.id}`,
            resourceType: "Document",
          });
        });
        if (decision === "fail") return { frameId: `frame-${tab.id}`, errorText: "net::ERR_BLOCKED_BY_CLIENT" };
      }
      const next = this.redirects.get(url);
      if (!next) break;
      url = next;
    }
    this.commit(tab, url);
    for (const resource of this.subresources.get(url) ?? []) {
      if (!this.fetching.has(tab.id)) break;
      const requestId = `request-${++this.request}`;
      const decision = await new Promise<"continue" | "fail">((resolve) => {
        this.paused.set(requestId, { resolve });
        this.event(tab.id, "Fetch.requestPaused", {
          requestId,
          request: { url: resource.url, method: "GET", headers: {} },
          frameId: resource.frameId ?? `frame-${tab.id}`,
          resourceType: resource.resourceType,
        });
      });
      if (decision === "fail") this.refused.push(resource.url);
    }
    queueMicrotask(() => {
      this.event(tab.id, "Page.frameNavigated", { frame: { id: `frame-${tab.id}`, url, loaderId: tab.loaderId }, type: "Navigation" });
      this.event(tab.id, "Page.loadEventFired", { timestamp: 1 });
    });
    return { frameId: `frame-${tab.id}`, loaderId: tab.loaderId };
  }
}

export function installChrome() {
  const fake = new FakeChrome();
  (globalThis as unknown as { chrome: unknown }).chrome = fake.chrome;
  return fake;
}
