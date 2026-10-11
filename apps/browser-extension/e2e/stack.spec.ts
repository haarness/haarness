/**
 * Haarness for Chrome end to end, with nothing faked: a real daemon, the real
 * bridge started by Chromium through native messaging, the built extension
 * running the shared page driver, and the SDK as the client. Approvals are
 * answered the way a person would, through the server's permissions API.
 */
import type { BrowserToolResponse } from "@haarness/sdk";
import { expect, test, value, type Stack } from "./fixtures/stack";

type Tab = { id: string; url: string; title: string; generation: number; browser?: string };

/** Opens a page in Chrome for `thread`, allowing its site in this chat when asked. */
async function open(stack: Stack, thread: Awaited<ReturnType<Stack["thread"]>>, url: string) {
  const call = stack.call(thread, "browser_tabs_open", { url, browser: "chrome" });
  await stack.answer(thread, "always");
  return value<Tab>(await call.done);
}

function failed(response: BrowserToolResponse) {
  expect(response.isError, response.output).toBe(true);
  return response.output;
}

/** The element ref whose snapshot line contains `text`. */
function ref(snapshot: { content: string }, text: string) {
  const line = snapshot.content.split("\n").find((entry) => entry.includes(text) && /@e\d+/.test(entry));
  if (!line) throw new Error(`No ref for ${text} in:\n${snapshot.content}`);
  return line.match(/@e\d+/)![0];
}

test("opens a Chrome tab in a group named after the chat once the site and its www twin are allowed", async ({ stack }) => {
  const thread = await stack.thread("Research trip");
  const url = stack.site.url("shop", "/");
  const call = stack.call(thread, "browser_tabs_open", { url, browser: "chrome" });
  // One prompt names the site and its www twin; it always needs a person.
  const prompt = await stack.answer(thread, "always");
  expect(prompt.explicit).toBe(true);
  expect(prompt.patterns).toEqual([
    `chrome-browsing:${thread.id}:${stack.site.origin("shop")}`,
    `chrome-browsing:${thread.id}:${stack.site.origin("www.shop")}`,
  ]);
  const response = await call.done;
  expect(response.metadata).toMatchObject({ browser: "chrome" });
  const tab = value<Tab>(response);
  expect(tab).toMatchObject({ url, title: "Home of shop.test", browser: "chrome" });

  const groups = await stack.worker.evaluate(() => chrome.tabGroups.query({}));
  expect(groups.map((group) => group.title)).toEqual(["Research trip"]);
  const grouped = await stack.worker.evaluate((groupId) => chrome.tabs.query({ groupId }), groups[0].id);
  expect(grouped.map((entry) => entry.url)).toEqual([url]);
  // Agent tabs open in the background.
  expect(grouped[0].active).toBe(false);

  const list = value<{ tabs: Tab[]; browsers: { browser: string; available: boolean }[] }>(
    await stack.tool(thread, "browser_tabs_list", {}),
  );
  expect(list.tabs.map((entry) => [entry.id, entry.browser])).toEqual([[tab.id, "chrome"]]);
  expect(list.browsers).toEqual([
    expect.objectContaining({ browser: "haarness", available: false }),
    expect.objectContaining({ browser: "chrome", available: true }),
  ]);

  // The twin rode along with the first answer; another site asks again.
  const twin = value<Tab>(await stack.tool(thread, "browser_navigate", { tabID: tab.id, url: stack.site.url("www.shop", "/") }));
  expect(twin.title).toBe("Home of www.shop.test");
  const other = stack.call(thread, "browser_navigate", { tabID: tab.id, url: stack.site.url("other", "/") });
  const asked = await stack.answer(thread, "once");
  expect(asked.patterns).toEqual([
    `chrome-browsing:${thread.id}:${stack.site.origin("other")}`,
    `chrome-browsing:${thread.id}:${stack.site.origin("www.other")}`,
  ]);
  expect(value<Tab>(await other.done).title).toBe("Home of other.test");

  const hosts = await stack.api.listBrowserHosts({ thread: thread.id });
  expect(hosts.hosts).toEqual([expect.objectContaining({ kind: "chrome", threadTabs: 1, paused: false, stopped: false })]);
});

test("snapshot, find, fill, fill_form, select, check, press, hover, scroll and click act on the page", async ({ stack }) => {
  const thread = await stack.thread("Order supplies");
  const tab = await open(stack, thread, stack.site.url("shop", "/form"));
  const page = await stack.page(tab.id);
  const found = value<{ content: string }>(await stack.tool(thread, "browser_find", { tabID: tab.id, text: "Send order" }));
  expect(found.content).toContain("Send order");
  // A snapshot expires the refs of every earlier one, including find's.
  let snapshot = value<{ content: string }>(await stack.tool(thread, "browser_snapshot", { tabID: tab.id }));
  // Snapshots hide password values.
  expect(snapshot.content).not.toContain("hunter2");

  // fill selects the field's text first, so it replaces it (⌘A does not select on macOS through CDP).
  await stack.tool(thread, "browser_fill", { tabID: tab.id, ref: ref(snapshot, "Name"), text: "Ada Lovelace" });
  await expect.poll(() => page.locator("#name").inputValue()).toBe("Ada Lovelace");

  snapshot = value(await stack.tool(thread, "browser_snapshot", { tabID: tab.id }));
  await stack.tool(thread, "browser_fill_form", {
    tabID: tab.id,
    fields: [
      { ref: ref(snapshot, "Notes"), type: "text", value: "Leave at the door" },
      { ref: ref(snapshot, "combobox"), type: "select", values: ["l"] },
      { ref: ref(snapshot, "Gift wrap"), type: "check", checked: true },
    ],
  });
  expect(await page.locator("#notes").inputValue()).toBe("Leave at the door");
  expect(await page.locator("#size").inputValue()).toBe("l");
  expect(await page.locator("#gift").isChecked()).toBe(true);

  await stack.tool(thread, "browser_select", { tabID: tab.id, ref: ref(snapshot, "combobox"), values: ["m"] });
  expect(await page.locator("#size").inputValue()).toBe("m");
  await stack.tool(thread, "browser_check", { tabID: tab.id, ref: ref(snapshot, "Gift wrap"), checked: false });
  expect(await page.locator("#gift").isChecked()).toBe(false);

  await stack.tool(thread, "browser_press", { tabID: tab.id, key: "x" });
  await expect.poll(() => page.locator("#keys").textContent()).toContain("x");
  await stack.tool(thread, "browser_hover", { tabID: tab.id, ref: ref(snapshot, "Hover me") });
  await expect.poll(() => page.locator("#hover").textContent()).toBe("Hovered");
  await stack.tool(thread, "browser_scroll", { tabID: tab.id, deltaY: 600 });
  await expect.poll(() => page.evaluate(() => scrollY)).toBeGreaterThan(0);
  await stack.tool(thread, "browser_scroll", { tabID: tab.id, deltaY: -10_000 });
  await expect.poll(() => page.evaluate(() => scrollY)).toBe(0);

  const click = value<{ dispatched: boolean; observation: { text: string } }>(
    await stack.tool(thread, "browser_click", { tabID: tab.id, ref: ref(snapshot, "Send order") }),
  );
  expect(click.dispatched).toBe(true);
  await expect.poll(() => page.locator("#status").textContent()).toBe("Saved order-1");

  // The agent's cursor is drawn in an isolated world: page scripts and snapshots never see it.
  const after = value<{ content: string }>(await stack.tool(thread, "browser_snapshot", { tabID: tab.id }));
  expect(after.content).toContain("Saved order-1");
  expect(after.content).not.toMatch(/Clicking|Typing|haarness/i);
  expect(await page.evaluate(() => "__haarnessCursor" in globalThis)).toBe(false);
});

test("console and network diagnostics keep request bodies and session headers out", async ({ stack }) => {
  const thread = await stack.thread("Check the order API");
  const tab = await open(stack, thread, stack.site.url("shop", "/form"));
  const page = await stack.page(tab.id);
  const snapshot = value<{ content: string }>(await stack.tool(thread, "browser_snapshot", { tabID: tab.id }));
  await stack.tool(thread, "browser_click", { tabID: tab.id, ref: ref(snapshot, "Send order") });
  await expect.poll(() => page.locator("#status").textContent()).toBe("Saved order-1");

  const console = value<{ messages: { level: string; text: string }[] }>(await stack.tool(thread, "browser_console", { tabID: tab.id }));
  expect(console.messages.map((entry) => entry.text).join("\n")).toContain("order sent");

  const list = value<{ requests: { id: string; url: string; method: string }[] }>(
    await stack.tool(thread, "browser_network_list", { tabID: tab.id }),
  );
  const order = list.requests.find((request) => request.url.endsWith("/api/order"));
  expect(order?.method).toBe("POST");
  const detail = await stack.tool(thread, "browser_network_get", { tabID: tab.id, id: order!.id, includeBody: true });
  const result = value<{
    requestHeaders: { name: string; value: string }[];
    responseHeaders: { name: string; value: string }[];
    requestBody: { state: string; reason?: string };
    responseBody: { state: string; text?: string };
  }>(detail);
  expect(result.requestBody).toEqual({ state: "unavailable", reason: "omitted" });
  expect(detail.output).not.toContain("hunter2");
  const header = (headers: { name: string; value: string }[], name: string) => headers.find((entry) => entry.name === name)?.value;
  expect(header(result.requestHeaders, "authorization")).toBe("<redacted>");
  expect(header(result.requestHeaders, "x-csrf-token")).toBe("<redacted>");
  expect(header(result.requestHeaders, "x-trace")).toBe("visible");
  expect(header(result.responseHeaders, "set-cookie")).toBe("<redacted>");
  expect(header(result.responseHeaders, "x-request-token")).toBe("<redacted>");
  for (const secret of ["csrf-secret", "api-secret", "cookie-secret", "token-secret"]) expect(detail.output).not.toContain(secret);
  expect(result.responseBody.text).toContain("order-1");
});

test("evaluate needs its own approval for scripts, and is refused on a blank page", async ({ stack }) => {
  const thread = await stack.thread("Read the title");
  const tab = await open(stack, thread, stack.site.url("shop", "/form"));
  // Browsing is allowed in this chat; running scripts on the site still asks.
  const call = stack.call(thread, "browser_evaluate", { tabID: tab.id, script: "document.title" });
  const prompt = await stack.answer(thread, "always");
  expect(prompt.patterns).toEqual([`chrome-scripts:${thread.id}:${stack.site.origin("shop")}`]);
  expect(value<{ value: unknown }>(await call.done).value).toBe("Order form");
  expect(value<{ value: unknown }>(await stack.tool(thread, "browser_evaluate", { tabID: tab.id, script: "1 + 1" })).value).toBe(2);

  // about:blank has no site to approve, and nothing that may use the user's identity runs there.
  const blank = value<Tab>(await stack.tool(thread, "browser_tabs_open", { url: "about:blank", browser: "chrome" }));
  expect(failed(await stack.tool(thread, "browser_evaluate", { tabID: blank.id, script: "document.cookie" }))).toContain(
    "This browser tool is not available in Chrome; use the Haarness browser in Desktop for it.",
  );
  // A blank page can still be read.
  expect(value<{ content: string }>(await stack.tool(thread, "browser_snapshot", { tabID: blank.id })).content).toBeDefined();
});

test("frames list a cross-site iframe whose fields are usable once its site is allowed", async ({ stack }) => {
  const thread = await stack.thread("Fill the embedded form");
  const tab = await open(stack, thread, stack.site.url("shop", "/frame"));
  const page = await stack.page(tab.id);
  await expect.poll(() => page.frames().length).toBe(2);
  const frames = value<{ frames: { id: string; parentID?: string; url: string }[] }>(await stack.tool(thread, "browser_frames", { tabID: tab.id }));
  expect(frames.frames.map((frame) => frame.url)).toEqual([stack.site.url("shop", "/frame"), stack.site.url("other", "/inner")]);
  const inner = frames.frames.find((frame) => frame.parentID)!;
  // Reading the other site's frame is gated as that site, as is a ref inside it.
  const read = stack.call(thread, "browser_snapshot", { tabID: tab.id, frameID: inner.id });
  const prompt = await stack.answer(thread, "always");
  expect(prompt.patterns).toEqual([`chrome-browsing:${thread.id}:${stack.site.origin("other")}`]);
  const snapshot = value<{ content: string }>(await read.done);
  value(await stack.tool(thread, "browser_fill", { tabID: tab.id, ref: ref(snapshot, "Inner field"), text: "From the agent" }));
  await expect.poll(() => page.frames()[1].locator("#inner-field").inputValue()).toBe("From the agent");
});

test("a JavaScript dialog is reported by the click that opened it and answered with browser_dialog", async ({ stack }) => {
  const thread = await stack.thread("Say hello");
  const tab = await open(stack, thread, stack.site.url("shop", "/form"));
  const snapshot = value<{ content: string }>(await stack.tool(thread, "browser_snapshot", { tabID: tab.id }));
  const click = value<{ dialog: { type: string; message: string } }>(
    await stack.tool(thread, "browser_click", { tabID: tab.id, ref: ref(snapshot, "Greet") }),
  );
  expect(click.dialog).toMatchObject({ type: "alert", message: "Hello from the page" });
  expect(value<{ dialog: unknown }>(await stack.tool(thread, "browser_dialog", { tabID: tab.id, action: "get" })).dialog).toMatchObject({
    message: "Hello from the page",
  });
  expect(value<{ dialog: unknown }>(await stack.tool(thread, "browser_dialog", { tabID: tab.id, action: "accept" })).dialog).toBeNull();
  // The page works again.
  expect(value<{ value: unknown }>(await evaluateAllowed(stack, thread, tab.id, "document.title")).value).toBe("Order form");
});

test("wait follows the page until text appears and a slow load finishes", async ({ stack }) => {
  const thread = await stack.thread("Wait for it");
  const tab = await open(stack, thread, stack.site.url("shop", "/later?ms=1500"));
  value(await stack.tool(thread, "browser_wait", { tabID: tab.id, condition: "text", text: "Ready now", timeoutMs: 10_000 }));
  const slow = stack.call(thread, "browser_navigate", { tabID: tab.id, url: stack.site.url("shop", "/slow?ms=1500") });
  expect(value<Tab>(await slow.done).title).toBe("Slow page");
  value(await stack.tool(thread, "browser_wait", { tabID: tab.id, condition: "load" }));
  expect(failed(await stack.tool(thread, "browser_wait", { tabID: tab.id, condition: "text", text: "Never shown", timeoutMs: 500 }))).toContain(
    "browser_wait failed",
  );
});

test("screenshots show a background tab only in a window the user is not using", async ({ stack }) => {
  const thread = await stack.thread("Capture the page");
  const tab = await open(stack, thread, stack.site.url("shop", "/"));
  const tabId = await stack.host((host, id) => host.debug.registry.all()[0].tabs.find((entry: any) => entry.id === id).tabId, tab.id);
  const agentWindow = await stack.worker.evaluate((id) => chrome.tabs.get(id).then((entry) => entry.windowId), tabId);
  const activeBefore = await stack.worker.evaluate((windowId) => chrome.tabs.query({ windowId, active: true }).then(([entry]) => entry.id), agentWindow);
  expect(activeBefore).not.toBe(tabId);
  const focused = () => stack.worker.evaluate(() => chrome.windows.getLastFocused().then((window) => (window.focused ? window.id : null)));

  // The user is in the agent's window: Haarness does not take the screen from them.
  expect(await focused()).toBe(agentWindow);
  expect(failed(await stack.tool(thread, "browser_screenshot", { tabID: tab.id }))).toContain(
    "Chrome cannot capture this tab while it is in the background of the window the user is using.",
  );

  // The user works in another window: the agent's tab is shown for the capture, then the user's tab comes back.
  await stack.worker.evaluate(() => chrome.windows.create({ url: "about:blank", focused: true }));
  await expect.poll(focused).not.toBe(agentWindow);
  const shot = await stack.tool(thread, "browser_screenshot", { tabID: tab.id });
  expect(shot.isError, shot.output).toBe(false);
  expect(JSON.stringify(shot.metadata)).toMatch(/screenshot\.png/);
  await expect
    .poll(() => stack.worker.evaluate((windowId) => chrome.tabs.query({ windowId, active: true }).then(([entry]) => entry.id), agentWindow))
    .toBe(activeBefore);

  // Focusing the tab is the model's way out; it never raises the window.
  value(await stack.tool(thread, "browser_tabs_focus", { tabID: tab.id }));
  expect(await focused()).not.toBe(agentWindow);
  expect((await stack.tool(thread, "browser_screenshot", { tabID: tab.id })).isError).toBe(false);
});

/** Runs a script on an origin this chat may already run scripts on, allowing it when asked. */
async function evaluateAllowed(stack: Stack, thread: Awaited<ReturnType<Stack["thread"]>>, tabID: string, script: string) {
  const call = stack.call(thread, "browser_evaluate", { tabID, script });
  const done = await Promise.race([call.done.then(() => true), new Promise((resolve) => setTimeout(() => resolve(false), 1_500))]);
  if (!done) await stack.answer(thread, "always");
  return call.done;
}

test("a Chrome site asks even in Auto mode, and Allow in this chat covers only that chat", async ({ stack }) => {
  const thread = await stack.thread("Auto chat");
  await stack.api.setApprovalMode(thread.id, { mode: "auto" });
  const call = stack.call(thread, "browser_tabs_open", { url: stack.site.url("shop", "/"), browser: "chrome" });
  const prompt = await stack.answer(thread, "always");
  expect(prompt.explicit).toBe(true);
  const tab = value<Tab>(await call.done);
  // Allowed in this chat: no further prompt for the site.
  expect(value<Tab>(await stack.tool(thread, "browser_navigate", { tabID: tab.id, url: stack.site.url("shop", "/form") })).title).toBe("Order form");

  // Another chat asks again.
  const other = await stack.thread("Another chat");
  const second = stack.call(other, "browser_tabs_open", { url: stack.site.url("shop", "/"), browser: "chrome" });
  const asked = await stack.answer(other, "reject");
  expect(asked.patterns).toContain(`chrome-browsing:${other.id}:${stack.site.origin("shop")}`);
  expect(failed(await second.done)).toBeTruthy();
});

test.describe("with every tool allowed in the project's configuration", () => {
  test.use({ projectConfig: '[defaults]\nprovider = "echo"\nmodel = "echo-1"\n[permissions]\n"*" = "allow"\nbrowser_site = "allow"\n' });

  test("Chrome sites still ask, and Allow once covers one call", async ({ stack }) => {
    const thread = await stack.thread("Configured chat");
    const call = stack.call(thread, "browser_tabs_open", { url: stack.site.url("shop", "/"), browser: "chrome" });
    expect((await stack.answer(thread, "once")).explicit).toBe(true);
    const tab = value<Tab>(await call.done);
    const again = stack.call(thread, "browser_navigate", { tabID: tab.id, url: stack.site.url("shop", "/form") });
    await stack.answer(thread, "once");
    expect(value<Tab>(await again.done).title).toBe("Order form");
  });
});

test("a blocked site is refused, and a redirect to a blocked or unapproved site is stopped", async ({ stack }) => {
  const read = (await stack.api.manageResource("browser-settings", { action: "get", scope: "global" })) as { data: { revision: string } };
  const all = (decision: string) => ({ browsing: decision, downloads: decision, uploads: decision, scripts: decision });
  await stack.api.manageResource("browser-settings", {
    action: "set",
    scope: "global",
    revision: read.data.revision,
    chrome: {
      enabled: true,
      defaultBrowser: "auto",
      defaults: all("ask"),
      sites: [{ origin: stack.site.origin("blocked"), ...all("deny") }],
    },
  });
  const thread = await stack.thread("Blocked sites");
  expect(failed(await stack.tool(thread, "browser_tabs_open", { url: stack.site.url("blocked", "/"), browser: "chrome" }))).toMatch(/denied|blocked|not allowed/i);
  expect(stack.site.requests.some((request) => request.host === "blocked.test")).toBe(false);

  const tab = await open(stack, thread, stack.site.url("shop", "/"));
  // shop.test is allowed in this chat, but the redirect lands on a site it is not.
  const unapproved = failed(
    await stack.tool(thread, "browser_navigate", { tabID: tab.id, url: stack.site.url("shop", `/redirect?to=${encodeURIComponent(stack.site.url("other", "/"))}`) }),
  );
  expect(unapproved).toContain(stack.site.origin("other"));
  expect(unapproved).toContain("Call browser_navigate with that URL to ask the user, or choose another site.");
  expect(stack.site.requests.some((request) => request.host === "other.test")).toBe(false);
  const blocked = failed(
    await stack.tool(thread, "browser_navigate", { tabID: tab.id, url: stack.site.url("shop", `/redirect?to=${encodeURIComponent(stack.site.url("blocked", "/"))}`) }),
  );
  expect(blocked).toContain(stack.site.origin("blocked"));
  expect(stack.site.requests.some((request) => request.host === "blocked.test")).toBe(false);
  // The tab never shows either site.
  const list = value<{ tabs: Tab[] }>(await stack.tool(thread, "browser_tabs_list", {}));
  expect(list.tabs[0].url).not.toMatch(/other\.test|blocked\.test/);
});

test("Chrome's own pages are off limits even in an agent tab", async ({ stack }) => {
  const thread = await stack.thread("Restricted");
  const tab = await open(stack, thread, stack.site.url("shop", "/"));
  // The user takes the tab to a browser page.
  const page = await stack.page(tab.id);
  await page.goto("chrome://version").catch(() => {});
  await expect.poll(() => page.url()).toBe("chrome://version/");
  expect(failed(await stack.tool(thread, "browser_snapshot", { tabID: tab.id }))).toContain(
    "Haarness cannot control this Chrome page (browser, extension and file pages are off limits). Navigate the tab to an http(s) site.",
  );
});

test("an upload reaches the site from the file the bridge staged", async ({ stack }) => {
  const thread = await stack.thread("Send the notes");
  const tab = await open(stack, thread, stack.site.url("shop", "/upload"));
  const snapshot = value<{ content: string }>(await stack.tool(thread, "browser_snapshot", { tabID: tab.id }));
  const input = snapshot.content.split("\n").find((line) => /@e\d+ \[button\]/.test(line) && !/Upload/.test(line));
  expect(input, snapshot.content).toBeTruthy();
  const call = stack.call(thread, "browser_files_upload", { tabID: tab.id, ref: input!.match(/@e\d+/)![0], paths: ["notes.txt"] });
  const prompt = await stack.answer(thread, "once");
  expect(prompt.patterns!.join(" ")).toContain(`chrome-uploads:${thread.id}:${stack.site.origin("shop")}`);
  value(await call.done);
  await expect.poll(() => stack.site.uploads).toEqual([{ name: "notes.txt", body: "Upload me 日本\n" }]);
  const page = await stack.page(tab.id);
  await expect(page.locator("#uploaded")).toHaveText("Uploaded notes.txt");
});

test("a download the agent started is listed for its tab and handed over once allowed", async ({ stack }) => {
  const thread = await stack.thread("Fetch the report");
  const tab = await open(stack, thread, stack.site.url("shop", "/downloads"));
  const snapshot = value<{ content: string }>(await stack.tool(thread, "browser_snapshot", { tabID: tab.id }));
  value(await stack.tool(thread, "browser_click", { tabID: tab.id, ref: ref(snapshot, "Get the report") }));
  let files: { id: string; name: string; state: string }[] = [];
  await expect
    .poll(async () => {
      files = value<{ files: typeof files }>(await stack.tool(thread, "browser_files_list", { tabID: tab.id })).files;
      return files.map((file) => file.state);
    })
    .toEqual(["completed"]);
  const call = stack.call(thread, "browser_files_get", { tabID: tab.id, fileID: files[0].id });
  const prompt = await stack.answer(thread, "once");
  expect(prompt.patterns).toEqual([`chrome-downloads:${thread.id}:${stack.site.origin("shop")}`]);
  const saved = value<{ files: { path: string; workspaceRelativePath?: string; bytes: number }[] }>(await call.done);
  const [file] = saved.files;
  expect(file.bytes).toBe("Quarterly report from the fixture".length);
  const { readFileSync } = await import("node:fs");
  expect(readFileSync(file.path, "utf8")).toBe("Quarterly report from the fixture");
  // A download the user starts in their own tab is never the agent's.
  const mine = await stack.context.newPage();
  await mine.goto(stack.site.url("shop", "/downloads"));
  await mine.click("#report");
  await new Promise((resolve) => setTimeout(resolve, 1_500));
  expect(value<{ files: unknown[] }>(await stack.tool(thread, "browser_files_list", { tabID: tab.id })).files).toHaveLength(1);
});

test("a window the agent's page opens joins the chat's group", async ({ stack }) => {
  const thread = await stack.thread("Follow the popup");
  const tab = await open(stack, thread, stack.site.url("shop", "/popup"));
  const snapshot = value<{ content: string }>(await stack.tool(thread, "browser_snapshot", { tabID: tab.id }));
  value(await stack.tool(thread, "browser_click", { tabID: tab.id, ref: ref(snapshot, "Open window") }));
  let tabs: Tab[] = [];
  await expect
    .poll(async () => {
      tabs = value<{ tabs: Tab[] }>(await stack.tool(thread, "browser_tabs_list", {})).tabs;
      return tabs.map((entry) => entry.title).sort();
    })
    .toEqual(["Popped window", "Popup opener"]);
  const popped = tabs.find((entry) => entry.title === "Popped window")!;
  const groups = await stack.worker.evaluate(() => chrome.tabGroups.query({}));
  expect(groups).toHaveLength(1);
  const grouped = await stack.worker.evaluate((groupId) => chrome.tabs.query({ groupId }), groups[0].id);
  expect(grouped).toHaveLength(2);
  expect(value<{ content: string }>(await stack.tool(thread, "browser_snapshot", { tabID: popped.id })).content).toContain("Opened by the page.");
});
