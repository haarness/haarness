/**
 * The `Platform` the core runs on, built from `chrome.*`.
 *
 * `listen()` registers every Chrome listener synchronously, as MV3 requires:
 * a listener added later (after an await) can miss the event that woke the
 * service worker.
 */
import { createAttribution } from "../../core/downloads";
import type { GroupsPort, LiveTab, Platform, PlatformEvents, TabsPort } from "../../core/platform";
import { createDebugger } from "./cdp";
import { createDownloads } from "./downloads";
import { createFileStore, type ChromeFileStore } from "./files";
import type { TabCache } from "./page";
import { createSession, type ChromeTabSession } from "./session";

export function toLive(tab: chrome.tabs.Tab): LiveTab {
  return {
    id: tab.id ?? -1,
    windowId: tab.windowId,
    groupId: tab.groupId ?? -1,
    url: tab.url || tab.pendingUrl || "",
    title: tab.title ?? "",
    loading: tab.status === "loading",
    active: tab.active,
    openerTabId: tab.openerTabId,
  };
}

const tabs: TabsPort = {
  get: (tabId) => chrome.tabs.get(tabId).then(toLive, () => undefined),
  async create({ url, active }) {
    // The window the user used last; Haarness never brings one forward.
    const last = await chrome.windows.getLastFocused({ windowTypes: ["normal"] }).catch(() => undefined);
    let windowId = last && !last.incognito && last.type === "normal" ? last.id : undefined;
    if (windowId === undefined) {
      const all = await chrome.windows.getAll({ windowTypes: ["normal"] });
      windowId = all.find((window) => !window.incognito)?.id;
    }
    if (windowId === undefined) {
      const window = await chrome.windows.create({ url, focused: false });
      return toLive(window!.tabs![0]);
    }
    return toLive(await chrome.tabs.create({ windowId, url, active }));
  },
  remove: async (tabIds) => {
    if (tabIds.length) await chrome.tabs.remove(tabIds);
  },
  activate: async (tabId) => {
    await chrome.tabs.update(tabId, { active: true });
  },
  raiseWindow: async (windowId) => {
    await chrome.windows.update(windowId, { focused: true });
  },
  async focusedWindow() {
    const window = await chrome.windows.getLastFocused().catch(() => undefined);
    return window?.focused ? window.id : undefined;
  },
  async activeTab(windowId) {
    const [tab] = await chrome.tabs.query({ windowId, active: true });
    return tab?.id;
  },
};

/** Tab groups exist in Chrome, Edge and Brave; a browser without them gets ungrouped tracking. */
export function groupsPort(): GroupsPort | undefined {
  if (typeof chrome.tabGroups?.update !== "function" || typeof chrome.tabs.group !== "function") return;
  return {
    add: (tabIds, groupId) =>
      chrome.tabs.group(groupId === undefined ? { tabIds: tabIds as [number, ...number[]] } : { tabIds: tabIds as [number, ...number[]], groupId }),
    ungroup: async (tabIds) => {
      if (tabIds.length) await chrome.tabs.ungroup(tabIds as [number, ...number[]]);
    },
    update: async (groupId, properties) => {
      await chrome.tabGroups.update(groupId, properties as chrome.tabGroups.UpdateProperties);
    },
    exists: (groupId) => chrome.tabGroups.get(groupId).then(
      () => true,
      () => false,
    ),
    tabs: (groupId) => chrome.tabs.query({ groupId }).then((list) => list.map(toLive)),
  };
}

export function createChromePlatform() {
  let events: PlatformEvents | undefined;
  let uploadsRoot: string | null = null;
  let cursor = true;
  const caches = new Map<number, TabCache>();
  const stores = new Map<number, ChromeFileStore>();
  const attribution = createAttribution();
  const uuid = () => crypto.randomUUID();

  const cache = (tabId: number) => {
    let value = caches.get(tabId);
    if (!value) caches.set(tabId, (value = { url: "", title: "", loading: false, zoom: 1 }));
    return value;
  };
  const store = (tabId: number) => {
    let value = stores.get(tabId);
    if (!value) stores.set(tabId, (value = createFileStore(uuid)));
    return value;
  };

  const downloads = createDownloads({ attribution, store, existing: (tabId) => stores.get(tabId), uuid });

  const debuggers = createDebugger<ChromeTabSession>(
    { detached: (tabId, reason) => events?.debuggerDetached(tabId, reason) },
    (transport) => {
      const tabCache = cache(transport.tabId);
      void chrome.tabs.get(transport.tabId).then(
        (tab) => Object.assign(tabCache, { url: tab.url || tab.pendingUrl || "", title: tab.title ?? "", loading: tab.status === "loading" }),
        () => {},
      );
      void chrome.tabs.getZoom(transport.tabId).then((zoom) => (tabCache.zoom = zoom), () => {});
      return createSession(transport, {
        cache: tabCache,
        files: store(transport.tabId),
        attribution,
        uploadsRoot: () => uploadsRoot,
        cursor: () => cursor,
        now: () => Date.now(),
      });
    },
  );

  const platform: Platform = {
    tabs,
    groups: groupsPort(),
    debugger: {
      session: (tabId) => debuggers.session(tabId),
      attached: (tabId) => debuggers.attached(tabId),
      park: (tabId) => debuggers.park(tabId),
      detach: (tabId) => debuggers.detach(tabId),
      detachAll: () => debuggers.detachAll(),
    },
    session: {
      get: async <T>(key: string) => (await chrome.storage.session.get(key))[key] as T | undefined,
      set: (key, value) => chrome.storage.session.set({ [key]: value }),
    },
    local: {
      get: async <T>(key: string) => (await chrome.storage.local.get(key))[key] as T | undefined,
      set: (key, value) => chrome.storage.local.set({ [key]: value }),
    },
    connectNative(name, handlers) {
      let port: chrome.runtime.Port;
      try {
        port = chrome.runtime.connectNative(name);
      } catch (error) {
        setTimeout(() => handlers.disconnect(error instanceof Error ? error.message : String(error)));
        return { post() {}, close() {} };
      }
      port.onMessage.addListener((message) => handlers.message(message));
      port.onDisconnect.addListener(() => handlers.disconnect(chrome.runtime.lastError?.message));
      return {
        post(message) {
          try {
            port.postMessage(message);
          } catch {
            // The port closed; onDisconnect reports it.
          }
        },
        close: () => port.disconnect(),
      };
    },
    badge(text, kind) {
      void chrome.action.setBadgeText({ text });
      if (!text) return;
      void chrome.action.setBadgeBackgroundColor({ color: kind === "waiting" ? "#dba273" : "#2c2a27" });
      void chrome.action.setBadgeTextColor?.({ color: kind === "waiting" ? "#151413" : "#eae5dd" });
    },
    title(text) {
      void chrome.action.setTitle({ title: text });
    },
    alarm(name, minutes) {
      if (minutes === null) void chrome.alarms.clear(name);
      else void chrome.alarms.create(name, { delayInMinutes: minutes, periodInMinutes: minutes });
    },
    reload: () => chrome.runtime.reload(),
    version: () => {
      const manifest = chrome.runtime.getManifest();
      return manifest.version_name ?? manifest.version;
    },
    browser: browserInfo,
    setUploadsRoot(root) {
      uploadsRoot = root;
    },
    setCursor(enabled) {
      cursor = enabled;
    },
    now: () => Date.now(),
    uuid,
  };

  /** Registers every Chrome listener now, delivering to `target`. */
  function listen(target: PlatformEvents) {
    events = target;
    chrome.tabs.onCreated.addListener((tab) => target.tabCreated(toLive(tab)));
    chrome.tabs.onUpdated.addListener((tabId, change, tab) => {
      const tabCache = caches.get(tabId);
      if (tabCache)
        Object.assign(tabCache, {
          url: tab.url || tab.pendingUrl || tabCache.url,
          title: tab.title ?? tabCache.title,
          loading: tab.status === "loading",
        });
      // A tab Haarness let go of while idle starts loading a new document.
      if (change.status === "loading") debuggers.navigated(tabId);
      if (change.url === undefined && change.groupId === undefined && change.title === undefined && change.status === undefined)
        return;
      target.tabUpdated(
        tabId,
        { url: change.url, groupId: change.groupId, title: change.title, status: change.status },
        toLive(tab),
      );
    });
    chrome.tabs.onRemoved.addListener((tabId) => {
      caches.delete(tabId);
      void stores.get(tabId)?.dispose();
      stores.delete(tabId);
      target.tabRemoved(tabId);
    });
    chrome.tabs.onReplaced.addListener((added, removed) => target.tabReplaced(added, removed));
    chrome.tabs.onZoomChange.addListener(({ tabId, newZoomFactor }) => {
      const tabCache = caches.get(tabId);
      if (tabCache) tabCache.zoom = newZoomFactor;
    });
    chrome.windows.onFocusChanged.addListener((windowId) =>
      target.windowFocused(windowId === chrome.windows.WINDOW_ID_NONE ? undefined : windowId),
    );
    chrome.debugger.onEvent.addListener((source, method, params) => debuggers.event(source, method, params));
    chrome.debugger.onDetach.addListener((source, reason) => debuggers.detachedBy(source, reason));
    chrome.downloads.onCreated.addListener((item) => downloads.created(item));
    chrome.downloads.onChanged.addListener((delta) => void downloads.changed(delta, (done) => target.downloadFinished(done)));
    chrome.commands.onCommand.addListener((name) => {
      if (name === "stop-all") target.stopAll();
    });
    chrome.alarms.onAlarm.addListener((alarm) => target.alarm(alarm.name));
  }

  return { platform, listen };
}

/** The browser's own name ("Google Chrome", "Microsoft Edge", "Brave") and full version. */
async function browserInfo() {
  const data = (navigator as Navigator & {
    userAgentData?: {
      brands: { brand: string; version: string }[];
      getHighEntropyValues(hints: string[]): Promise<{ fullVersionList?: { brand: string; version: string }[] }>;
    };
  }).userAgentData;
  const pick = (brands: { brand: string; version: string }[]) =>
    brands.find((entry) => !/not.?a.?brand|^chromium$/i.test(entry.brand)) ??
    brands.find((entry) => /^chromium$/i.test(entry.brand));
  const fallback = /Chrome\/([\d.]+)/.exec(navigator.userAgent)?.[1] ?? "";
  if (!data) return { name: "Chrome", version: fallback };
  const high = await data.getHighEntropyValues(["fullVersionList"]).catch(() => ({ fullVersionList: undefined }));
  const brand = pick(high.fullVersionList ?? []) ?? pick(data.brands);
  return { name: brand?.brand ?? "Chrome", version: brand?.version ?? fallback };
}
