/**
 * The service worker's host logic against an in-memory Chrome (tests/fakes):
 * the bridge protocol, the per-chat registry and groups, the user's controls,
 * and the page-driver seam.
 */
import assert from "node:assert/strict";
import test, { afterEach } from "node:test";
import { createHost } from "../src/core/host";
import type { CreatePage, PageOptions, PageSurface } from "../src/core/driver-contract";
import { createChromePlatform } from "../src/platform/chrome/index";
import { installChrome, type FakeChrome } from "./fakes/chrome";

const THREAD = "0192a5f0-7c1e-7d3a-9b1e-4f2a9c3d5e61";
const SUBAGENT = "0192a5f0-7c1e-7d3a-9b1e-4f2a9c3d5e62";
const WORKSPACE = "/work/acme";
const ask = { enabled: true, defaults: { browsing: "ask", downloads: "ask", uploads: "ask" }, sites: [] as object[] };

const hosts: { dispose(): void }[] = [];
afterEach(() => {
  for (const host of hosts.splice(0)) host.dispose();
});

async function boot(options: { prepare?(fake: FakeChrome): void; createPage?: CreatePage; reuse?: FakeChrome; idleMs?: number } = {}) {
  const fake = options.reuse ?? installChrome();
  if (options.reuse) (globalThis as unknown as { chrome: unknown }).chrome = fake.chrome;
  options.prepare?.(fake);
  const { platform, listen } = createChromePlatform();
  const host = createHost({ platform, createPage: options.createPage, idleMs: options.idleMs });
  hosts.push(host);
  listen(host.events);
  await host.start();
  return new Harness(fake, host);
}

type Message = Record<string, any>;

class Harness {
  private sequence = 0;
  constructor(
    readonly fake: FakeChrome,
    readonly host: ReturnType<typeof createHost>,
  ) {}
  get sent(): Message[] {
    return this.fake.port.sent;
  }
  async until<T = Message>(match: (message: Message) => boolean, from = 0, timeout = 2_000): Promise<T> {
    const deadline = Date.now() + timeout;
    while (Date.now() < deadline) {
      const found = this.sent.slice(from).find(match);
      if (found) return found as T;
      await new Promise((resolve) => setTimeout(resolve, 2));
    }
    throw new Error(`Nothing matched. Sent: ${JSON.stringify(this.sent.slice(-6))}`);
  }
  start(server = "server-1") {
    this.fake.port.deliver({ type: "bridge", protocol: 1, version: "0.1.1-preview.1", uploadsRoot: "/tmp/up", extension: null });
    this.fake.port.deliver({ type: "servers", servers: [{ id: server, workspace: WORKSPACE, name: "acme", version: "0.1.1", state: "connected" }] });
  }
  attach(server = "server-1", instance = "instance-1") {
    this.fake.port.deliver({
      type: "from-server",
      server,
      message: { type: "attached", version: 1, lease: "l", server: { id: instance, workspace: WORKSPACE, pid: 1 } },
    });
  }
  async command(command: Message, options: { thread?: string; title?: string; parent?: string | null; server?: string } = {}) {
    const id = `c${++this.sequence}`;
    const from = this.sent.length;
    this.fake.port.deliver({
      type: "from-server",
      server: options.server ?? "server-1",
      message: {
        type: "command",
        id,
        thread: options.thread ?? THREAD,
        title: options.title ?? "Plan the trip",
        parent: options.parent ?? null,
        command: { files: [], ...command },
      },
    });
    const result = await this.until((message) => message.type === "to-server" && message.message.type === "result" && message.message.id === id, from);
    const index = this.sent.indexOf(result);
    return { outcome: result.message.outcome as Message, before: this.sent.slice(from, index) };
  }
  async open(url = "about:blank", options: Parameters<Harness["command"]>[1] & { focus?: boolean } = {}) {
    const { outcome } = await this.command(
      { action: { type: "tabs.open", url, focus: options.focus }, policy: ask, target: { resources: [url], key: "open" } },
      options,
    );
    assert.equal(outcome.type, "success", JSON.stringify(outcome));
    const tab = outcome.result.value as { id: string; url: string };
    const live = [...this.fake.tabsById.values()].at(-1)!;
    return { tab, live };
  }
  states(thread = THREAD) {
    return this.sent.filter((message) => message.type === "to-server" && message.message.type === "state" && message.message.thread === thread);
  }
}

test("hello names this browser once and keeps its id across restarts", async () => {
  const first = await boot();
  const hello = first.sent[0];
  assert.equal(hello.type, "hello");
  assert.equal(hello.protocol, 1);
  assert.equal(hello.browser.kind, "chrome");
  assert.match(hello.browser.id, /^[0-9a-f-]{36}$/);
  assert.deepEqual(hello.extension, { version: "0.1.1-preview.1" });
  assert.equal(hello.paused, false);
  first.host.dispose();
  const second = await boot({ reuse: first.fake });
  assert.equal(second.sent[0].browser.id, hello.browser.id);
});

test("a worker woken by the reconnect alarm or a Stop waits for its stored state before saying hello", async () => {
  const fake = installChrome();
  const id = "5b0f2a3c-1d4e-4f6a-8b9c-0d1e2f3a4b5c";
  fake.stores.local.set("browserId", id);
  // Chrome reads storage when asked and answers a little later.
  const session = fake.chrome.storage.session;
  const read = session.get;
  session.get = (key: string) => {
    const value = read(key);
    return new Promise((resolve) => setTimeout(() => resolve(value), 5));
  };
  const { platform, listen } = createChromePlatform();
  const host = createHost({ platform });
  hosts.push(host);
  listen(host.events);
  // As in background.ts: start() is under way when the event that woke the worker arrives.
  const starting = host.start();
  fake.chrome.alarms.onAlarm.emit({ name: "reconnect" });
  // The user presses Stop all while start() is still reading the stored state.
  await new Promise((resolve) => setTimeout(resolve, 1));
  fake.chrome.commands.onCommand.emit("stop-all");
  await starting;
  await new Promise((resolve) => setTimeout(resolve, 5));
  assert.equal(fake.ports.length, 1, "one bridge");
  const hello = fake.port.sent[0];
  assert.equal(hello.type, "hello");
  assert.equal(hello.browser.id, id);
  // The Stop pressed while the worker started is not undone by the stored state.
  assert.equal(host.snapshot().stopped, true);
  assert.equal(fake.stores.session.get("stopped"), true);
});

test("the extension's own reload keeps the chats' tabs, but a stale hand-over is ignored", async () => {
  const first = await boot();
  first.start();
  first.attach();
  const { tab } = await first.open("about:blank");
  assert.deepEqual(await first.host.request({ type: "reload" }), { ok: true });
  assert.equal(first.fake.reloads, 1);
  first.host.dispose();
  // Chrome clears session storage when the extension reloads.
  first.fake.stores.session.clear();
  const second = await boot({ reuse: first.fake });
  assert.deepEqual(
    second.host.debug.registry.all().flatMap((entry) => entry.tabs.map((record) => record.id)),
    [tab.id],
  );
  second.start();
  second.attach();
  // The next server connection gets the chat's inventory again.
  await second.until((message) => message.type === "to-server" && message.message.state?.tabs[0]?.id === tab.id);
  assert.equal(first.fake.stores.local.get("handoff"), null, "a hand-over is used once");

  // A hand-over older than a minute (the browser restarted meanwhile) is not taken.
  await second.host.request({ type: "reload" });
  second.host.dispose();
  first.fake.stores.session.clear();
  const handoff = first.fake.stores.local.get("handoff") as { at: number };
  first.fake.stores.local.set("handoff", { ...handoff, at: handoff.at - 61_000 });
  const third = await boot({ reuse: first.fake });
  assert.deepEqual(third.host.debug.registry.all(), []);
});

test("a Stop outlasts the extension's own reload even when the tab registry could not be saved", async () => {
  const first = await boot();
  first.start();
  first.host.events.stopAll();
  await first.until((message) => message.type === "status" && message.stopped === true);
  // Session storage refuses the registry (its quota is full)…
  const session = first.fake.chrome.storage.session;
  const write = session.set;
  session.set = async (values: Record<string, unknown>) => {
    if ("registry" in values) throw new Error("QUOTA_BYTES quota exceeded");
    return write(values);
  };
  // …when an update arrives: the reload still hands the Stop over.
  assert.deepEqual(await first.host.request({ type: "reload" }), { ok: true });
  session.set = write;
  first.host.dispose();
  first.fake.stores.session.clear();
  const second = await boot({ reuse: first.fake });
  assert.equal(second.host.snapshot().stopped, true);
  assert.equal(second.sent.at(-1)!.stopped, true, "the new bridge hears stopped in hello");
  second.start();
  assert.equal((await second.command({ action: { type: "tabs.list" } })).outcome.code, "stopped-by-user");
});

test("new tabs open in the background, in one group per chat named after it; a subagent gets its own", async () => {
  const h = await boot();
  h.start();
  h.attach();
  const { tab, live } = await h.open("about:blank");
  assert.match(tab.id, /^tab_[0-9a-f-]{36}$/);
  assert.equal(live.active, false);
  const group = h.fake.groupsById.get(live.groupId)!;
  assert.equal(group.title, "Plan the trip");
  const second = await h.open("about:blank");
  assert.equal(second.live.groupId, live.groupId);

  const sub = await h.open("about:blank", { thread: SUBAGENT, title: "Check prices", parent: THREAD });
  assert.notEqual(sub.live.groupId, live.groupId);
  assert.equal(h.fake.groupsById.get(sub.live.groupId)!.title, "Check prices");

  // The chat's title changes: so does its group's.
  await h.command({ action: { type: "tabs.list" } }, { title: "Plan the Lisbon trip" });
  assert.equal(h.fake.groupsById.get(live.groupId)!.title, "Plan the Lisbon trip");

  // focus brings the tab forward while the user is not using Chrome…
  h.fake.chromeFocused = false;
  const focused = await h.open("about:blank", { focus: true });
  assert.equal(focused.live.active, true);
  assert.equal(h.fake.focused, 1, "never raises a window");
  // …but never over the user's tab in the window they are using.
  h.fake.chromeFocused = true;
  const mine = h.fake.createTab({ url: "https://mail.example/", windowId: 1, active: true });
  const behind = await h.open("about:blank", { focus: true });
  assert.equal(behind.live.active, false);
  assert.equal(h.fake.tabsById.get(mine.id)!.active, true);
  assert.match((behind.tab as { note?: string }).note ?? "", /opened in the background/);
  assert.equal(h.states().at(-1)!.message.state.focusedTabID, behind.tab.id, "it is still the chat's focused tab");
});

test("state always reaches the server before the result, failures included", async () => {
  const h = await boot();
  h.start();
  h.attach();
  const opened = await h.command({ action: { type: "tabs.open" }, policy: ask });
  assert.equal(opened.before.at(-1)!.message.type, "state");
  assert.equal(opened.before.at(-1)!.message.state.tabs.length, 1);
  const failed = await h.command({ action: { type: "tabs.focus", tabID: "tab_00000000-0000-0000-0000-000000000000" } });
  assert.equal(failed.outcome.type, "failure");
  assert.equal(failed.before.at(-1)!.message.type, "state");
  const inspect = await h.command({ action: { type: "tabs.list" }, inspect: true });
  assert.deepEqual(inspect.outcome, { type: "success", result: { value: { resources: [], key: "inventory" }, files: [] } });
});

test("tabs.open asks once for both spellings of a site", async () => {
  const h = await boot();
  h.start();
  h.attach();
  const { outcome } = await h.command({ action: { type: "tabs.open", url: "example.com/docs" }, inspect: true });
  assert.deepEqual(outcome.result.value, { resources: ["https://example.com/docs", "https://www.example.com"], key: "open" });
});

test("the registry is keyed by project, so a restarted server gets this browser's tabs back", async () => {
  const h = await boot();
  h.start();
  h.attach();
  const { tab } = await h.open("https://a.example/");
  const entry = h.host.debug.registry.all()[0];
  assert.deepEqual(entry.approved, ["https://a.example"]);

  // The bridge restarts and finds the project's new server.
  h.fake.port.close("Native host has exited.");
  h.host.events.alarm("reconnect");
  await h.until((message) => message.type === "hello");
  h.start("server-2");
  h.attach("server-2", "instance-2");
  const state = await h.until((message) => message.type === "to-server" && message.message.type === "state");
  assert.equal(state.server, "server-2");
  assert.equal(state.message.state.tabs[0].id, tab.id);
  // A new server instance forgot this chat's approvals; so does the extension.
  assert.deepEqual(h.host.debug.registry.all()[0].approved, []);
  const info = await h.until((message) => message.type === "thread-info");
  assert.deepEqual(info, { type: "thread-info", server: "server-2", thread: THREAD });
  h.fake.port.deliver({ type: "thread", server: "server-2", thread: THREAD, title: "Renamed", archived: false, parent: null });
  await new Promise((resolve) => setTimeout(resolve, 10));
  assert.equal([...h.fake.groupsById.values()][0].title, "Renamed");
});

test("dragging a tab out of the group, or closing it, gives it back to the user", async () => {
  const h = await boot();
  h.start();
  h.attach();
  const first = await h.open("https://a.example/one");
  const second = await h.open("https://a.example/two");
  await h.fake.chrome.tabs.ungroup(first.live.id);
  const revoked = await h.until((message) => message.type === "to-server" && message.message.revoked?.includes(first.tab.id));
  assert.deepEqual(revoked.message.state.tabs.map((tab: { id: string }) => tab.id), [second.tab.id]);
  h.fake.removeTab(second.live.id);
  const closed = await h.until((message) => message.type === "to-server" && message.message.state?.tabs.length === 0);
  assert.deepEqual(closed.message.revoked, [first.tab.id, second.tab.id]);
  const after = await h.command({ action: { type: "tabs.focus", tabID: first.tab.id } });
  assert.equal(after.outcome.type, "failure");
});

test("a dragged-in web tab is shared; browser pages and a full chat are refused", async () => {
  const h = await boot();
  h.start();
  h.attach();
  const { live } = await h.open("https://a.example/");
  const user = h.fake.createTab({ url: "https://b.example/", windowId: 1 });
  await h.fake.chrome.tabs.group({ tabIds: user.id, groupId: live.groupId });
  await h.until((message) => message.type === "to-server" && message.message.state?.tabs.length === 2);

  const settings = h.fake.createTab({ url: "chrome://settings", windowId: 1 });
  await h.fake.chrome.tabs.group({ tabIds: settings.id, groupId: live.groupId });
  await new Promise((resolve) => setTimeout(resolve, 10));
  assert.equal(h.fake.tabsById.get(settings.id)!.groupId, -1);
  assert.equal(h.host.snapshot().chats[0].refused, "restricted");

  for (let index = 2; index < 32; index++) await h.open("about:blank");
  const extra = h.fake.createTab({ url: "https://c.example/", windowId: 1 });
  await h.fake.chrome.tabs.group({ tabIds: extra.id, groupId: live.groupId });
  await new Promise((resolve) => setTimeout(resolve, 10));
  assert.equal(h.fake.tabsById.get(extra.id)!.groupId, -1);
  assert.equal(h.host.snapshot().chats[0].refused, "full");
  const full = await h.command({ action: { type: "tabs.open" }, policy: ask });
  assert.match(full.outcome.message, /32 maximum/);
});

test("a popup from an agent tab joins its chat; one bound for a blocked site is closed", async () => {
  const h = await boot();
  h.start();
  h.attach();
  const { live } = await h.command({
    action: { type: "tabs.open" },
    policy: { ...ask, sites: [{ origin: "https://evil.example", browsing: "deny", downloads: "deny", uploads: "deny" }] },
  }).then(() => ({ live: [...h.fake.tabsById.values()].at(-1)! }));
  const popup = h.fake.createTab({ url: "https://login.example/", windowId: 1, openerTabId: live.id });
  await h.until((message) => message.type === "to-server" && message.message.state?.tabs.length === 2);
  assert.equal(h.fake.tabsById.get(popup.id)!.groupId, live.groupId);
  const blocked = h.fake.createTab({ url: "https://evil.example/x", windowId: 1, openerTabId: live.id });
  await new Promise((resolve) => setTimeout(resolve, 10));
  assert.equal(h.fake.tabsById.has(blocked.id), false);
});

test("a tab a background agent tab opens joins its chat, whatever Chrome names as its opener", async () => {
  const h = await boot();
  h.start();
  h.attach();
  const { live } = await h.open("https://a.example/");
  const mine = h.fake.createTab({ url: "https://mine.example/", active: true });
  // Chrome names the active tab as opener, but groups the new tab with the tab that opened it.
  const popup = h.fake.createTab({ url: "", openerTabId: mine.id, source: live.id });
  await h.until((message) => message.type === "to-server" && message.message.state?.tabs.length === 2);
  assert.equal(h.host.debug.registry.owner(popup.id)?.entry.thread, THREAD);
  assert.equal(h.host.debug.registry.owner(mine.id), undefined);
  // A browser page opened in the group (the user's "New tab in group") is not the agent's.
  const page = h.fake.createTab({ url: "chrome://newtab/", source: live.id });
  await new Promise((resolve) => setTimeout(resolve, 10));
  assert.equal(h.host.debug.registry.owner(page.id), undefined);
  assert.equal(h.fake.tabsById.get(page.id)!.groupId, -1);
});

test("the page may not follow a redirect to a site this chat was not allowed", async () => {
  const h = await boot();
  h.start();
  h.attach();
  h.fake.redirects.set("https://a.example/go", "https://b.example/landing");
  const blocked = await h.command({
    action: { type: "tabs.open", url: "https://a.example/go" },
    policy: ask,
    target: { resources: ["https://a.example/go"], key: "open" },
  });
  assert.equal(blocked.outcome.code, "site-blocked");
  assert.match(blocked.outcome.message, /https:\/\/b\.example/);
  assert.equal([...h.fake.tabsById.values()].at(-1)!.url, "about:blank");

  h.fake.redirects.set("https://a.example/stay", "https://www.a.example/home");
  const twin = await h.command({
    action: { type: "tabs.open", url: "https://a.example/stay" },
    policy: ask,
    target: { resources: ["https://a.example/stay", "https://www.a.example"], key: "open" },
  });
  assert.equal(twin.outcome.type, "success");
  assert.equal(twin.outcome.result.value.url, "https://www.a.example/home");
});

test("a frame or resource from a blocked site is not loaded, and the allowed page and the command stand", async () => {
  const h = await boot();
  h.start();
  h.attach();
  h.fake.subresources.set("https://shop.example/", [
    { url: "https://tracker.example/pixel.gif", resourceType: "Image" },
    { url: "https://cdn.example/app.js", resourceType: "Script" },
    { url: "https://tracker.example/frame", resourceType: "Document", frameId: "child-frame" },
  ]);
  const policy = { ...ask, sites: [{ origin: "https://tracker.example", browsing: "deny", downloads: "deny", uploads: "deny" }] };
  const opened = await h.command({
    action: { type: "tabs.open", url: "https://shop.example/" },
    policy,
    target: { resources: ["https://shop.example/"], key: "open" },
  });
  assert.equal(opened.outcome.type, "success", JSON.stringify(opened.outcome));
  assert.equal([...h.fake.tabsById.values()].at(-1)!.url, "https://shop.example/");
  assert.deepEqual(h.fake.refused, ["https://tracker.example/pixel.gif", "https://tracker.example/frame"]);
});

/** A driver page that answers every command with `{ok:true}` and records it. */
function recordingPage(calls: string[]): CreatePage {
  return (surface, _cdp, options) => ({
    ready: Promise.resolve(),
    state: () => ({ id: options.id, url: surface.url(), title: "", loading: false, canGoBack: false, canGoForward: false, generation: 0 }),
    sources: () => [],
    async inspect() {
      return { resources: [], key: "k" };
    },
    async execute(command) {
      calls.push(command.action.type);
      return { value: { ok: true }, files: [] };
    },
    navigationStarted() {},
    personActed() {},
    dispose() {},
  });
}

test("a download the page begins just after a click returned is still that click's, for 10 s", async () => {
  const calls: string[] = [];
  const h = await boot({ createPage: recordingPage(calls) });
  h.start();
  h.attach();
  const { tab, live } = await h.open("https://a.example/");
  const items = new Map<number, object>();
  (h.fake.chrome.downloads as { search: unknown }).search = async ({ id }: { id: number }) => [items.get(id)];
  const download = (id: number, name: string) => {
    const url = `https://a.example/${name}`;
    // The server answers the click with an attachment after the click returned.
    h.fake.chrome.debugger.onEvent.emit({ tabId: live.id }, "Page.downloadWillBegin", { frameId: "f", guid: `g${id}`, url, suggestedFilename: name });
    const item = { id, url, finalUrl: url, filename: `/Users/me/Downloads/${name}`, fileSize: 3, mime: "text/csv", startTime: new Date().toISOString() };
    items.set(id, item);
    h.fake.chrome.downloads.onCreated.emit(item);
    h.fake.chrome.downloads.onChanged.emit({ id, state: { current: "complete", previous: "in_progress" } });
  };
  const click = await h.command({ action: { type: "click", tabID: tab.id, ref: "@e1" }, policy: ask, target: { resources: ["https://a.example"], key: "k" } });
  assert.equal(click.outcome.type, "success");
  const id = h.sent.findLast((message) => message.type === "to-server" && message.message.type === "result")!.message.id;
  download(1, "export.csv");
  assert.deepEqual(await h.until((message) => message.type === "download"), {
    type: "download",
    server: "server-1",
    thread: THREAD,
    command: id,
    path: "/Users/me/Downloads/export.csv",
    url: "https://a.example/export.csv",
  });
  // Long after the command, a download the page starts (or the user's own) is not the agent's.
  const now = Date.now;
  Date.now = () => now() + 11_000;
  try {
    const from = h.sent.length;
    download(2, "later.csv");
    await new Promise((resolve) => setTimeout(resolve, 10));
    assert.deepEqual(h.sent.slice(from).filter((message) => message.type === "download"), []);
  } finally {
    Date.now = now;
  }
});

/** A driver page that logs its life (creation, commands, navigations, disposal); `delay` slows a command. */
function loggingPage(log: string[], delay: (type: string) => number = () => 0): CreatePage {
  return (surface, _cdp, options) => {
    let generation = options.generation ?? 0;
    log.push("create");
    return {
      ready: Promise.resolve(),
      state: () => ({ id: options.id, url: surface.url(), title: "", loading: false, canGoBack: false, canGoForward: false, generation }),
      sources: () => [],
      async inspect() {
        return { resources: [], key: "k" };
      },
      async execute(command) {
        log.push(command.action.type);
        await new Promise((resolve) => setTimeout(resolve, delay(command.action.type)));
        return { value: { ok: true }, files: [] };
      },
      navigationStarted() {
        generation++;
        log.push("navigation");
        options.publish();
      },
      personActed() {},
      dispose() {
        log.push("dispose");
      },
    };
  };
}

async function eventually(check: () => boolean, timeout = 2_000) {
  const deadline = Date.now() + timeout;
  while (!check()) {
    if (Date.now() > deadline) throw new Error("It never happened");
    await new Promise((resolve) => setTimeout(resolve, 2));
  }
}

test("an idle chat lets go of its tabs but keeps their driver pages, so the model's refs outlast its thinking", async () => {
  const log: string[] = [];
  const h = await boot({ createPage: loggingPage(log), idleMs: 20 });
  h.start();
  h.attach();
  const { tab, live } = await h.open("https://a.example/");
  const page = h.host.debug.pages.get(live.id);
  assert.equal((await h.command({ action: { type: "snapshot", tabID: tab.id } })).outcome.type, "success");
  await eventually(() => !h.host.debug.attached(live.id));
  assert.equal(h.fake.attached.has(live.id), false, "Chrome's debugging bar can go");
  assert.equal(h.host.debug.pages.get(live.id), page, "the page and its refs stay");

  // The model comes back: the same page runs its command on the same document.
  const click = await h.command({ action: { type: "click", tabID: tab.id, ref: "@e1" } });
  assert.equal(click.outcome.type, "success");
  assert.equal(h.host.debug.attached(live.id), true);
  assert.deepEqual(log, ["create", "navigation", "snapshot", "click"]);

  // The user loads another page while Haarness is away: that page has none of the refs.
  await eventually(() => !h.host.debug.attached(live.id));
  await h.fake.chrome.tabs.update(live.id, { url: "https://a.example/next" });
  assert.deepEqual(log.slice(4), ["navigation"]);
  assert.equal(h.states().at(-1)!.message.state.tabs[0].generation, 2);
  await h.command({ action: { type: "snapshot", tabID: tab.id } });
  assert.deepEqual(log.slice(4), ["navigation", "snapshot"], "counted once, not again on attaching");

  // A document Chrome did not report (or could not be checked) is caught on attaching again.
  await eventually(() => !h.host.debug.attached(live.id));
  h.fake.tabsById.get(live.id)!.loaderId = "loader-unseen";
  await h.command({ action: { type: "snapshot", tabID: tab.id } });
  assert.deepEqual(log.slice(6), ["navigation", "snapshot"]);
  assert.equal(log.filter((entry) => entry === "create").length, 1);
});

test("a chat that goes idle with a JavaScript dialog open lets go of its page entirely", async () => {
  const log: string[] = [];
  const h = await boot({ createPage: loggingPage(log), idleMs: 20 });
  h.start();
  h.attach();
  const { tab, live } = await h.open("https://a.example/");
  await h.command({ action: { type: "snapshot", tabID: tab.id } });
  h.fake.chrome.debugger.onEvent.emit({ tabId: live.id }, "Page.javascriptDialogOpening", { url: "https://a.example/", message: "Sure?", type: "confirm", hasBrowserHandler: true });
  // Detached, the page would never hear the user close it.
  await eventually(() => !h.host.debug.pages.has(live.id));
  assert.equal(h.fake.attached.has(live.id), false);
  assert.equal(log.at(-1), "dispose");
  assert.equal((await h.command({ action: { type: "snapshot", tabID: tab.id } })).outcome.type, "success");
  assert.equal(log.filter((entry) => entry === "create").length, 2);
});

test("one idle timer per chat: commands that overlapped do not let go of the tabs early", async () => {
  const log: string[] = [];
  const h = await boot({ createPage: loggingPage(log, (type) => (type === "wait" ? 100 : 0)), idleMs: 200 });
  h.start();
  h.attach();
  const { tab, live } = await h.open("https://a.example/");
  // Two calls for one chat at once (an inspect and an execute, say): the first finishes first.
  await Promise.all([
    h.command({ action: { type: "snapshot", tabID: tab.id } }),
    h.command({ action: { type: "wait", tabID: tab.id, condition: "load" } }),
  ]);
  await new Promise((resolve) => setTimeout(resolve, 50));
  const started = Date.now();
  await h.command({ action: { type: "snapshot", tabID: tab.id } });
  // A timer the first call left behind would fire 200 ms after it, about now.
  await new Promise((resolve) => setTimeout(resolve, 100 - (Date.now() - started)));
  assert.equal(h.host.debug.attached(live.id), true, "attached for 200 ms after the last command");
  await eventually(() => !h.host.debug.attached(live.id));
});

test("a command that arrives while an idle chat is being let go keeps its tab", async () => {
  const log: string[] = [];
  const h = await boot({ createPage: loggingPage(log) });
  h.start();
  h.attach();
  const tabs = [await h.open("https://a.example/1"), await h.open("https://a.example/2"), await h.open("https://a.example/3")];
  for (const { tab } of tabs) await h.command({ action: { type: "snapshot", tabID: tab.id } });
  // Chrome takes its time to detach.
  const detach = h.fake.chrome.debugger.detach;
  h.fake.chrome.debugger.detach = async (target: { tabId: number }) => {
    await new Promise((resolve) => setTimeout(resolve, 20));
    return detach(target);
  };
  const key = h.host.snapshot().chats[0].key;
  const releasing = h.host.debug.release(key);
  const last = tabs[2];
  const page = h.host.debug.pages.get(last.live.id);
  const snapshot = await h.command({ action: { type: "snapshot", tabID: last.tab.id } });
  await releasing;
  assert.equal(snapshot.outcome.type, "success", JSON.stringify(snapshot.outcome));
  assert.equal(h.host.debug.attached(tabs[0].live.id), false, "the tab being let go went");
  assert.equal(h.host.debug.attached(last.live.id), true, "the tab in use stays");
  assert.equal(h.host.debug.pages.get(last.live.id), page);
  assert.equal(log.includes("dispose"), false);
});

test("pages Haarness may not control, opaque pages and Chrome-less tools are refused", async () => {
  const calls: string[] = [];
  const h = await boot({ createPage: recordingPage(calls) });
  h.start();
  h.attach();
  const web = await h.open("https://a.example/");
  const blank = await h.open("about:blank");
  h.fake.commit(h.fake.tabsById.get(web.live.id)!, "chrome://settings");
  const restricted = await h.command({ action: { type: "snapshot", tabID: web.tab.id } });
  assert.equal(restricted.outcome.code, "restricted-page");
  const evaluate = await h.command({ action: { type: "evaluate", tabID: blank.tab.id, script: "1" } });
  assert.equal(evaluate.outcome.code, "unsupported");
  const lighthouse = await h.command({ action: { type: "lighthouse", tabID: blank.tab.id } });
  assert.equal(lighthouse.outcome.code, "unsupported");
  // A blank page still has a snapshot.
  const snapshot = await h.command({ action: { type: "snapshot", tabID: blank.tab.id } });
  assert.equal(snapshot.outcome.type, "success");
  assert.deepEqual(calls, ["snapshot"]);
});

test("pause and stop fail commands with their own codes and tell every server", async () => {
  const h = await boot();
  h.start();
  h.attach();
  await h.host.request({ type: "pause", paused: true });
  await h.until((message) => message.type === "status" && message.paused === true);
  assert.equal((await h.command({ action: { type: "tabs.list" } })).outcome.code, "paused");
  await h.host.request({ type: "pause", paused: false });
  assert.equal((await h.command({ action: { type: "tabs.list" } })).outcome.type, "success");

  // Cancel on Chrome's debugging bar.
  h.host.events.debuggerDetached(1, "canceled_by_user");
  await h.until((message) => message.type === "status" && message.stopped === true);
  assert.equal((await h.command({ action: { type: "tabs.list" } })).outcome.code, "stopped-by-user");
  assert.equal(h.host.snapshot().pill, "stopped");
  await h.host.request({ type: "resume" });
  await h.until((message) => message.type === "status" && message.stopped === false);
  assert.equal((await h.command({ action: { type: "tabs.list" } })).outcome.type, "success");
});

test("Stop all stops the browser and interrupts every working chat", async () => {
  const h = await boot();
  h.start();
  h.attach();
  await h.open("about:blank");
  h.fake.port.deliver({ type: "activity", server: "server-1", active: [THREAD], approvals: [] });
  await new Promise((resolve) => setTimeout(resolve, 5));
  assert.equal(h.host.snapshot().chats[0].working, true);
  h.host.events.stopAll();
  const stop = await h.until((message) => message.type === "stop");
  assert.deepEqual(stop, { type: "stop", server: "server-1", thread: THREAD });
  assert.equal(h.host.snapshot().stopped, true);
});

test("approvals from the popup reach the bridge, and only explicit Chrome ones are shown", async () => {
  const h = await boot();
  h.start();
  h.attach();
  h.fake.port.deliver({
    type: "activity",
    server: "server-1",
    active: [],
    approvals: [
      { id: "p1", thread: THREAD, patterns: [`chrome-browsing:${THREAD}:https://a.example`], explicit: true },
      { id: "p2", thread: THREAD, patterns: ["bash:ls"], explicit: false },
    ],
  });
  await new Promise((resolve) => setTimeout(resolve, 5));
  assert.deepEqual(h.host.snapshot().approvals.map((approval) => approval.id), ["p1"]);
  assert.equal(h.fake.badge, "!");
  const answer = h.host.request({ type: "answer", server: "server-1", permission: "p1", decision: "always" });
  await h.until((message) => message.type === "answer");
  h.fake.port.deliver({ type: "answered", server: "server-1", permission: "p1", ok: true });
  assert.deepEqual(await answer, { ok: true });
});

test("a Chrome site waiting together with another approval is shown, to answer in Haarness", async () => {
  const h = await boot();
  h.start();
  h.attach();
  h.fake.port.deliver({
    type: "activity",
    server: "server-1",
    active: [THREAD],
    approvals: [
      { id: "p3", thread: THREAD, patterns: [`chrome-uploads:${THREAD}:https://a.example`], explicit: true, answerable: false },
    ],
  });
  await new Promise((resolve) => setTimeout(resolve, 5));
  const [approval] = h.host.snapshot().approvals;
  assert.equal(approval.answerable, false);
  assert.deepEqual(approval.lines, [{ category: "uploads", origin: "https://a.example" }]);
  assert.equal(h.fake.badge, "!");
  // An approval from a bridge that predates `answerable` is one Chrome may answer.
  h.fake.port.deliver({
    type: "activity",
    server: "server-1",
    active: [],
    approvals: [{ id: "p4", thread: THREAD, patterns: [`chrome-browsing:${THREAD}:https://b.example`], explicit: true }],
  });
  await new Promise((resolve) => setTimeout(resolve, 5));
  assert.equal(h.host.snapshot().approvals[0].answerable, true);
});

test("failures the popup and welcome page show are interface keys, translated where they render", async () => {
  const h = await boot();
  h.start();
  h.attach();
  await h.open("about:blank");
  const key = h.host.snapshot().chats[0].key;
  assert.deepEqual(await h.host.request({ type: "show-tabs", key: "missing" }), { ok: false, error: "This chat has no tabs here." });
  const answer = (error: string) => {
    const reply = h.host.request({ type: "answer", server: "server-1", permission: "p1", decision: "always" });
    void h.until((message) => message.type === "answer").then(() =>
      h.fake.port.deliver({ type: "answered", server: "server-1", permission: "p1", ok: false, error }),
    );
    return reply;
  };
  assert.deepEqual(await answer("Chrome can only answer a Chrome approval that is waiting."), {
    ok: false,
    error: "This approval is no longer waiting.",
  });
  // Haarness's own text, which has no key, is the reason in a translated sentence.
  assert.deepEqual(await answer("Permission not found"), {
    ok: false,
    error: "Haarness could not do that: {reason}",
    values: { reason: "Permission not found" },
  });
  // The project's server went away.
  h.fake.port.deliver({
    type: "servers",
    servers: [{ id: "server-1", workspace: WORKSPACE, name: "acme", version: "0.1.1", state: "error", error: "restart this project's Haarness server" }],
  });
  await new Promise((resolve) => setTimeout(resolve, 5));
  assert.equal(h.host.snapshot().projects[0].error, "Restart this project's Haarness server.");
  h.fake.port.deliver({ type: "servers", servers: [] });
  assert.deepEqual(await h.host.request({ type: "stop-chat", key }), { ok: false, error: "This project is not connected." });
});

test("a newer copy on disk reloads the extension once, when nothing is running", async () => {
  const h = await boot();
  h.fake.port.deliver({ type: "bridge", protocol: 1, version: "0.1.2", uploadsRoot: "/tmp", extension: { path: "/x", version: "0.1.2" } });
  await new Promise((resolve) => setTimeout(resolve, 5));
  assert.equal(h.fake.reloads, 1);
  h.fake.port.deliver({ type: "bridge", protocol: 1, version: "0.1.2", uploadsRoot: "/tmp", extension: { path: "/x", version: "0.1.2" } });
  await new Promise((resolve) => setTimeout(resolve, 5));
  assert.equal(h.fake.reloads, 1, "a copy Chrome does not load from is offered, not reloaded forever");
  assert.equal(h.host.snapshot().pill, "update");
});

test("a bridge from another protocol version relays nothing", async () => {
  const h = await boot();
  h.fake.port.deliver({ type: "bridge", protocol: 2, version: "9.0.0", uploadsRoot: "/tmp", extension: null });
  h.fake.port.deliver({ type: "servers", servers: [{ id: "s", workspace: WORKSPACE, name: "acme", version: "9", state: "connected" }] });
  await new Promise((resolve) => setTimeout(resolve, 5));
  assert.equal(h.host.snapshot().pill, "mismatch");
  assert.deepEqual(h.host.snapshot().projects, []);
});

test("tabs that closed while the worker was stopped are revoked when it starts again", async () => {
  const h = await boot({
    prepare(fake) {
      fake.stores.session.set("registry", {
        instances: { [WORKSPACE]: "instance-1" },
        threads: [
          {
            workspace: WORKSPACE,
            thread: THREAD,
            title: "Old",
            parent: null,
            groupId: null,
            tabs: [{ id: "tab_00000000-0000-0000-0000-000000000001", tabId: 999, generation: 2, url: "https://a.example/", title: "", loading: false, canGoBack: false, canGoForward: false }],
            focusedTabID: null,
            revoked: [],
            approved: [],
            policy: null,
            refused: null,
            touchedAt: 1,
          },
        ],
      });
    },
  });
  const entry = h.host.debug.registry.all()[0];
  assert.deepEqual(entry.tabs, []);
  assert.deepEqual(entry.revoked, ["tab_00000000-0000-0000-0000-000000000001"]);
});

test("the shared page driver plugs in through createPage and runs inside the site guard", async () => {
  const calls: string[] = [];
  let surface: PageSurface | undefined;
  let pageOptions: PageOptions | undefined;
  let generation = 0;
  const createPage: CreatePage = (pageSurface, cdp, options) => {
    surface = pageSurface;
    pageOptions = options as PageOptions;
    generation = options.generation ?? 0;
    return {
      ready: Promise.resolve(),
      state: () => ({ id: options.id, url: pageSurface.url(), title: "", loading: false, canGoBack: false, canGoForward: false, generation }),
      sources: () => [],
      async inspect() {
        return { resources: [], key: "k" };
      },
      async execute(command) {
        calls.push(`${command.action.type}@${command.generation}`);
        await cdp.send("Runtime.evaluate", { expression: "1" });
        return { value: { ok: true }, files: [] };
      },
      navigationStarted() {
        generation++;
      },
      personActed() {},
      dispose() {
        calls.push("dispose");
      },
    };
  };
  const h = await boot({ createPage });
  h.start();
  h.attach();
  const { tab } = await h.open("https://a.example/");
  // As on Desktop, the page exists before the tab's first load, so it saw that navigation;
  // the about:blank Chrome reports for the new tab is not a navigation of its own.
  assert.equal(pageOptions?.generation, 0);
  const state = h.states().at(-1)!.message.state.tabs[0];
  const result = await h.command({
    action: { type: "click", tabID: tab.id, ref: "@e1" },
    policy: ask,
    target: { resources: ["https://a.example"], key: "k" },
    generation: state.generation,
  });
  assert.deepEqual(result.outcome, { type: "success", result: { value: { ok: true }, files: [] } });
  // The page counts the tab's generation itself; nothing is translated.
  assert.equal(state.generation, 1, "loading the URL moved the generation");
  assert.deepEqual(calls, [`click@${state.generation}`]);
  // Request bodies and session headers stay out of network.get in the user's own browser.
  assert.equal(pageOptions?.diagnostics?.requestBodies, false);
  assert.equal(pageOptions?.diagnostics?.redactHeader?.("x-csrf-token"), true);
  assert.equal(surface?.resourceOrigins, "securityOrigin");
  assert.equal(surface?.wwwTwin, true);
  assert.equal(surface?.gateTopDocumentOnly, true);
  assert.deepEqual(surface?.selectAll, { commands: ["selectAll"] });
  assert.equal(surface?.supports("heap.snapshot"), false);
});

test("a command that fails as its tab closes answers a moment later, so a quitting Chrome's disconnect answers instead", async () => {
  let fail: (() => void) | undefined;
  const createPage: CreatePage = (pageSurface, _cdp, options) => ({
    ready: Promise.resolve(),
    state: () => ({ id: options.id, url: pageSurface.url(), title: "", loading: false, canGoBack: false, canGoForward: false, generation: 0 }),
    sources: () => [],
    async inspect() {
      return { resources: [], key: "k" };
    },
    execute: () => new Promise((_, reject) => (fail = () => reject(new Error("Detached while handling command.")))),
    navigationStarted() {},
    personActed() {},
    dispose() {},
  });
  const h = await boot({ createPage });
  h.start();
  h.attach();
  const { tab, live } = await h.open("https://a.example/");
  const from = h.sent.length;
  const waiting = h.command({ action: { type: "wait", tabID: tab.id, condition: "text", text: "Never" }, policy: ask, target: { resources: ["https://a.example"], key: "k" } });
  while (!fail) await new Promise((resolve) => setTimeout(resolve, 2));
  h.fake.removeTab(live.id);
  fail();
  await new Promise((resolve) => setTimeout(resolve, 500));
  assert.equal(h.sent.slice(from).some((message) => message.message?.type === "result"), false);
  assert.equal((await waiting).outcome.type, "failure");
});

test("an older copy on disk (Haarness was downgraded) reloads once but is not offered as an update", async () => {
  const h = await boot();
  h.fake.port.deliver({ type: "bridge", protocol: 1, version: "0.1.0", uploadsRoot: "/tmp", extension: { path: "/x", version: "0.1.0" } });
  await new Promise((resolve) => setTimeout(resolve, 5));
  assert.equal(h.fake.reloads, 1);
  assert.equal(h.host.snapshot().update, null);
});

test("a new profile label reaches Haarness in a fresh hello once nothing is running", async () => {
  const h = await boot();
  h.start();
  const before = h.fake.ports.length;
  await h.host.request({ type: "settings", settings: { label: "  Work  " } });
  assert.equal(h.fake.ports.length, before + 1);
  assert.equal(h.fake.ports[before - 1].connected, false, "the old bridge is let go");
  assert.equal(h.sent[0].browser.label, "Work");
  assert.equal((h.fake.stores.local.get("settings") as { label: string }).label, "Work");
});

test("Show tabs is the one place Haarness brings a window forward, because the user asked", async () => {
  const h = await boot();
  h.start();
  h.attach();
  const { live } = await h.open("about:blank");
  h.fake.windowIds.push(2);
  h.fake.focused = 2;
  const key = h.host.snapshot().chats[0].key;
  assert.deepEqual(await h.host.request({ type: "show-tabs", key }), { ok: true });
  assert.equal(h.fake.focused, live.windowId);
  assert.equal(h.fake.tabsById.get(live.id)!.active, true);
});

test("tabs.focus brings a tab forward only where that takes nothing from the user", async () => {
  const h = await boot();
  h.start();
  h.attach();
  const first = await h.open("https://a.example/one");
  const second = await h.open("https://a.example/two");
  // The user is reading a tab of their own in the window they are using.
  const own = h.fake.createTab({ url: "https://mail.example/", windowId: 1, active: true });
  const focus = async (tabID: string) => {
    const { outcome } = await h.command({ action: { type: "tabs.focus", tabID } });
    assert.equal(outcome.type, "success", JSON.stringify(outcome));
    return outcome.result.value as { id: string; note?: string };
  };
  const kept = await focus(first.tab.id);
  assert.match(kept.note ?? "", /stays in the background.*will not bring it forward/);
  assert.equal(h.fake.tabsById.get(own.id)!.active, true, "the user's tab stays in front");
  assert.equal(h.fake.tabsById.get(first.live.id)!.active, false);
  assert.equal(h.states().at(-1)!.message.state.focusedTabID, first.tab.id, "it is still the chat's focused tab");

  // In a window the user is not using, the tab comes forward.
  h.fake.windowIds.push(2);
  h.fake.focused = 2;
  const shown = await focus(second.tab.id);
  assert.equal(shown.note, undefined);
  assert.equal(h.fake.tabsById.get(second.live.id)!.active, true);

  // That window is the user's again but they have not come back to it: the
  // agent may replace the tab it brought forward there with another of its own.
  h.fake.focused = 1;
  assert.equal((await focus(first.tab.id)).note, undefined);
  assert.equal(h.fake.tabsById.get(first.live.id)!.active, true);
  // A tab already in front stays as it is.
  assert.equal((await focus(first.tab.id)).note, undefined);
  assert.equal(h.fake.focused, 1, "never raises a window");

  // Once the user comes back to the window, its front tab is theirs to read.
  h.fake.chrome.windows.onFocusChanged.emit(1);
  assert.match((await focus(second.tab.id)).note ?? "", /stays in the background/);
  assert.equal(h.fake.tabsById.get(first.live.id)!.active, true);
});

test("tabs.focus never brings a tab forward over a tab the user dragged into the chat", async () => {
  const h = await boot();
  h.start();
  h.attach();
  const agents = await h.open("https://a.example/one");
  // The user drags a tab of theirs into the chat's group and keeps working in it.
  const user = h.fake.createTab({ url: "https://b.example/", windowId: 1 });
  await h.fake.chrome.tabs.group({ tabIds: user.id, groupId: agents.live.groupId });
  await h.until((message) => message.type === "to-server" && message.message.state?.tabs.length === 2);
  await h.fake.chrome.tabs.update(user.id, { active: true });
  const { outcome } = await h.command({ action: { type: "tabs.focus", tabID: agents.tab.id } });
  assert.equal(outcome.type, "success", JSON.stringify(outcome));
  assert.match(outcome.result.value.note ?? "", /stays in the background/);
  assert.equal(h.fake.tabsById.get(user.id)!.active, true);
});
