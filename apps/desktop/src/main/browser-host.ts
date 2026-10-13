import { browserSiteAccess, type BrowserPolicy } from "../browser-settings";
import { BrowserWindow, session } from "electron";
import { browserData, recordBrowserVisit, recordBrowserDownload } from "./browser-data";
import { randomUUID } from "node:crypto";
import WebSocket from "ws";
import { authorizationHeaders } from "@haarness/sdk";
import {
  Browser,
  decodeAction,
  decodeCommand,
  encodeOutcome,
} from "@haarness/browser-driver/rpc";
import { createBrowserPage, type BrowserPage } from "./browser-chromium";
import { createBrowserNetwork, type BrowserNetwork } from "./browser/network";
import { browserFailure } from "@haarness/browser-driver/errors";
import { destinationOrigin, normalizeURL, policyOrigin } from "@haarness/browser-driver/policy";
import { connections } from "./runtime";
import { data, persist } from "./store";
import { translate } from "../localization";
import type { DesktopEvent, BrowserLayout } from "../shared";

type Entry = {
  key: string;
  connection: string;
  thread: string;
  draftID?: string;
  win: BrowserWindow;
  endpoint: string;
  socket: WebSocket;
  lease: string;
  network?: BrowserNetwork;
  partition: string;
  pages: Map<Browser.TabID, BrowserPage>;
  tabs: Map<Browser.TabID, Browser.Tab>;
  /** Site icons for the Desktop tab strip, never sent to the server. */
  icons: Map<Browser.TabID, { origin: string; data: string }>;
  focusedTabID: Browser.TabID | null;
  requests: Map<string, AbortController>;
  requestTabs: Map<string, string>;
  queue: Promise<unknown>;
  ready: Promise<void>;
  disposed: boolean;
  layout?: BrowserLayout;
  save?: ReturnType<typeof setTimeout>;
  abort: AbortController;
  access?: { policy: BrowserPolicy; approved: Set<string>; error?: string };
};
export function createBrowserHost(
  emit: (win: BrowserWindow, event: DesktopEvent) => void,
) {
  const entries = new Map<string, Entry>();
  /** Browsing data being cleared, by connection. */
  const clearing = new Map<string, Promise<void>>();
  const key = (connection: string, thread: string) =>
    JSON.stringify([connection, thread]);
  const state = (e: Entry): Browser.State => ({
    tabs: [...e.tabs].map(([id, tab]) => e.pages.get(id)?.state() ?? tab),
    focusedTabID: e.focusedTabID,
  });
  /**
   * Sends the tabs to the server and the window. `focus` brings the Browser
   * panel forward (the agent opened or focused a tab); `reveal` says the
   * agent took the shown tab to another page, which reopens a closed panel
   * and marks the Browser tab when another panel is showing.
   */
  function publish(e: Entry, error?: string, focus = false, reveal = false) {
    if (e.disposed) return;
    const inventory = state(e);
    if (e.socket.readyState === WebSocket.OPEN)
      e.socket.send(JSON.stringify({ type: "state", state: inventory }));
    emit(e.win, {
      kind: "browser",
      connection: e.connection,
      id: e.thread,
      data: { state: withIcons(e, inventory), error, focus, reveal },
    });
    clearTimeout(e.save);
    e.save = setTimeout(() => {
      data().values["browser:" + e.key] = {
        tabs: inventory.tabs.map((tab) => ({
          id: tab.id,
          url: tab.url || e.tabs.get(tab.id)?.url || "about:blank",
          generation: tab.generation,
        })),
        focusedTabID: inventory.focusedTabID,
      };
      void persist();
    }, 200);
    layout(e, e.layout);
  }
  /** An icon shows only while its tab is still on the site it came from. */
  function withIcons(e: Entry, inventory: Browser.State): Browser.State {
    return {
      ...inventory,
      tabs: inventory.tabs.map((tab) => {
        const icon = e.icons.get(tab.id);
        return icon && icon.origin === origin(tab.url) ? { ...tab, favicon: icon.data } : tab;
      }),
    };
  }
  function origin(url: string) {
    try {
      return new URL(url).origin;
    } catch {
      return "";
    }
  }
  /**
   * Reads the page's first usable icon through the tab's own session (the
   * page has usually just loaded it, so this is a cache hit) and keeps it as a
   * small data URL; the renderer's CSP allows only data: images.
   */
  async function loadIcon(e: Entry, id: Browser.TabID, urls: string[]) {
    const page = e.pages.get(id);
    const candidate = urls.find((url) => /^(https?:|data:image\/)/.test(url));
    if (!page || !candidate) return;
    const site = origin(page.state().url);
    try {
      let data: string;
      if (candidate.startsWith("data:")) {
        if (candidate.length > 96_000) return;
        data = candidate;
      } else {
        const response = await page.contents.session.fetch(candidate, {
          signal: AbortSignal.timeout(5_000),
        });
        const type = (response.headers.get("content-type") ?? "").split(";")[0].trim();
        if (!response.ok || !/^image\/(png|x-icon|vnd\.microsoft\.icon|svg\+xml|gif|jpeg|webp|avif)$/.test(type))
          return;
        const bytes = Buffer.from(await response.arrayBuffer());
        if (bytes.length > 64_000) return;
        data = `data:${type};base64,${bytes.toString("base64")}`;
      }
      if (e.disposed || !e.pages.has(id)) return;
      e.icons.set(id, { origin: site, data });
      publish(e);
    } catch {
      /* A missing icon leaves the globe; it is never worth an error. */
    }
  }
  function layout(e: Entry, value?: BrowserLayout) {
    e.layout = value;
    for (const [id, page] of e.pages) {
      if (!value?.visible || id !== e.focusedTabID || e.disposed)
        page.setVisible(false);
    }
    if (!value?.visible || e.disposed || !e.focusedTabID) return;
    const b = value.bounds;
    const size = e.win.getContentSize();
    const bounds = {
      x: Math.max(0, Math.round(b.x)),
      y: Math.max(0, Math.round(b.y)),
      width: Math.max(
        0,
        Math.min(Math.round(b.width), size[0] - Math.max(0, Math.round(b.x))),
      ),
      height: Math.max(
        0,
        Math.min(Math.round(b.height), size[1] - Math.max(0, Math.round(b.y))),
      ),
    };
    if (bounds.width <= 0 || bounds.height <= 0) return;
    const page =
      e.pages.get(e.focusedTabID) ??
      (e.tabs.has(e.focusedTabID)
        ? create(e, true, undefined, e.tabs.get(e.focusedTabID))
        : undefined);
    page?.layout(bounds, undefined, 0);
    page?.setVisible(true);
  }
  function focus(e: Entry, id: Browser.TabID) {
    e.focusedTabID = id;
    publish(e, undefined, true);
  }
  async function closePage(e: Entry, id: Browser.TabID) {
    for (const [request, tab] of e.requestTabs)
      if (tab === id) e.requests.get(request)?.abort();
    const page = e.pages.get(id);
    e.pages.delete(id);
    e.tabs.delete(id);
    e.icons.delete(id);
    if (e.focusedTabID === id)
      e.focusedTabID = e.tabs.keys().next().value ?? null;
    await page?.dispose();
    publish(e);
  }
  function create(
    e: Entry,
    initialize = true,
    popupOptions?: Electron.BrowserWindowConstructorOptions,
    restore?: Browser.Tab,
  ): BrowserPage {
    if (!e.network || e.disposed)
      throw new Error("Browser network is unavailable");
    if (e.tabs.size >= 32 && !restore)
      throw new Error("Close a browser tab before opening more (32 maximum)");
    const id = restore?.id ?? Browser.TabID.make("tab_" + randomUUID());
    let lastVisited = "";
    const page = createBrowserPage(e.win, {
      id,
      partition: e.partition,
      network: e.network,
      initialize,
      restore,
      popupOptions,
      publish: (error) => {
        if (e.pages.has(id)) {
          const tab = e.pages.get(id)!.state();
          if (!tab.loading && tab.url && tab.url !== lastVisited) {
            lastVisited = tab.url;
            recordBrowserVisit(e.connection, e.thread, tab.url, tab.title);
          }
          publish(e, error);
        }
      },
      downloaded: (file) => {
        void recordBrowserDownload(e.win, e.connection, { id: file.id, thread: e.thread, url: file.url, name: file.name, bytes: file.bytes, state: file.state, time: new Date().toISOString() }, file.path, () => !e.disposed).catch((error) => publish(e, String(error)));
      },
      fail: () => {
        void closePage(e, id);
      },
      favicon: (urls) => void loadIcon(e, id, urls),
      agentCursor: () => data().preferences.browserAgentCursor !== false,
      translate: (key) => translate(data().preferences.language, key),
      popup: (options) => {
        const popup = create(e, false, options);
        focus(e, popup.state().id);
        return popup.contents;
      },
    });
    e.pages.set(id, page);
    e.tabs.set(id, restore ?? page.state());
    void page.ready
      .then(() => publish(e))
      .catch((error) => {
        publish(e, String(error));
      });
    return page;
  }
  async function execute(
    e: Entry,
    command: Browser.Command,
    signal: AbortSignal,
    agent = false,
  ): Promise<Browser.Result> {
    if (signal.aborted || e.disposed)
      throw new Error(
        "Browser command cancelled. Inspect the page before repeating a mutation.",
      );
    const a = command.action;
    if (a.type === "tabs.list")
      return {
        value: command.inspect ? { resources: [], key: "inventory" } : state(e),
        files: [],
      };
    if (a.type === "tabs.open") {
      if (command.inspect)
        return {
          value: {
            resources: [normalizeURL(a.url ?? "about:blank")],
            key: "open",
          },
          files: [],
        };
      const page = create(e);
      if (a.focus !== false) focus(e, page.state().id);
      await page.ready;
      await page.execute(
        {
          action: {
            type: "navigate",
            tabID: page.state().id,
            url: a.url ?? "about:blank",
          },
          files: [],
        },
        signal,
      );
      publish(e);
      return { value: page.state(), files: [] };
    }
    const tab = state(e).tabs.find((tab) => tab.id === a.tabID);
    if (!tab)
      throw new Error(
        "Tab is closed or belongs to another conversation. Call browser_tabs_list.",
      );
    if (a.type === "tabs.focus" || a.type === "tabs.close") {
      const target = {
        resources: [tab.url],
        key: tab.id + ":" + tab.generation,
      };
      if (command.inspect) return { value: target, files: [] };
      if (
        command.target &&
        JSON.stringify(command.target) !== JSON.stringify(target)
      )
        throw new Error("Browser target changed while permission was pending");
      if (a.type === "tabs.focus") {
        focus(e, a.tabID);
        return { value: tab, files: [] };
      }
      await closePage(e, a.tabID);
      return { value: state(e), files: [] };
    }
    const page = e.pages.get(a.tabID) ?? create(e, true, undefined, tab);
    await page.ready;
    const result = await page.execute(command, signal);
    // The agent took the tab the Browser shows to another page: the Browser
    // comes back if the user closed it. A reload is the same page again.
    publish(
      e,
      undefined,
      false,
      agent &&
        !command.inspect &&
        (a.type === "navigate" || a.type === "back" || a.type === "forward") &&
        a.tabID === e.focusedTabID,
    );
    return result;
  }
  async function dispose(
    e: Entry,
    reason = "Browser disconnected",
    forget = false,
  ) {
    if (e.disposed) return;
    e.disposed = true;
    clearTimeout(e.save);
    // Clearing browsing data closes the tabs for good instead of saving them
    // for the next attach to reopen.
    const inventory: Browser.State = forget
      ? { tabs: [], focusedTabID: null }
      : state(e);
    if (!forget) {
      data().values["browser:" + e.key] = {
        tabs: inventory.tabs.map((tab) => ({
          id: tab.id,
          url: tab.url || e.tabs.get(tab.id)?.url || "about:blank",
          generation: tab.generation,
        })),
        focusedTabID: inventory.focusedTabID,
      };
      void persist();
    }
    e.abort.abort();
    for (const abort of e.requests.values()) abort.abort();
    e.socket.close();
    await Promise.allSettled(
      [...e.pages.values()].map((page) => page.dispose()),
    );
    await e.network?.dispose();
    if (entries.get(e.key) === e) entries.delete(e.key);
    emit(e.win, {
      kind: "browser",
      connection: e.connection,
      id: e.thread,
      data: { state: inventory, error: reason, disconnected: true },
    });
  }
  async function attach(
    win: BrowserWindow,
    connection: string,
    thread: string,
  ) {
    if (thread.length > 128) throw new Error("Invalid browser owner");
    // A reconnect while browsing data is being cleared waits for the clear,
    // so it cannot reopen the tabs being cleared.
    await clearing.get(connection)?.catch(() => {});
    const owner = connections.get(connection);
    if (!owner) throw new Error("Project disconnected");
    // Verify that this conversation belongs to the routed project before attaching native views.
    if (thread) await owner.client.getThread(thread);
    else if (!owner.view.info.capabilities.includes("browser-drafts.v1"))
      throw new Error("Update this project's server to browse before starting a chat.");
    const id = key(connection, thread);
    let e = entries.get(id);
    if (e && e.win === win && e.endpoint === owner.url && !e.disposed) {
      await e.ready;
      publish(e);
      return state(e);
    }
    if (e) await dispose(e, "Browser opened in another window");
    let draftID: string | undefined;
    if (!thread) {
      const stored = data().values["browser-draft:" + connection];
      draftID = typeof stored === "string" && /^[a-f0-9-]{36}$/.test(stored) ? stored : randomUUID();
      data().values["browser-draft:" + connection] = draftID;
    }
    const endpoint = owner.url.replace(/\/$/, "") + (thread
      ? "/v1/threads/" + encodeURIComponent(thread) + "/browser"
      : "/v1/browser/drafts/" + draftID);
    const socket = new WebSocket(endpoint.replace(/^http/, "ws") + "/host?version=2", {
      headers: authorizationHeaders(owner.auth),
      maxPayload: 8 * 1024 * 1024,
    });
    const saved = data().values["browser:" + id] as
      | {
          tabs?: { id: string; url: string; generation: number }[];
          focusedTabID?: string;
        }
      | undefined;
    const tabs = (saved?.tabs ?? [])
      .filter(
        (tab) =>
          /^tab_[a-f0-9-]{36}$/.test(tab.id) &&
          (destinationOrigin(tab.url) || tab.url === "about:blank"),
      )
      .slice(0, 32)
      .map((tab) => ({
        id: Browser.TabID.make(tab.id),
        url: tab.url,
        title: "",
        generation:
          (Number.isSafeInteger(tab.generation) ? tab.generation : 0) + 1,
        loading: false,
        canGoBack: false,
        canGoForward: false,
      }));
    let resolveReady!: () => void, rejectReady!: (error: Error) => void;
    const ready = new Promise<void>((resolve, reject) => {
      resolveReady = resolve;
      rejectReady = reject;
    });
    e = {
      key: id,
      connection,
      thread,
      draftID,
      win,
      endpoint: owner.url,
      socket,
      lease: "",
      partition: "haarness-browser-" + randomUUID(),
      pages: new Map(),
      tabs: new Map(tabs.map((tab) => [tab.id, tab])),
      icons: new Map(),
      focusedTabID:
        tabs.find((tab) => tab.id === saved?.focusedTabID)?.id ??
        tabs[0]?.id ??
        null,
      requests: new Map(),
      requestTabs: new Map(),
      queue: Promise.resolve(),
      ready,
      disposed: false,
      abort: new AbortController(),
    };
    entries.set(id, e);
    const entry = e;
    const timer = setTimeout(() => {
      rejectReady(new Error("Browser connection timed out"));
      void dispose(entry);
    }, 15000);
    let alive = true;
    socket.on("pong", () => {
      alive = true;
    });
    const heartbeat = setInterval(() => {
      if (socket.readyState !== WebSocket.OPEN) return;
      if (!alive) {
        socket.terminate();
        return;
      }
      alive = false;
      socket.ping();
    }, 20000);
    socket.on("error", (error) => rejectReady(error));
    socket.on("close", () => {
      clearInterval(heartbeat);
      clearTimeout(timer);
      rejectReady(new Error("Browser connection closed"));
      void dispose(entry);
    });
    socket.on("message", (raw) => {
      void (async () => {
        const message = JSON.parse(raw.toString());
        if (message.type === "attached") {
          if (message.version !== 1)
            throw new Error(
              "Update Desktop and the Haarness server for browser compatibility",
            );
          entry.lease = message.lease;
          entry.network = await createBrowserNetwork(
            entry.partition,
            async (action, input, signal) => {
              const response = await fetch(endpoint + "/tunnel/" + action, {
                method: "POST",
                headers: {
                  ...authorizationHeaders(owner.auth),
                  "Content-Type": "application/json",
                },
                body: JSON.stringify({ ...input, lease: entry.lease }),
                signal: AbortSignal.any([
                  entry.abort.signal,
                  signal ?? AbortSignal.timeout(35000),
                ]),
              });
              if (!response.ok) throw new Error(await response.text());
              return response.json();
            },
            (url) => {
              const access = entry.access;
              // WebSockets count as their HTTP(S) origin; addresses that
              // reach no site (data:, blob:, about:) are the page's own.
              const target = policyOrigin(url);
              if (!access || !target) return true;
              const { origin } = target;
              if (target.credentials) {
                access.error = `Browser site permission blocked an address on ${origin} that carries a user name or password; agents cannot use such addresses.`;
                return false;
              }
              const decision = browserSiteAccess(access.policy, origin).browsing;
              if (access.policy.enabled && (decision === "allow" || (decision === "ask" && access.approved.has(origin)))) return true;
              access.error = `Browser site permission blocked ${origin}. ${decision === "ask" ? "Navigate to this site directly to request approval." : "This site is blocked in Settings → Browser."}`;
              return false;
            },
          );
          if (entry.disposed) {
            await entry.network.dispose();
            return;
          }
          publish(entry);
          clearTimeout(timer);
          resolveReady();
          return;
        }
        if (message.type === "cancel") {
          entry.requests.get(message.id)?.abort();
          return;
        }
        if (message.type !== "command") return;
        const abort = new AbortController();
        entry.requests.set(message.id, abort);
        if (typeof message.command?.action?.tabID === "string")
          entry.requestTabs.set(message.id, message.command.action.tabID);
        entry.queue = entry.queue
          .catch(() => {})
          .then(async () => {
            let outcome: Browser.Outcome;
            try {
              const command = decodeCommand(message.command);
              if (command.policy) entry.access = {
                policy: command.policy as BrowserPolicy,
                approved: new Set((command.target?.resources ?? (command.action.type === "tabs.open" ? [normalizeURL(command.action.url ?? "about:blank")] : [])).map(destinationOrigin).filter((origin): origin is string => !!origin)),
              };
              const result = await execute(entry, command, abort.signal, true);
              if (entry.access?.error) throw new Error(entry.access.error);
              outcome = { type: "success", result };
            } catch (error) {
              outcome = browserFailure(
                message.command?.action ?? { type: "tabs.list" },
                error,
              );
            }
            entry.access = undefined;
            entry.requests.delete(message.id);
            entry.requestTabs.delete(message.id);
            if (!entry.disposed && socket.readyState === WebSocket.OPEN)
              socket.send(
                JSON.stringify({
                  type: "result",
                  id: message.id,
                  outcome: encodeOutcome(outcome),
                }),
              );
          });
      })().catch((error) => {
        rejectReady(error);
        void dispose(entry, String(error));
      });
    });
    win.once("closed", () => {
      void dispose(entry);
    });
    await ready;
    return state(entry);
  }
  return {
    attach,
    async adopt(win: BrowserWindow, connection: string, thread: string) {
      const source = key(connection, "");
      const saved = data().values["browser:" + source] as { tabs?: unknown[] } | undefined;
      if (!entries.has(source) && !saved?.tabs?.length) return;
      await attach(win, connection, "");
      const e = entries.get(source);
      if (!e || !e.tabs.size || !e.draftID) return;
      const owner = connections.get(connection);
      if (!owner) throw new Error("Project disconnected");
      await owner.client.getThread(thread);
      const destination = key(connection, thread);
      if (entries.has(destination)) throw new Error("This conversation already has a browser");
      const transfer = e.queue.catch(() => {}).then(async () => {
        if (e.disposed || entries.get(source) !== e) throw new Error("The new-chat browser changed before transfer");
        const response = await fetch(owner.url.replace(/\/$/, "") + "/v1/browser/drafts/" + e.draftID + "/adopt", {
          method: "POST",
          headers: { ...authorizationHeaders(owner.auth), "Content-Type": "application/json" },
          body: JSON.stringify({ thread, lease: e.lease }),
          signal: AbortSignal.any([e.abort.signal, AbortSignal.timeout(15000)]),
        });
        if (!response.ok) throw new Error(await response.text());
        clearTimeout(e.save);
        entries.delete(source);
        e.thread = thread;
        e.key = destination;
        entries.set(destination, e);
        delete data().values["browser:" + source];
        delete data().values["browser-draft:" + connection];
        data().values["browser:" + destination] = state(e);
        const records = browserData(connection);
        for (const record of [...records.history, ...records.downloads])
          if (!record.thread) record.thread = thread;
        await persist();
        publish(e);
      });
      e.queue = transfer;
      await transfer;
    },
    async command(
      win: BrowserWindow,
      connection: string,
      thread: string,
      raw: unknown,
    ) {
      await attach(win, connection, thread);
      const e = entries.get(key(connection, thread));
      if (!e) throw new Error("The browser moved to its new conversation; try again there.");
      // UI controls have a deliberately small surface; agent operations use server permissions.
      const action = decodeAction(raw);
      if (
        ![
          "tabs.list",
          "tabs.open",
          "tabs.close",
          "tabs.focus",
          "navigate",
          "back",
          "forward",
          "reload",
          "stop",
        ].includes(action.type)
      )
        throw new Error("This operation is available through browser tools");
      if (["tabs.close", "tabs.focus", "stop"].includes(action.type)) {
        return (
          await execute(e, { action, files: [] }, new AbortController().signal)
        ).value;
      }
      const result = e.queue
        .catch(() => {})
        .then(() =>
          execute(e, { action, files: [] }, new AbortController().signal),
        );
      e.queue = result;
      return (await result).value;
    },
    layout(
      win: BrowserWindow,
      connection: string,
      thread: string,
      value: BrowserLayout,
    ) {
      // Hiding any other view first prevents stale conversation content showing during transitions.
      for (const e of entries.values())
        if (e.win === win)
          layout(e, e.key === key(connection, thread) ? value : undefined);
    },
    hide(win: BrowserWindow) {
      for (const e of entries.values()) if (e.win === win) layout(e, undefined);
    },
    /** The focused page as drawn now, so the window can hold a still copy
        while a menu or dialog has to paint where the native view sits. */
    async snapshot(win: BrowserWindow, connection: string, thread: string) {
      const e = entries.get(key(connection, thread));
      const page = e?.focusedTabID ? e.pages.get(e.focusedTabID) : undefined;
      if (!e || e.win !== win || e.disposed || !page || !page.view.getVisible()) return null;
      const image = await page.contents.capturePage();
      return image.isEmpty() ? null : "data:image/jpeg;base64," + image.toJPEG(85).toString("base64");
    },
    async clear(connection: string) {
      // Forget the saved tabs before anything awaits: an attach that reads
      // them later starts empty, and one already attached is disposed below.
      for (const stored of Object.keys(data().values)) {
        if (!stored.startsWith("browser:")) continue;
        try { if (JSON.parse(stored.slice(8))[0] === connection) delete data().values[stored]; } catch { /* Ignore unrelated or obsolete state. */ }
      }
      delete data().values["browser-draft:" + connection];
      const owned = [...entries.values()].filter((entry) => entry.connection === connection);
      const cleared = (async () => {
        // Every view stops publishing (and saving) before the first wipe awaits.
        await Promise.all(
          owned.map((entry) =>
            dispose(entry, "Browsing data cleared. Reconnecting…", true),
          ),
        );
        for (const entry of owned) {
          await session.fromPartition(entry.partition).clearStorageData();
          await session.fromPartition(entry.partition).clearCache();
        }
        await persist();
      })();
      clearing.set(connection, cleared);
      try {
        await cleared;
      } finally {
        if (clearing.get(connection) === cleared) clearing.delete(connection);
      }
    },
    disconnect(connection: string) {
      for (const e of entries.values())
        if (e.connection === connection)
          void dispose(e, "Project disconnected");
    },
    dispose() {
      for (const e of entries.values()) void dispose(e);
    },
  };
}
