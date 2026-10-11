import { test } from "node:test";
import assert from "node:assert/strict";
import {
  chromeConnection,
  chromeThreadTabs,
  defaultChromeSettings,
  editableChromeSettings,
  extensionsPage,
  setupBrowsers,
  webStoreLink,
  type ChromeHost,
  type ChromeSetupInfo,
} from "../src/chrome";

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
