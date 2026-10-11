import { test } from "node:test";
import assert from "node:assert/strict";
import type { PendingPermissionInfo } from "@haarness/sdk";
import {
  chromeApproval,
  chromeApprovalLines,
  chromeConnection,
  chromeThreadTabs,
  defaultChromeSettings,
  editableChromeSettings,
  extensionsPage,
  isExplicitPermission,
  setupBrowsers,
  webStoreLink,
  type ChromeHost,
  type ChromeSetupInfo,
} from "../src/chrome";
import { canAutoApprove, createAutoApprover } from "../src/renderer/approval-mode";
import { ranInChrome } from "../src/renderer/transcript";

const host = (overrides: Partial<ChromeHost> = {}): ChromeHost => ({
  kind: "chrome",
  browserId: crypto.randomUUID(),
  name: "Google Chrome",
  label: null,
  paused: false,
  stopped: false,
  ...overrides,
});

test("the Chrome status names one browser, counts several, and says when the user paused or stopped it", () => {
  assert.deepEqual(chromeConnection([]), { state: "none" });
  assert.deepEqual(chromeConnection([host({ kind: "desktop" })]), { state: "none" });
  assert.deepEqual(chromeConnection([host()]), { state: "connected", name: "Google Chrome" });
  assert.deepEqual(chromeConnection([host({ label: "Work" })]), {
    state: "connected",
    name: "Google Chrome (Work)",
  });
  assert.deepEqual(chromeConnection([host(), host({ name: "Microsoft Edge" })]), {
    state: "several",
    count: 2,
  });
  // A usable browser is what matters; a paused one beside it is not news.
  assert.deepEqual(chromeConnection([host({ paused: true }), host({ name: "Brave" })]), {
    state: "connected",
    name: "Brave",
  });
  assert.deepEqual(chromeConnection([host({ paused: true })]), { state: "paused" });
  assert.deepEqual(chromeConnection([host({ stopped: true })]), { state: "stopped" });
  assert.deepEqual(
    chromeConnection([host({ stopped: true }), host({ paused: true })]),
    { state: "paused" },
  );
  assert.equal(
    chromeThreadTabs([host({ threadTabs: 2 }), host({ threadTabs: 1 }), host({ kind: "desktop", threadTabs: 4 })]),
    3,
  );
  assert.equal(chromeThreadTabs([host()]), 0);
});

test("Chrome rows edit the global layer, never a project's tightened copy", () => {
  assert.deepEqual(editableChromeSettings(undefined), defaultChromeSettings());
  assert.deepEqual(defaultChromeSettings().defaults, {
    browsing: "ask",
    downloads: "ask",
    uploads: "ask",
    scripts: "ask",
  });
  const global = { ...defaultChromeSettings(), defaultBrowser: "chrome" as const };
  const effective = { ...global, enabled: false };
  assert.deepEqual(
    editableChromeSettings({ settings: effective, globalSettings: global, projectRestrictions: ["Chrome is turned off"] }),
    global,
  );
  assert.deepEqual(
    editableChromeSettings({ settings: effective, globalSettings: null, projectRestrictions: [] }),
    defaultChromeSettings(),
  );
  const view = { settings: global, globalSettings: global, projectRestrictions: [] };
  editableChromeSettings(view).sites.push({ origin: "https://x.test", ...global.defaults });
  assert.equal(view.globalSettings.sites.length, 0);
});

test("the setup report picks Chrome's extensions page and offers only an https Web Store link", () => {
  const info: ChromeSetupInfo = {
    hostName: "ai.haarness.browser",
    extensionId: "accdjhmcgljbadpeanajllmcembhkomn",
    webStoreUrl: null,
    browsers: [
      { browser: "edge", name: "Microsoft Edge", extensionsPage: "edge://extensions", registered: true, supported: true },
      { browser: "chrome", name: "Google Chrome", extensionsPage: "chrome://extensions", registered: true, supported: true },
      { browser: "chromium", name: "Chromium", extensionsPage: "chrome://extensions", registered: false, supported: false, note: "Snap" },
    ],
    extension: { path: "/data/browser/chrome/Haarness for Chrome", version: "0.1.1" },
  };
  assert.equal(extensionsPage(info), "chrome://extensions");
  assert.deepEqual(setupBrowsers(info).ready.map((item) => item.browser), ["edge", "chrome"]);
  assert.deepEqual(setupBrowsers(info).unsupported.map((item) => item.browser), ["chromium"]);
  assert.equal(extensionsPage({ ...info, browsers: [info.browsers[0]] }), "edge://extensions");
  assert.equal(extensionsPage({ ...info, browsers: [] }), "chrome://extensions");
  assert.equal(webStoreLink(info), undefined);
  assert.equal(webStoreLink({ ...info, webStoreUrl: "javascript:alert(1)" }), undefined);
  assert.equal(
    webStoreLink({ ...info, webStoreUrl: "https://chromewebstore.google.com/detail/x" }),
    "https://chromewebstore.google.com/detail/x",
  );
});

const thread = "0199a7a2-7c41-7000-8000-000000000001";
test("Chrome approvals read as sites per category, with a site and its www twin as one line", () => {
  assert.deepEqual(chromeApproval(`chrome-browsing:${thread}:https://mail.example.test`), {
    category: "browsing",
    thread,
    origin: "https://mail.example.test",
  });
  assert.deepEqual(chromeApproval(`browser_site:chrome-scripts:${thread}:http://127.0.0.1:8443`), {
    category: "scripts",
    thread,
    origin: "http://127.0.0.1:8443",
  });
  for (const pattern of [
    "browsing:https://example.test",
    "browser_site:browsing:https://example.test",
    "browser:navigate:https://example.test",
    `chrome-cookies:${thread}:https://example.test`,
  ])
    assert.equal(chromeApproval(pattern), undefined, pattern);
  assert.deepEqual(
    chromeApprovalLines([
      `browser_site:chrome-browsing:${thread}:https://example.test`,
      `browser_site:chrome-browsing:${thread}:https://www.example.test`,
      `browser_site:chrome-scripts:${thread}:https://example.test`,
      `browser_site:chrome-browsing:${thread}:https://example.test`,
      `chrome-downloads:${thread}:https://files.example.test`,
      "read:/tmp/upload.txt",
    ]),
    [
      { category: "browsing", origin: "https://example.test" },
      { category: "scripts", origin: "https://example.test" },
      { category: "downloads", origin: "https://files.example.test" },
    ],
  );
});

const request = (overrides: Partial<PendingPermissionInfo> & { explicit?: boolean } = {}) =>
  ({
    permissionId: crypto.randomUUID(),
    permission: "browser_site",
    operation: `browser_site:chrome-browsing:${thread}:https://example.test`,
    patterns: [`chrome-browsing:${thread}:https://example.test`],
    alwaysPatterns: [`browser_site:chrome-browsing:${thread}:https://example.test`],
    threadId: thread,
    rootThreadId: thread,
    createdAt: new Date().toISOString(),
    ...overrides,
  }) as PendingPermissionInfo & { explicit?: boolean };

test("explicit Chrome requests are never answered by Auto, even before a server marks them", async () => {
  assert.equal(isExplicitPermission(request()), true);
  assert.equal(isExplicitPermission(request({ explicit: true, operation: "x", patterns: [] })), true);
  const desktopSite = request({
    operation: "browser_site:browsing:https://example.test",
    patterns: ["browsing:https://example.test"],
  });
  assert.equal(isExplicitPermission(desktopSite), false);
  assert.equal(canAutoApprove(request()), false);
  assert.equal(canAutoApprove(request({ explicit: true, operation: "bash:ls", permission: "bash", patterns: [] })), false);
  assert.equal(canAutoApprove(desktopSite), true);
  const combined = request({
    permission: "multiple",
    operation: "browser:navigate:https://example.test",
    patterns: [
      "browser:navigate:https://example.test",
      `browser_site:chrome-browsing:${thread}:https://example.test`,
    ],
  });
  assert.equal(canAutoApprove(combined), false);
  const replies: string[] = [];
  const consume = createAutoApprover({
    enabled: () => true,
    reply: async (_, id) => {
      replies.push(id);
    },
    failed: () => assert.fail("no reply should fail"),
  });
  const chrome = request();
  consume("a", [chrome, combined, desktopSite], []);
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(replies, [desktopSite.permissionId]);
});

test("a browser step carries a Chrome chip from its result, or from where it asked to open", () => {
  assert.equal(ranInChrome("browser_click", { tabID: "tab_1" }, { output: "{}", is_error: false, metadata: { browser: "chrome" } }), true);
  assert.equal(ranInChrome("browser_click", {}, { output: "{}", is_error: false, metadata: { browser: "haarness" } }), false);
  assert.equal(ranInChrome("browser_tabs_open", { browser: "chrome" }), true);
  assert.equal(ranInChrome("browser_tabs_open", { browser: "chrome" }, { output: "{}", is_error: false, metadata: { browser: "haarness" } }), false);
  assert.equal(ranInChrome("browser_tabs_open", { browser: "chrome" }, { output: "Chrome is not connected.", is_error: true }), true);
  assert.equal(ranInChrome("browser_tabs_open", { browser: "chrome" }, { output: "{}", is_error: false }), false);
  assert.equal(ranInChrome("browser_tabs_open", { browser: "haarness" }), false);
  assert.equal(ranInChrome("webfetch", { browser: "chrome" }, { output: "", is_error: false, metadata: { browser: "chrome" } }), false);
});
