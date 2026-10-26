/**
 * The extension against a fake bridge in a real Chromium: native messaging,
 * reconnects, the per-chat tab inventory and the user's controls.
 */
import { askPolicy, inner, isToServer, OTHER_THREAD, SERVER, THREAD, VERSION, WORKSPACE } from "./fixtures/bridge";
import { expect, snapshot, test } from "./fixtures/extension";

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

test("says hello over native messaging and reconnects when the bridge exits", async ({ bridge, worker }) => {
  const first = await bridge.wait((message) => message.type === "hello");
  expect(first.message).toMatchObject({
    type: "hello",
    protocol: 1,
    browser: { kind: "chrome", label: null },
    extension: { version: VERSION },
    paused: false,
    stopped: false,
  });
  const browser = first.message.browser as { id: string; name: string; version: string };
  expect(browser.id).toMatch(UUID);
  expect(browser.name.length).toBeGreaterThan(0);
  expect(browser.version).toMatch(/^\d+\./);
  expect(worker.url()).toBe("chrome-extension://accdjhmcgljbadpeanajllmcembhkomn/background.js");

  bridge.start();
  await expect.poll(async () => (await snapshot(worker)).connection).toBe("connected");

  // The bridge dies; Chrome reports "Native host has exited." and the worker starts a new one.
  bridge.send({ __exit: 3 });
  await expect.poll(async () => (await snapshot(worker)).connection).toBe("bridge-exited");
  const second = await bridge.wait((message, entry) => message.type === "hello" && entry.connection === 2, 20_000);
  expect((second.message.browser as { id: string }).id).toBe(browser.id);
  bridge.start();
  await expect.poll(async () => (await snapshot(worker)).connection).toBe("connected");
});

test.describe("before setup", () => {
  test.use({ setUp: false });
  test("shows how to finish setup, then connects once the host is registered", async ({ bridge, installHost, openExtensionPage }) => {
    const popup = await openExtensionPage("popup.html");
    await expect(popup.getByRole("status")).toHaveText("Not set up");
    await expect(popup.getByRole("heading", { name: "Finish setup" })).toBeVisible();
    await expect(popup.getByText("haarness browser setup")).toBeVisible();
    await expect(popup.getByRole("button", { name: "Copy command" })).toBeVisible();
    // `haarness browser setup` writes the manifest; the open popup retries every 2 s.
    installHost();
    await bridge.wait((message) => message.type === "hello", 20_000);
    bridge.start();
    await expect(popup.getByRole("status")).toHaveText("Connected");
    await expect(popup.getByRole("heading", { name: "No chats are using Chrome yet" })).toBeVisible();
  });
});

test("opens agent tabs in a group named after the chat and sends state before every result", async ({ bridge, worker, site }) => {
  await bridge.wait((message) => message.type === "hello");
  bridge.start();
  bridge.attach();
  const url = `${site.origin}/docs`;

  const inspect = await bridge.command({ action: { type: "tabs.open", url }, inspect: true });
  expect(inspect.result.outcome).toEqual({ type: "success", result: { value: { resources: [url], key: "open" }, files: [] } });

  const open = await bridge.command({ action: { type: "tabs.open", url }, policy: askPolicy, target: { resources: [url], key: "open" } });
  expect(open.result.outcome.type).toBe("success");
  const tab = (open.result.outcome as { result: { value: { id: string; url: string; title: string } } }).result.value;
  expect(tab.id).toMatch(/^tab_[a-f0-9-]{36}$/);
  expect(tab.url).toBe(url);
  expect(tab.title).toBe("Docs");
  // The chat's inventory reaches the server before the result does.
  const states = open.before.filter((entry) => isToServer(entry.message, "state"));
  expect(states.length).toBeGreaterThan(0);
  expect(inner(states.at(-1)!.message)).toMatchObject({ thread: THREAD, state: { tabs: [{ id: tab.id, url }] } });
  expect(states.at(-1)!.message.server).toBe(SERVER);

  const groups = await worker.evaluate(() => chrome.tabGroups.query({}));
  expect(groups).toHaveLength(1);
  expect(groups[0].title).toBe("Research trip");
  const grouped = await worker.evaluate((groupId) => chrome.tabs.query({ groupId }), groups[0].id);
  expect(grouped.map((entry) => entry.url)).toEqual([url]);
  expect(grouped[0].active).toBe(false);

  const list = await bridge.command({ action: { type: "tabs.list" } });
  expect(list.result.outcome).toMatchObject({ type: "success", result: { value: { tabs: [{ id: tab.id }], focusedTabID: tab.id } } });

  // Another chat's tabs are its own, in its own group.
  const other = await bridge.command({ action: { type: "tabs.list" } }, { thread: OTHER_THREAD, title: "Second" });
  expect(other.result.outcome).toMatchObject({ type: "success", result: { value: { tabs: [] } } });
  const focus = await bridge.command(
    { action: { type: "tabs.focus", tabID: tab.id } },
    { thread: OTHER_THREAD, title: "Second" },
  );
  expect(focus.result.outcome).toMatchObject({ type: "failure", code: "operation_failed" });

  // After a reconnect the server learns this browser's tabs again, and the title follows the chat.
  bridge.attach("instance-2");
  const republished = await bridge.wait((message) => isToServer(message, "state") && inner(message).thread === THREAD, 15_000, bridge.received.length);
  expect(inner(republished.message).state.tabs[0].id).toBe(tab.id);
  const info = await bridge.wait((message) => message.type === "thread-info");
  expect(info.message).toMatchObject({ server: SERVER, thread: THREAD });
  bridge.send({ type: "thread", server: SERVER, thread: THREAD, title: "Trip to Lisbon", archived: false, parent: null });
  await expect.poll(async () => (await worker.evaluate(() => chrome.tabGroups.query({})))[0]?.title).toBe("Trip to Lisbon");

  // The server keys a tab's target on the generation the extension last published.
  const latest = await bridge.command({ action: { type: "tabs.list" } });
  const generation = (latest.result.outcome as { result: { value: { tabs: { generation: number }[] } } }).result.value.tabs[0].generation;
  const stale = await bridge.command({ action: { type: "tabs.close", tabID: tab.id }, target: { resources: [url], key: `${tab.id}:${generation + 1}` } });
  expect(stale.result.outcome).toMatchObject({ type: "failure", message: expect.stringContaining("target changed") });
  const close = await bridge.command({ action: { type: "tabs.close", tabID: tab.id }, target: { resources: [url], key: `${tab.id}:${generation}` } });
  expect(close.result.outcome).toMatchObject({ type: "success", result: { value: { tabs: [] } } });
  await expect.poll(() => worker.evaluate(() => chrome.tabs.query({ url: "http://127.0.0.1/*" }).then((tabs) => tabs.length))).toBe(0);
});

test("a tab the user drags out of the group is revoked", async ({ bridge, worker, site }) => {
  await bridge.wait((message) => message.type === "hello");
  bridge.start();
  bridge.attach();
  const url = `${site.origin}/`;
  const open = await bridge.command({ action: { type: "tabs.open", url }, policy: askPolicy, target: { resources: [url], key: "open" } });
  const tab = (open.result.outcome as { result: { value: { id: string } } }).result.value;
  const chromeTab = await worker.evaluate((url) => chrome.tabs.query({ url }).then(([found]) => found.id!), url);

  const from = bridge.received.length;
  await worker.evaluate((id) => chrome.tabs.ungroup(id), chromeTab);
  const state = await bridge.wait((message) => isToServer(message, "state") && inner(message).state.tabs.length === 0, 15_000, from);
  expect(inner(state.message).revoked).toEqual([tab.id]);
  const after = await bridge.command({ action: { type: "tabs.focus", tabID: tab.id } });
  expect(after.result.outcome.type).toBe("failure");
  // The user's tab stays open; it is simply theirs again.
  expect(await worker.evaluate((id) => chrome.tabs.get(id).then((found) => found.groupId), chromeTab)).toBe(-1);
});

test("a tab dragged into a chat's group is shared, but browser pages are refused", async ({ bridge, worker, site, context }) => {
  await bridge.wait((message) => message.type === "hello");
  bridge.start();
  bridge.attach();
  const url = `${site.origin}/`;
  await bridge.command({ action: { type: "tabs.open", url }, policy: askPolicy, target: { resources: [url], key: "open" } });
  const [group] = await worker.evaluate(() => chrome.tabGroups.query({}));

  const page = await context.newPage();
  await page.goto(`${site.origin}/shared`);
  const shared = await worker.evaluate((target) => chrome.tabs.query({ url: target }).then(([found]) => found.id!), `${site.origin}/shared`);
  await worker.evaluate(([id, groupId]) => chrome.tabs.group({ tabIds: id, groupId }), [shared, group.id] as const);
  await bridge.wait((message) => isToServer(message, "state") && inner(message).state.tabs.length === 2);

  // The user's own browser page. (Not context.newPage(): Chromium names the
  // active tab, here a chat's, as the opener of that blank tab, so the
  // extension rightly adopts it as a page the agent's tab opened.)
  const restricted = await worker.evaluate(() =>
    chrome.tabs.create({ url: chrome.runtime.getURL("welcome.html") }).then((tab) => tab.id!),
  );
  await expect.poll(() => worker.evaluate((id) => chrome.tabs.get(id).then((found) => found.status), restricted)).toBe("complete");
  await worker.evaluate(([id, groupId]) => chrome.tabs.group({ tabIds: id, groupId }), [restricted, group.id] as const);
  await expect.poll(() => worker.evaluate((id) => chrome.tabs.get(id).then((found) => found.groupId), restricted)).toBe(-1);
  await expect.poll(async () => (await snapshot(worker)).chats[0]?.refused).toBe("restricted");
  const list = await bridge.command({ action: { type: "tabs.list" } });
  expect((list.result.outcome as { result: { value: { tabs: unknown[] } } }).result.value.tabs).toHaveLength(2);
});

test("stops the page from leaving for a site this chat was not allowed", async ({ bridge, site, worker }) => {
  await bridge.wait((message) => message.type === "hello");
  bridge.start();
  bridge.attach();
  // Approved for 127.0.0.1 only; the page redirects to localhost (another origin, still "ask").
  const url = `${site.origin}/redirect?to=${encodeURIComponent(`${site.other}/elsewhere`)}`;
  const open = await bridge.command({ action: { type: "tabs.open", url }, policy: askPolicy, target: { resources: [url], key: "open" } });
  expect(open.result.outcome).toMatchObject({ type: "failure", code: "site-blocked" });
  expect((open.result.outcome as { message: string }).message).toContain(site.other);
  const urls = await worker.evaluate(() => chrome.tabs.query({}).then((tabs) => tabs.map((tab) => tab.url)));
  expect(urls.some((entry) => entry?.startsWith(site.other))).toBe(false);

  // With localhost approved too, the same redirect lands.
  const allowed = await bridge.command({
    action: { type: "tabs.open", url },
    policy: { ...askPolicy, sites: [{ origin: site.other, browsing: "allow", downloads: "ask", uploads: "ask" }] },
    target: { resources: [url], key: "open" },
  });
  expect(allowed.result.outcome).toMatchObject({ type: "success", result: { value: { url: `${site.other}/elsewhere` } } });
});

test("pause and stop refuse commands until the user turns Haarness back on", async ({ bridge, worker, site, openExtensionPage }) => {
  await bridge.wait((message) => message.type === "hello");
  bridge.start();
  bridge.attach();
  const url = `${site.origin}/`;
  await bridge.command({ action: { type: "tabs.open", url }, policy: askPolicy, target: { resources: [url], key: "open" } });
  const popup = await openExtensionPage("popup.html");
  const toggle = popup.getByRole("switch", { name: "Let Haarness use this browser" });
  await expect(toggle).toBeChecked();

  await toggle.click();
  await bridge.wait((message) => message.type === "status" && message.paused === true);
  await expect(popup.getByRole("status")).toHaveText("Paused");
  const paused = await bridge.command({ action: { type: "tabs.list" } });
  expect(paused.result.outcome).toMatchObject({ type: "failure", code: "paused" });
  await toggle.click();
  await bridge.wait((message) => message.type === "status" && message.paused === false);

  // "Stop Haarness in this browser" (the shortcut, or Cancel on Chrome's debugging bar).
  await worker.evaluate(() => (globalThis as unknown as { haarness: { events: { stopAll(): void } } }).haarness.events.stopAll());
  await bridge.wait((message) => message.type === "status" && message.stopped === true);
  await expect(popup.getByRole("heading", { name: "You stopped Haarness" })).toBeVisible();
  const stopped = await bridge.command({ action: { type: "tabs.list" } });
  expect(stopped.result.outcome).toMatchObject({ type: "failure", code: "stopped-by-user" });

  await popup.getByRole("button", { name: "Resume" }).click();
  await bridge.wait((message) => message.type === "status" && message.stopped === false);
  const resumed = await bridge.command({ action: { type: "tabs.list" } });
  expect(resumed.result.outcome.type).toBe("success");
});

test("answers a Chrome approval and stops a working chat from the popup", async ({ bridge, site, openExtensionPage }) => {
  await bridge.wait((message) => message.type === "hello");
  bridge.start();
  bridge.attach();
  const url = `${site.origin}/`;
  await bridge.command({ action: { type: "tabs.open", url }, policy: askPolicy, target: { resources: [url], key: "open" } });
  const permission = "0192a5f0-0000-7000-8000-000000000001";
  bridge.send({
    type: "activity",
    server: SERVER,
    active: [THREAD],
    approvals: [{ id: permission, thread: THREAD, patterns: [`chrome-browsing:${THREAD}:https://example.com`], explicit: true }],
  });
  const popup = await openExtensionPage("popup.html");
  await expect(popup.getByText("Use example.com in Chrome")).toBeVisible();
  await popup.getByRole("button", { name: "Allow in this chat" }).click();
  const answer = await bridge.wait((message) => message.type === "answer");
  expect(answer.message).toEqual({ type: "answer", server: SERVER, permission, decision: "always" });
  bridge.send({ type: "answered", server: SERVER, permission, ok: true });
  bridge.send({ type: "activity", server: SERVER, active: [THREAD], approvals: [] });
  await expect(popup.getByText("Use example.com in Chrome")).toBeHidden();

  await expect(popup.getByText("Working…")).toBeVisible();
  await popup.getByRole("button", { name: "Stop", exact: true }).click();
  const stop = await bridge.wait((message) => message.type === "stop");
  expect(stop.message).toEqual({ type: "stop", server: SERVER, thread: THREAD });
  bridge.send({ type: "stopped", server: SERVER, thread: THREAD, ok: true });
  void WORKSPACE;
});
