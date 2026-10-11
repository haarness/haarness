/**
 * The browser host: everything the service worker does between the bridge,
 * the servers behind it, and this browser's tabs.
 *
 * - Each server sends `command`s for a chat (thread). Commands for one chat run
 *   in order; the chat's inventory (`state`) is always sent before a command's
 *   `result`, so the server never answers from a stale inventory.
 * - A chat only ever touches the tabs in its own tab group. Tabs the user drags
 *   in are shared with it; tabs dragged out or closed by the user are revoked.
 * - Pause, Stop and Chrome's debugging bar are the user's: while any of them
 *   holds, every command fails with a code the server explains to the model.
 */
import {
  decodeHostCommand,
  encodeHostOutcome,
  MAX_TABS,
  methods,
  tabID,
  type Action,
  type HostCommand,
  type HostResult,
  type Outcome,
  type State,
  type Tab,
} from "./browser";
import { approvalLines, badgeText } from "./approvals";
import { createBridge, RECONNECT_ALARM } from "./bridge";
import { isSensitiveHeader } from "@haarness/browser-driver/diagnostics";
import { createPage as driverCreatePage, lastRef } from "./driver";
import type { CreatePage, Page } from "./driver-contract";
import { CANCELLED, failure, HostError, NOT_THIS_CHAT, TARGET_CHANGED } from "./errors";
import type { FetchContext } from "./fetch-policy";
import { groupColor, groupTitle } from "./groups";
import { key } from "./i18n";
import type { LiveTab, Platform, PlatformEvents, TabChange, TabSession } from "./platform";
import {
  allowedOnBlank,
  destinationOrigin,
  normalizeURL,
  restricted,
  siteAccess,
  supportedInChrome,
  wwwTwin,
} from "./policy";
import { PROTOCOL, type Approval, type FromBridge, type RawCommand, type ServerCommand, type ServerEntry, type ToBridge } from "./protocol";
import { createQueues } from "./queue";
import { createRegistry, threadKey, wireState, wireTab, type Snapshot, type TabRecord, type ThreadRecord } from "./registry";
import { pill, type Connection } from "./status";
import type { UiRequest, UiResponse, UiSettings, UiSnapshot } from "./ui";
import { compareVersions } from "./version";

interface Server extends ServerEntry {
  attached?: { workspace: string; instance: string };
}

const OPEN_LIMIT = "Close a browser tab before opening more (32 maximum).";
const IDLE_DETACH_MS = 30_000;
const REQUEST_TIMEOUT_MS = 15_000;
const REFUSAL_NOTE_MS = 120_000;
/** How long a reload's hand-over stays valid for the instance that starts next. */
const HANDOFF_MS = 60_000;

/** This browser session's state, kept in storage.local across the extension's own reload. */
interface Handoff {
  at: number;
  registry: unknown;
  refSeed: number;
  stopped: boolean;
}

export interface HostOptions {
  platform: Platform;
  /** The driver's page factory; tests pass a fake. */
  createPage?: CreatePage;
  /** Translates the agent cursor's labels and the toolbar tooltip. */
  translate?(key: string): string;
  /** The user's interface language, at start and whenever it changes. */
  language?(code: string): void;
}

export function createHost(options: HostOptions) {
  const { platform } = options;
  const createPage = options.createPage ?? driverCreatePage;
  const registry = createRegistry(
    {
      load: () => platform.session.get<Snapshot>("registry"),
      save: (snapshot) => platform.session.set("registry", snapshot),
    },
    platform.now,
  );
  const queues = createQueues();
  const servers = new Map<string, Server>();
  const activity = new Map<string, { active: Set<string>; approvals: Approval[] }>();
  const pages = new Map<number, Page>();
  const idle = new Map<string, ReturnType<typeof setTimeout>>();
  /** Tabs the agent is closing, so their removal is not read as the user's. */
  const closing = new Set<number>();
  const waiting = new Map<string, (message: FromBridge | undefined) => void>();
  const watchers = new Set<(snapshot: UiSnapshot) => void>();
  const publishing = new Set<string>();

  const control = { paused: false, stopped: false, focusedAt: 0 };
  let settings: UiSettings = { language: "en", label: "", cursor: true };
  let browserId = "";
  let browser = { name: "Chrome", version: "" };
  let bridgeInfo: { version: string; extension: { path: string; version: string } | null } | null = null;
  let update: string | null = null;
  let restartWhenIdle = false;
  let shortcut: string | null = null;
  let notifyTimer: ReturnType<typeof setTimeout> | undefined;
  let statusTimer: ReturnType<typeof setTimeout> | undefined;
  /**
   * The last element ref the driver minted, persisted for the browser session:
   * after a worker restart, new pages number their refs past it, so a ref the
   * model kept from before never names a different element.
   */
  let refSeed = 0;

  const bridge = createBridge(platform, {
    hello,
    message: onBridge,
    connection(connection) {
      if (connection !== "connected") {
        servers.clear();
        activity.clear();
        bridgeInfo = connection === "mismatch" ? bridgeInfo : null;
        // Nobody can receive these answers any more; the server reports the recovery text.
        queues.cancelAll();
        for (const resolve of [...waiting.values()]) resolve(undefined);
      }
      refresh();
    },
  });

  // --------------------------------------------------------------- startup

  let started: Promise<void> | undefined;
  function start() {
    started ??= (async () => {
      await takeHandoff();
      const [id, paused, saved, stopped, seed, focused, info] = await Promise.all([
        platform.local.get<string>("browserId"),
        platform.local.get<boolean>("paused"),
        platform.local.get<Partial<UiSettings>>("settings"),
        platform.session.get<boolean>("stopped"),
        platform.session.get<number>("refSeed"),
        platform.tabs.focusedWindow().catch(() => undefined),
        platform.browser().catch(() => browser),
      ]);
      if (Number.isSafeInteger(seed) && seed! > 0) refSeed = seed!;
      if (focused !== undefined) control.focusedAt = platform.now();
      browserId = typeof id === "string" && id ? id : platform.uuid();
      if (browserId !== id) await platform.local.set("browserId", browserId);
      control.paused = paused === true;
      control.stopped = stopped === true;
      settings = { ...settings, ...(saved ?? {}) };
      platform.setCursor(settings.cursor);
      options.language?.(settings.language);
      browser = info;
      await registry.load();
      await reconcile();
      refresh();
      bridge.connect();
    })();
    return started;
  }

  /**
   * Chrome clears session storage when the extension reloads (to pick up an
   * update), which would orphan every agent tab. The instance that reloads
   * hands its state over through storage.local; the next one takes it back,
   * only within a minute, so a browser restart never revives old tab ids.
   */
  async function reload() {
    await registry.flush();
    const handoff: Handoff = {
      at: platform.now(),
      registry: await platform.session.get("registry"),
      refSeed: Math.max(refSeed, lastRef()),
      stopped: control.stopped,
    };
    await platform.local.set("handoff", handoff);
    platform.reload();
  }

  async function takeHandoff() {
    const handoff = await platform.local.get<Handoff | null>("handoff");
    if (!handoff) return;
    await platform.local.set("handoff", null);
    if (platform.now() - handoff.at >= HANDOFF_MS) return;
    // Session storage that survived is newer than any hand-over.
    const kept = await Promise.all(["registry", "refSeed", "stopped"].map((name) => platform.session.get(name)));
    if (kept.some((value) => value !== undefined)) return;
    // The Stop and ref seed are handed over even when the registry could not be saved.
    if (handoff.registry) await platform.session.set("registry", handoff.registry);
    await platform.session.set("refSeed", handoff.refSeed);
    await platform.session.set("stopped", handoff.stopped);
  }

  /** After a worker restart: forget tabs that closed or left their group while it was stopped. */
  async function reconcile() {
    for (const entry of registry.all()) {
      for (const tab of [...entry.tabs]) {
        const live = await platform.tabs.get(tab.tabId);
        if (!live) registry.removeTab(entry, tab.id, true);
        else if (platform.groups && entry.groupId !== null && live.groupId !== entry.groupId)
          registry.removeTab(entry, tab.id, true);
        else {
          // It may have navigated while nothing watched it.
          if (live.url !== tab.url) tab.generation++;
          Object.assign(tab, { url: live.url, title: live.title, loading: live.loading });
        }
      }
    }
  }

  function hello(): ToBridge {
    return {
      type: "hello",
      protocol: PROTOCOL,
      browser: {
        id: browserId,
        kind: "chrome",
        name: browser.name.slice(0, 64),
        label: settings.label.trim() ? settings.label.trim().slice(0, 64) : null,
        version: browser.version.slice(0, 64),
      },
      extension: { version: platform.version().slice(0, 32) },
      ...control,
    };
  }

  // ---------------------------------------------------------- bridge input

  function onBridge(message: FromBridge) {
    switch (message.type) {
      case "bridge":
        bridgeInfo = { version: message.version, extension: message.extension };
        platform.setUploadsRoot(message.uploadsRoot);
        void checkUpdate();
        break;
      case "servers": {
        const next = new Map<string, Server>();
        for (const server of message.servers) next.set(server.id, { ...server, attached: servers.get(server.id)?.attached });
        for (const id of servers.keys()) if (!next.has(id)) activity.delete(id);
        servers.clear();
        for (const [id, server] of next) servers.set(id, server);
        break;
      }
      case "from-server": {
        const inner = message.message;
        if (inner.type === "attached") attached(message.server, inner.server.workspace, inner.server.id, inner.version);
        else if (inner.type === "command") void command(message.server, inner);
        else if (inner.type === "cancel") queues.cancel(inner.id);
        break;
      }
      case "activity":
        activity.set(message.server, {
          active: new Set(message.active),
          approvals: message.approvals.filter((approval) => approval.explicit),
        });
        break;
      case "thread":
        waiting.get(`thread:${message.server}:${message.thread}`)?.(message);
        break;
      case "stopped":
        waiting.get(`stopped:${message.server}:${message.thread}`)?.(message);
        break;
      case "answered":
        waiting.get(`answered:${message.server}:${message.permission}`)?.(message);
        break;
    }
    refresh();
  }

  /** Asks the bridge something and waits for its answer. */
  function ask<T extends FromBridge>(key: string, message: ToBridge): Promise<T | undefined> {
    return new Promise((resolve) => {
      if (!bridge.send(message)) return resolve(undefined);
      const timer = setTimeout(() => waiting.get(key)?.(undefined), REQUEST_TIMEOUT_MS);
      waiting.set(key, (reply) => {
        clearTimeout(timer);
        waiting.delete(key);
        resolve(reply as T | undefined);
      });
    });
  }

  function attached(serverId: string, workspace: string, instance: string, version: number) {
    if (version !== 1) return;
    const server = servers.get(serverId) ?? {
      id: serverId,
      workspace,
      name: basename(workspace),
      version: "",
      state: "connected" as const,
    };
    server.attached = { workspace, instance };
    servers.set(serverId, server);
    registry.attach(workspace, instance);
    // A new connection starts with no inventory: give it every chat this browser holds there.
    const held = registry
      .all()
      .filter((entry) => entry.workspace === workspace && (entry.tabs.length || entry.revoked.length))
      .sort((a, b) => b.touchedAt - a.touchedAt)
      .slice(0, 256);
    for (const entry of held) sendState(serverId, entry);
    for (const entry of held) if (entry.tabs.length) void refreshThread(serverId, entry);
  }

  /** Brings a chat's title (and so its group's title) up to date after a reconnect. */
  async function refreshThread(serverId: string, entry: ThreadRecord) {
    const reply = await ask<Extract<FromBridge, { type: "thread" }>>(`thread:${serverId}:${entry.thread}`, {
      type: "thread-info",
      server: serverId,
      thread: entry.thread,
    });
    if (!reply || typeof reply.title !== "string") return;
    entry.parent = typeof reply.parent === "string" ? reply.parent : null;
    rename(entry, reply.title);
  }

  function rename(entry: ThreadRecord, title: string) {
    if (!title || title === entry.title) return;
    entry.title = title;
    registry.save();
    if (platform.groups && entry.groupId !== null)
      void platform.groups.update(entry.groupId, { title: groupTitle(title) }).catch(() => {});
    refresh();
  }

  // ------------------------------------------------------------- commands

  async function command(serverId: string, message: ServerCommand) {
    const server = servers.get(serverId);
    const workspace = server?.attached?.workspace ?? server?.workspace;
    if (!workspace) {
      const outcome = failure(actionOf(message.command), new Error("Haarness for Chrome restarted. Call browser_tabs_list, then try again."));
      bridge.send({ type: "to-server", server: serverId, message: { type: "result", id: message.id, outcome: encode(message.command, outcome) } });
      return;
    }
    const entry = registry.ensure(workspace, message.thread);
    entry.parent = message.parent;
    rename(entry, message.title);
    const key = threadKey(workspace, message.thread);
    clearTimeout(idle.get(key));
    const outcome = await queues.run(key, message.id, (signal) => run(serverId, entry, message, signal));
    if (message.command.inspect !== true) sendState(serverId, entry);
    bridge.send({ type: "to-server", server: serverId, message: { type: "result", id: message.id, outcome: encode(message.command, outcome) } });
    idle.set(
      key,
      setTimeout(() => void release(entry), IDLE_DETACH_MS),
    );
    if (restartWhenIdle && !queues.pending()) {
      restartWhenIdle = false;
      bridge.restart();
    }
    if (update && !queues.pending()) void checkUpdate();
    refresh();
  }

  /** No command for this chat in 30 s: let go of its tabs so Chrome's debugging bar can go away. */
  async function release(entry: ThreadRecord) {
    if (queues.pending(threadKey(entry.workspace, entry.thread))) return;
    for (const tab of entry.tabs) {
      // The driver assumes a live debugger: the next command attaches again with a new page.
      pages.get(tab.tabId)?.dispose();
      pages.delete(tab.tabId);
      await platform.debugger.detach(tab.tabId).catch(() => {});
    }
  }

  async function run(serverId: string, entry: ThreadRecord, message: ServerCommand, signal: AbortSignal): Promise<Outcome> {
    const action = actionOf(message.command);
    try {
      if (control.stopped) throw new HostError("stopped-by-user");
      if (control.paused) throw new HostError("paused");
      if (signal.aborted) throw new Error(CANCELLED);
      const result = await execute(serverId, entry, message.id, decode(message.command), signal);
      return { type: "success", result };
    } catch (error) {
      if (control.stopped) return failure(action, new HostError("stopped-by-user"));
      if (control.paused) return failure(action, new HostError("paused"));
      return failure(action, error);
    }
  }

  async function execute(
    serverId: string,
    entry: ThreadRecord,
    id: string,
    command: HostCommand,
    signal: AbortSignal,
  ): Promise<HostResult> {
    const action = command.action;
    if (command.policy) entry.policy = command.policy;
    const context = command.policy && !command.inspect ? approve(entry, command) : undefined;
    const value = (value: unknown): HostResult => ({ value: value as HostResult["value"], files: [] });

    if (action.type === "tabs.list")
      return value(command.inspect ? { resources: [], key: "inventory" } : stateOf(entry));
    if (action.type === "tabs.open") {
      const url = normalizeURL(typeof action.url === "string" ? action.url : "about:blank");
      if (command.inspect) return value({ resources: openResources(url), key: "open" });
      return value(await open(serverId, id, entry, url, action.focus === true, context, signal));
    }
    const record = entry.tabs.find((tab) => tab.id === action.tabID);
    if (!record) throw new Error(NOT_THIS_CHAT);
    if (action.type === "tabs.focus" || action.type === "tabs.close") {
      const tab = tabOf(record);
      const target = { resources: [tab.url], key: `${tab.id}:${tab.generation}` };
      if (command.inspect) return value(target);
      if (command.target && JSON.stringify(command.target) !== JSON.stringify(target)) throw new Error(TARGET_CHANGED);
      if (action.type === "tabs.close") {
        await close(entry, [record], false);
        return value(stateOf(entry));
      }
      await owned(entry, record);
      await platform.tabs.activate(record.tabId);
      entry.focusedTabID = record.id;
      registry.save();
      return value(tabOf(record));
    }
    return page(serverId, id, command, entry, record, context, signal);
  }

  /** Remembers the origins this command was approved for, and returns the rules to enforce. */
  function approve(entry: ThreadRecord, command: HostCommand): FetchContext {
    const resources =
      command.target?.resources ??
      (command.action.type === "tabs.open"
        ? openResources(normalizeURL(typeof command.action.url === "string" ? command.action.url : "about:blank"))
        : []);
    registry.approve(
      entry,
      resources.map(destinationOrigin).filter((origin): origin is string => !!origin),
    );
    return { policy: command.policy!, approved: new Set(entry.approved) };
  }

  function openResources(url: string) {
    const origin = destinationOrigin(url);
    const twin = origin && wwwTwin(origin);
    return twin ? [url, twin] : [url];
  }

  async function open(
    serverId: string,
    id: string,
    entry: ThreadRecord,
    url: string,
    focus: boolean,
    context: FetchContext | undefined,
    signal: AbortSignal,
  ): Promise<Tab> {
    if (entry.tabs.length >= MAX_TABS) throw new Error(OPEN_LIMIT);
    const live = await platform.tabs.create({ url: "about:blank", active: focus });
    const record: TabRecord = {
      id: tabID(platform.uuid()),
      tabId: live.id,
      generation: 0,
      url: "about:blank",
      title: "",
      loading: false,
      canGoBack: false,
      canGoForward: false,
    };
    registry.addTab(entry, record);
    if (focus) entry.focusedTabID = record.id;
    await join(entry, [live.id]).catch(() => {});
    publish(entry);
    if (url !== "about:blank") {
      const session = await platform.debugger.session(live.id, record.id);
      // As on Desktop, the page exists before its first load: it sees every
      // frame the document attaches, and its console and requests, from the start.
      await pageFor(entry, record, session).ready;
      session.begin({ server: serverId, thread: entry.thread, command: id });
      try {
        if (context) await session.guard(context, signal, () => session.load(url, signal));
        else await session.load(url, signal);
      } finally {
        await session.end();
      }
    }
    await sync(record);
    return tabOf(record);
  }

  async function page(
    serverId: string,
    id: string,
    command: HostCommand,
    entry: ThreadRecord,
    record: TabRecord,
    context: FetchContext | undefined,
    signal: AbortSignal,
  ): Promise<HostResult> {
    const method = command.action.type;
    if (!supportedInChrome(method)) throw new HostError("unsupported");
    const live = await owned(entry, record);
    if (!/^https?:/i.test(live.url) && !allowedOnBlank(method)) throw new HostError("unsupported");
    const session = await platform.debugger.session(record.tabId, record.id);
    const current = pageFor(entry, record, session);
    await current.ready;
    try {
      if (command.inspect) return await current.execute(command, signal);
      session.begin({ server: serverId, thread: entry.thread, command: id });
      try {
        const work = () => current.execute(command, signal);
        return context ? await session.guard(context, signal, work) : await work();
      } finally {
        await session.end();
      }
    } finally {
      saveRefSeed();
    }
  }

  function pageFor(entry: ThreadRecord, record: TabRecord, session: TabSession): Page {
    const existing = pages.get(record.tabId);
    if (existing) return existing;
    const created: Page = session.page(createPage, {
      id: record.id,
      // The page goes on from the generation the server last saw for this tab.
      generation: record.generation,
      refSeed,
      translate: options.translate,
      // In the user's own browser requests carry the user's credentials.
      diagnostics: { requestBodies: false, redactHeader: isSensitiveHeader },
      publish() {
        if (pages.get(record.tabId) !== created) return;
        const state = created.state();
        Object.assign(record, { url: state.url, title: state.title, loading: state.loading, generation: state.generation });
        registry.save();
        publish(entry);
      },
    });
    pages.set(record.tabId, created);
    return created;
  }

  function saveRefSeed() {
    const last = lastRef();
    if (last <= refSeed) return;
    refSeed = last;
    void platform.session.set("refSeed", refSeed);
  }

  /** The live tab, if it is still this chat's and a page Haarness may control. */
  async function owned(entry: ThreadRecord, record: TabRecord): Promise<LiveTab> {
    const live = await platform.tabs.get(record.tabId);
    if (!live) {
      forget(entry, record, true);
      throw new Error(NOT_THIS_CHAT);
    }
    if (platform.groups && entry.groupId !== null && live.groupId !== entry.groupId) {
      forget(entry, record, true);
      throw new HostError("not-in-group");
    }
    if (restricted(live.url)) throw new HostError("restricted-page");
    return live;
  }

  function forget(entry: ThreadRecord, record: TabRecord, revoke: boolean) {
    registry.removeTab(entry, record.id, revoke);
    pages.get(record.tabId)?.dispose();
    pages.delete(record.tabId);
    void platform.debugger.detach(record.tabId).catch(() => {});
    publish(entry);
  }

  async function close(entry: ThreadRecord, records: TabRecord[], revoke: boolean) {
    for (const record of records) {
      closing.add(record.tabId);
      forget(entry, record, revoke);
    }
    await platform.tabs.remove(records.map((record) => record.tabId)).catch(() => {});
  }

  async function sync(record: TabRecord) {
    const live = await platform.tabs.get(record.tabId);
    if (live && !pages.has(record.tabId)) Object.assign(record, { url: live.url, title: live.title, loading: live.loading });
  }

  /** Puts tabs into the chat's group, creating it (named and coloured for the chat) when needed. */
  async function join(entry: ThreadRecord, tabIds: number[]) {
    const groups = platform.groups;
    if (!groups) return;
    if (entry.groupId !== null && (await groups.exists(entry.groupId))) {
      await groups.add(tabIds, entry.groupId);
      return;
    }
    const groupId = await groups.add(tabIds);
    entry.groupId = groupId;
    registry.save();
    await groups.update(groupId, { title: groupTitle(entry.title), color: groupColor(entry.thread), collapsed: false });
  }

  // ------------------------------------------------------------ publishing

  function tabOf(record: TabRecord): Tab {
    const current = pages.get(record.tabId);
    if (!current) return wireTab(record);
    return wireTab({ ...record, ...current.state(), id: record.id, tabId: record.tabId });
  }

  function stateOf(entry: ThreadRecord): State {
    return wireState(entry, tabOf);
  }

  function sendState(serverId: string, entry: ThreadRecord) {
    publishing.delete(threadKey(entry.workspace, entry.thread));
    bridge.send({
      type: "to-server",
      server: serverId,
      message: {
        type: "state",
        thread: entry.thread,
        state: stateOf(entry),
        ...(entry.revoked.length ? { revoked: entry.revoked } : {}),
      },
    });
  }

  /** Sends the chat's inventory to every server attached for its project, once per turn of the event loop. */
  function publish(entry: ThreadRecord) {
    const key = threadKey(entry.workspace, entry.thread);
    refresh();
    if (publishing.has(key)) return;
    publishing.add(key);
    queueMicrotask(() => {
      if (!publishing.delete(key)) return;
      for (const server of servers.values())
        if (server.attached?.workspace === entry.workspace) sendState(server.id, entry);
    });
  }

  // ---------------------------------------------------------- browser events

  const events: PlatformEvents = {
    tabCreated(tab) {
      void after(() => adopt(tab));
    },
    tabUpdated(tabId, change, tab) {
      void after(() => updated(tabId, change, tab));
    },
    tabRemoved(tabId) {
      void after(() => {
        if (closing.delete(tabId)) return;
        const owner = registry.owner(tabId);
        if (owner) forget(owner.entry, owner.tab, true);
      });
    },
    tabReplaced(added, removed) {
      void after(() => {
        pages.get(removed)?.dispose();
        pages.delete(removed);
        registry.replaceTab(removed, added);
      });
    },
    windowFocused(windowId) {
      if (windowId === undefined) return;
      control.focusedAt = platform.now();
      // Focus changes come in bursts; one status per second is plenty.
      statusTimer ??= setTimeout(() => {
        statusTimer = undefined;
        sendStatus();
      }, 1_000);
    },
    debuggerDetached(tabId, reason) {
      pages.get(tabId)?.dispose();
      pages.delete(tabId);
      if (reason === "canceled_by_user") void after(() => setStopped(true));
    },
    downloadFinished(download) {
      bridge.send({ type: "download", ...download });
    },
    stopAll() {
      void after(stopAll);
    },
    alarm(name) {
      if (name === RECONNECT_ALARM) void after(() => bridge.retry());
    },
  };

  /**
   * Runs an event's work once start() has loaded the stored state. The event
   * that woke a stopped worker arrives first: a hello sent before then would
   * carry no browser id, and a Stop recorded then would be overwritten.
   */
  async function after(work: () => void | Promise<void>) {
    await start();
    await work();
  }

  /**
   * A tab opened from an agent tab (a popup, a link in a new tab) belongs to
   * the same chat. Chrome puts such a tab in the group of the tab that opened
   * it, so a tab created inside a chat's group is that chat's. `openerTabId`
   * is only a fallback (no tab groups): when the opening page is in the
   * background, as agent tabs usually are, Chrome names the window's active
   * tab instead.
   */
  async function adopt(tab: LiveTab) {
    if (registry.owner(tab.id)) return;
    const grouped = platform.groups && tab.groupId >= 0 ? registry.byGroup(tab.groupId) : undefined;
    const entry = grouped ?? (tab.openerTabId === undefined ? undefined : registry.owner(tab.openerTabId)?.entry);
    if (!entry) return;
    // A browser page (the user's "New tab in group") is never the agent's.
    if (restricted(tab.url)) {
      if (grouped) await share(tab, tab.groupId);
      return;
    }
    const origin = destinationOrigin(tab.url);
    if (origin && entry.policy && (!entry.policy.enabled || siteAccess(entry.policy, origin).browsing === "deny")) {
      closing.add(tab.id);
      await platform.tabs.remove([tab.id]).catch(() => {});
      return;
    }
    if (entry.tabs.length >= MAX_TABS) {
      if (platform.groups && tab.groupId >= 0) await platform.groups.ungroup([tab.id]).catch(() => {});
      return;
    }
    const record: TabRecord = {
      id: tabID(platform.uuid()),
      tabId: tab.id,
      generation: 0,
      url: tab.url || "about:blank",
      title: tab.title,
      loading: tab.loading,
      canGoBack: false,
      canGoForward: false,
    };
    registry.addTab(entry, record);
    await join(entry, [tab.id]).catch(() => {});
    if (entry.tabs.some((other) => other.tabId !== tab.id && platform.debugger.attached(other.tabId)))
      await platform.debugger.session(tab.id, record.id).catch(() => undefined);
    publish(entry);
  }

  async function updated(tabId: number, change: TabChange, tab: LiveTab) {
    const owner = registry.owner(tabId);
    if (!owner) {
      if (change.groupId !== undefined && change.groupId >= 0) await share(tab, change.groupId);
      return;
    }
    const { entry, tab: record } = owner;
    if (change.groupId !== undefined && platform.groups && change.groupId !== entry.groupId) {
      if (await followGroup(entry, change.groupId)) return;
      forget(entry, record, true);
      if (change.groupId >= 0) await share(tab, change.groupId);
      return;
    }
    if (change.url !== undefined && change.url !== record.url) {
      // Without a page watching navigations, any new URL may be a new document.
      // (A new tab first reports the about:blank it was created with: not a navigation.)
      if (!pages.has(tabId)) record.generation++;
      record.url = change.url;
    }
    if (change.title !== undefined) record.title = change.title;
    if (change.status !== undefined) record.loading = change.status === "loading";
    registry.save();
    publish(entry);
  }

  /**
   * The chat's tab moved to another group. When that group holds only this
   * chat's tabs (the user dragged the whole group to another window, or Chrome
   * recreated it), the chat follows it instead of losing its tabs.
   */
  async function followGroup(entry: ThreadRecord, groupId: number) {
    const groups = platform.groups;
    if (!groups || groupId < 0) return false;
    if (entry.groupId !== null && (await groups.exists(entry.groupId))) {
      const remaining = await groups.tabs(entry.groupId);
      if (remaining.some((tab) => registry.owner(tab.id)?.entry === entry)) return false;
    }
    const members = await groups.tabs(groupId);
    if (!members.every((tab) => registry.owner(tab.id)?.entry === entry)) return false;
    entry.groupId = groupId;
    registry.save();
    return true;
  }

  /** The user dragged a tab into a chat's group: share it, if Haarness may use it. */
  async function share(tab: LiveTab, groupId: number) {
    const entry = registry.byGroup(groupId);
    if (!entry || registry.owner(tab.id)) return;
    const refusal = !/^https?:\/\//i.test(tab.url) ? "restricted" : entry.tabs.length >= MAX_TABS ? "full" : null;
    entry.refused = refusal;
    entry.refusedAt = platform.now();
    if (refusal) {
      await platform.groups?.ungroup([tab.id]).catch(() => {});
      registry.save();
      refresh();
      return;
    }
    registry.addTab(entry, {
      id: tabID(platform.uuid()),
      tabId: tab.id,
      generation: 0,
      url: tab.url,
      title: tab.title,
      loading: tab.loading,
      canGoBack: false,
      canGoForward: false,
    });
    publish(entry);
  }

  // ---------------------------------------------------------- user control

  function sendStatus() {
    bridge.send({ type: "status", ...control });
  }

  async function setStopped(stopped: boolean) {
    control.stopped = stopped;
    await platform.session.set("stopped", stopped);
    if (stopped) await letGo();
    sendStatus();
    refresh();
  }

  async function setPaused(paused: boolean) {
    control.paused = paused;
    await platform.local.set("paused", paused);
    if (paused) await letGo();
    sendStatus();
    refresh();
  }

  /** Cancels every command and detaches from every tab. */
  async function letGo() {
    queues.cancelAll();
    for (const current of pages.values()) current.dispose();
    pages.clear();
    await platform.debugger.detachAll().catch(() => {});
  }

  async function stopAll() {
    await setStopped(true);
    await Promise.all(chats().filter((chat) => chat.working).map((chat) => stopChat(chat.entry)));
  }

  async function stopChat(entry: ThreadRecord): Promise<UiResponse> {
    const server = serverFor(entry.workspace);
    if (!server) return { ok: false, error: "This project is not connected." };
    const reply = await ask<Extract<FromBridge, { type: "stopped" }>>(`stopped:${server.id}:${entry.thread}`, {
      type: "stop",
      server: server.id,
      thread: entry.thread,
    });
    return reply?.ok ? { ok: true } : { ok: false, error: reply?.error ?? "Haarness did not answer." };
  }

  /**
   * Setup copies the extension that matches the installed Haarness to disk.
   * When that copy differs from what runs, reload once (Chrome reads the
   * folder again); if Chrome loads the extension from elsewhere, a reload
   * changes nothing, so a newer copy is offered as "Update ready" instead.
   */
  async function checkUpdate() {
    const disk = bridgeInfo?.extension?.version;
    const running = platform.version();
    update = disk && compareVersions(disk, running) > 0 ? disk : null;
    if (disk && disk !== running && !queues.pending()) {
      const tried = await platform.local.get<string>("reloadedFor");
      if (tried !== disk) {
        await platform.local.set("reloadedFor", disk);
        await reload();
        return;
      }
    }
    refresh();
  }

  // -------------------------------------------------------------- the UI

  function serverFor(workspace: string) {
    for (const server of servers.values()) if (server.attached?.workspace === workspace) return server;
    for (const server of servers.values()) if (server.workspace === workspace && server.state === "connected") return server;
  }

  function projectName(workspace: string) {
    for (const server of servers.values()) if (server.workspace === workspace && server.name) return server.name;
    return basename(workspace);
  }

  function working(entry: ThreadRecord) {
    for (const [id, current] of activity) {
      const server = servers.get(id);
      if (server?.attached?.workspace === entry.workspace && current.active.has(entry.thread)) return true;
    }
    return false;
  }

  /** Chats holding tabs here: working ones first, then the most recently used. */
  function chats() {
    return registry
      .all()
      .filter((entry) => entry.tabs.length > 0)
      .map((entry) => ({ entry, working: working(entry) }))
      .sort((a, b) => Number(b.working) - Number(a.working) || b.entry.touchedAt - a.entry.touchedAt);
  }

  function snapshot(): UiSnapshot {
    const connection: Connection = bridge.connection();
    const list = chats();
    const approvals = [...activity].flatMap(([serverId, current]) => {
      const server = servers.get(serverId);
      const workspace = server?.attached?.workspace ?? server?.workspace ?? "";
      return current.approvals
        .map((approval) => ({
          server: serverId,
          id: approval.id,
          title: registry.get(threadKey(workspace, approval.thread))?.title ?? "",
          project: projectName(workspace),
          lines: approvalLines(approval.patterns),
        }))
        .filter((approval) => approval.lines.length);
    });
    return {
      connection,
      pill: pill({ connection, ...control, update: !!update }),
      failures: bridge.failures(),
      paused: control.paused,
      stopped: control.stopped,
      update,
      projects: [...servers.values()].map((server) => ({
        id: server.id,
        name: server.name || basename(server.workspace),
        workspace: server.workspace,
        version: server.version,
        state: server.state,
        error: server.error,
      })),
      chats: list.map(({ entry, working }) => ({
        key: threadKey(entry.workspace, entry.thread),
        server: serverFor(entry.workspace)?.id ?? null,
        project: projectName(entry.workspace),
        title: entry.title,
        tabs: entry.tabs.length,
        working,
        subagent: entry.parent !== null,
        // The note explains a tab that just bounced out of the group; it fades after two minutes.
        refused: entry.refused && platform.now() - (entry.refusedAt ?? 0) < REFUSAL_NOTE_MS ? entry.refused : null,
      })),
      approvals,
      settings,
      versions: { extension: platform.version(), haarness: bridgeInfo?.version ?? null, browser: browser.version },
      browserName: browser.name,
      extensionPath: bridgeInfo?.extension?.path ?? null,
      shortcut,
    };
  }

  /** Badge, title and open pages follow every change, at most once per turn of the event loop. */
  function refresh() {
    if (notifyTimer) return;
    notifyTimer = setTimeout(() => {
      notifyTimer = undefined;
      const current = snapshot();
      const waitingCount = current.approvals.length;
      const workingCount = current.chats.filter((chat) => chat.working).length;
      platform.badge(
        control.stopped || control.paused ? "" : badgeText(workingCount, waitingCount),
        waitingCount ? "waiting" : workingCount ? "working" : "idle",
      );
      platform.title(titleFor(current, options.translate));
      for (const watcher of watchers) watcher(current);
    }, 0);
  }

  async function request(message: UiRequest): Promise<UiResponse> {
    await start();
    const entry = "key" in message ? registry.get(message.key) : undefined;
    switch (message.type) {
      case "answer": {
        const reply = await ask<Extract<FromBridge, { type: "answered" }>>(
          `answered:${message.server}:${message.permission}`,
          { type: "answer", server: message.server, permission: message.permission, decision: message.decision },
        );
        return reply?.ok ? { ok: true } : { ok: false, error: reply?.error ?? "Haarness did not answer." };
      }
      case "resume":
        await setStopped(false);
        return { ok: true };
      case "pause":
        await setPaused(message.paused);
        return { ok: true };
      case "stop-chat":
        return entry ? stopChat(entry) : { ok: false, error: "This chat has no tabs here." };
      case "show-tabs": {
        const record = entry?.tabs.find((tab) => tab.id === entry.focusedTabID) ?? entry?.tabs[0];
        const live = record && (await platform.tabs.get(record.tabId));
        if (!entry || !live) return { ok: false, error: "This chat has no tabs here." };
        if (platform.groups && entry.groupId !== null)
          await platform.groups.update(entry.groupId, { collapsed: false }).catch(() => {});
        await platform.tabs.activate(live.id);
        await platform.tabs.raiseWindow(live.windowId);
        return { ok: true };
      }
      case "close-tabs":
        if (entry) await close(entry, [...entry.tabs], true);
        return { ok: true };
      case "close-all":
        for (const current of registry.all()) if (current.tabs.length) await close(current, [...current.tabs], true);
        return { ok: true };
      case "reload":
        await reload();
        return { ok: true };
      case "retry":
        bridge.retry();
        return { ok: true };
      case "settings": {
        const before = settings;
        settings = { ...settings, ...message.settings, label: (message.settings.label ?? settings.label).trim().slice(0, 64) };
        await platform.local.set("settings", settings);
        platform.setCursor(settings.cursor);
        if (before.language !== settings.language) options.language?.(settings.language);
        // The label travels in `hello`; reconnect once nothing is running so servers see it.
        if (before.label !== settings.label) {
          if (queues.pending()) restartWhenIdle = true;
          else bridge.restart();
        }
        refresh();
        return { ok: true };
      }
    }
  }

  return {
    start,
    events,
    request,
    /** Pages subscribe here; the current snapshot arrives as soon as the host has started. */
    watch(watcher: (snapshot: UiSnapshot) => void) {
      let open = true;
      let counted = false;
      void start().then(() => {
        if (!open) return;
        watchers.add(watcher);
        counted = true;
        bridge.watch(1);
        watcher(snapshot());
      });
      return () => {
        open = false;
        watchers.delete(watcher);
        if (counted) bridge.watch(-1);
      };
    },
    setShortcut(value: string | null) {
      shortcut = value;
      refresh();
    },
    snapshot,
    /** Stops timers and the bridge (tests; a worker that is going away). */
    dispose() {
      for (const timer of idle.values()) clearTimeout(timer);
      idle.clear();
      clearTimeout(notifyTimer);
      clearTimeout(statusTimer);
      for (const resolve of [...waiting.values()]) resolve(undefined);
      bridge.close();
    },
    /** For tests and the e2e harness. */
    debug: { registry, control, pages, servers, queues, settings: () => settings },
  };
}

export type Host = ReturnType<typeof createHost>;

/** The action a raw command names, for a failure outcome before (or without) decoding it. */
function actionOf(command: RawCommand) {
  return { type: command.action.type } as Pick<Action, "type">;
}

/**
 * Decodes a server command with the driver's codec. A tool this extension
 * does not know fails `unsupported` (a newer Haarness); anything else
 * malformed fails plainly.
 */
function decode(command: RawCommand): HostCommand {
  if (!(methods as readonly string[]).includes(command.action.type)) throw new HostError("unsupported");
  try {
    return decodeHostCommand(command);
  } catch (error) {
    const detail = (error instanceof Error ? error.message : String(error)).slice(0, 600);
    throw new Error(`Haarness for Chrome could not read this command. Check the tool's arguments. Details: ${detail}`);
  }
}

/** The outcome as JSON for the bridge; one that cannot be encoded becomes a failure. */
function encode(command: RawCommand, outcome: Outcome): unknown {
  try {
    return encodeHostOutcome(outcome);
  } catch (error) {
    return encodeHostOutcome(failure(actionOf(command), new Error(`Haarness for Chrome could not send this result. ${String(error).slice(0, 400)}`)));
  }
}

function basename(path: string) {
  return path.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || path;
}

/** The toolbar button's tooltip: the brand, then whatever needs the user. */
export function titleFor(snapshot: UiSnapshot, translate: (key: string) => string = (key) => key) {
  const states: Partial<Record<UiSnapshot["pill"], string>> = {
    "not-set-up": key("Not set up"),
    paused: key("Paused"),
    stopped: key("Stopped"),
    connecting: key("Connecting…"),
    mismatch: key("Update needed"),
  };
  const state = snapshot.approvals.length ? key("Waiting for you") : states[snapshot.pill];
  return state ? `Haarness · ${translate(state)}` : "Haarness";
}
