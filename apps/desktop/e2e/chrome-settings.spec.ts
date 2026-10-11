/**
 * Settings › Browser › Chrome, the "Use Chrome with Haarness" dialog, and the
 * explicit Chrome approval dock.
 *
 * Two kinds of test live here:
 * - Tests that run against any server: the rows render (disabled, with the
 *   reason, where the server lacks `browser-extension.v1`), search finds
 *   them, and a stubbed Desktop bridge drives status, setup and the site
 *   table without a real Chrome.
 * - Tests marked "[Chrome server]" drive a fake Haarness for Chrome host over
 *   the real extension endpoint (`/v1/browser/extension/host?version=1`, bearer
 *   token, no Origin). They skip themselves on a server without
 *   `browser-extension.v1` and run once the server side is integrated.
 */
import { settingsControls, settingsTitles } from "../src/renderer/settings-catalog";
import type { ChromeHost, ChromeSettings, ChromeSettingsView } from "../src/chrome";
import {
  test,
  expect,
  _electron as electron,
  type ElectronApplication,
  type Page,
} from "@playwright/test";
import { mkdtemp, mkdir, readFile, writeFile, realpath, rm, stat } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";
import { createServer, type Server } from "node:http";
import { WebSocket } from "ws";
import { Haarness, type PendingPermissionInfo, type Thread } from "@haarness/sdk";

let app: ElectronApplication, page: Page, temporary: string, site: Server, siteURL: string;
let env: Record<string, string>,
  endpoint: { url: string; token: string },
  client: Haarness,
  thread: Thread,
  connection: string,
  directory: string,
  chromeServer = false;
const errors: string[] = [];
const evidence = process.env.HAARNESS_BROWSER_EVIDENCE;
const binary = resolve(process.env.HAARNESS_BIN ?? "../../target/debug/haarness");
const chromeTitles = settingsControls
  .filter(([section, title]) => section === "browser" && /Chrome|agent tabs/.test(title))
  .map(([, title]) => title);
test.describe.configure({ mode: "serial" });

test.beforeAll(async () => {
  test.setTimeout(240000);
  temporary = await realpath(await mkdtemp(join(tmpdir(), "haarness-chrome-settings-")));
  env = Object.fromEntries(
    Object.entries(process.env).filter((x): x is [string, string] => x[1] !== undefined),
  );
  Object.assign(env, {
    HAARNESS_BIN: binary,
    HAARNESS_HOME: join(temporary, "home"),
    HAARNESS_DATA_DIR: join(temporary, "state"),
    HAARNESS_DESKTOP_DATA: join(temporary, "desktop"),
    HAARNESS_TEST_PROVIDER: "1",
    HAARNESS_DESKTOP_TEST: "1",
    // Chrome setup registers only with this test profile, and copies a
    // stand-in extension: the user's own Chrome is never touched.
    HAARNESS_BROWSER_MANIFEST_DIR: join(temporary, "chrome-profile", "NativeMessagingHosts"),
    HAARNESS_BROWSER_EXTENSION_DIR: join(temporary, "extension"),
  });
  delete env.ELECTRON_RUN_AS_NODE;
  await mkdir(env.HAARNESS_BROWSER_EXTENSION_DIR, { recursive: true });
  await writeFile(
    join(env.HAARNESS_BROWSER_EXTENSION_DIR, "manifest.json"),
    JSON.stringify({ manifest_version: 3, name: "Haarness", version: "0.1.1.1", version_name: "0.1.1-preview.1" }),
  );
  directory = join(temporary, "chrome-project");
  await mkdir(join(directory, ".haarness"), { recursive: true });
  await writeFile(
    join(directory, ".haarness/config.toml"),
    '[defaults]\nprovider="echo"\nmodel="echo-1"\n',
  );
  for (const args of [
    ["init", "-b", "main"],
    ["add", "."],
    ["-c", "user.name=Chrome Fixture", "-c", "user.email=fixture@example.test", "commit", "-m", "Fixture"],
  ])
    execFileSync("git", args, { cwd: directory, stdio: "ignore" });
  site = createServer((_, response) => {
    response.setHeader("Content-Type", "text/html");
    response.end("<title>Chrome fixture</title><h1>Chrome fixture</h1>");
  });
  await new Promise<void>((resolve) => site.listen(0, "127.0.0.1", resolve));
  siteURL = "http://127.0.0.1:" + (site.address() as { port: number }).port;
  if (evidence) await mkdir(evidence, { recursive: true });
  app = await electron.launch({ args: ["."], env, timeout: 120000 });
  page = await app.firstWindow();
  page.on("pageerror", (error) => errors.push(error.message));
  await app.evaluate(({ BrowserWindow }) => {
    const win = BrowserWindow.getAllWindows()[0];
    win.setContentSize(1500, 980);
    win.show();
  });
  await installBridgeStub();
  await app.evaluate(({ dialog }, directory) => {
    dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [directory] });
  }, directory);
  await page
    .getByRole("region", { name: "Project home" })
    .getByRole("button", { name: "Open project", exact: true })
    .click();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
  endpoint = JSON.parse(
    execFileSync(binary, ["--workspace", directory, "--trust-project", "desktop"], {
      env,
      input: '{"action":"connect"}',
      encoding: "utf8",
      timeout: 30000,
    }),
  );
  client = new Haarness({ baseUrl: endpoint.url, token: endpoint.token });
  chromeServer = (await client.info()).capabilities.includes("browser-extension.v1");
  connection = "local:" + directory;
  thread = await client.createThread({ title: "Chrome chat", provider: "echo", model: "echo-1" });
  await page.getByRole("button", { name: thread.title, exact: true }).click();
  await expect
    .poll(() => page.evaluate((id) => window.haarness.storage("session:" + id), connection))
    .toBe(thread.id);
});
test.afterAll(async () => {
  await app?.close().catch(() => {});
  client?.close();
  await new Promise<void>((resolve) => site?.close(() => resolve()));
  try {
    execFileSync(binary, ["--workspace", directory, "--trust-project", "serve", "--stop"], {
      env,
      stdio: "ignore",
      timeout: 30000,
    });
  } catch {}
  if (evidence) await writeFile(join(evidence, "fixture-path.txt"), temporary);
  else if (temporary) await rm(temporary, { recursive: true, force: true });
});

// ------------------------------------------------------------ bridge stub
/**
 * What the stubbed bridge answers while `active`: Desktop's IPC handlers are
 * wrapped in the main process, so the renderer runs unchanged against a
 * server that has (or lacks) Chrome support, connected browsers, Chrome
 * settings and pending explicit approvals that only this test controls.
 */
interface Fake {
  active: boolean;
  capable: boolean;
  /** Capabilities hidden from the renderer, such as server-side Auto. */
  strip: string[];
  hosts: ChromeHost[];
  threadTabs: Record<string, number>;
  view: ChromeSettingsView;
  saves: { chrome: ChromeSettings; scope: string }[];
  permissions: PendingPermissionInfo[];
  replies: { id: string; decision: string }[];
  setup: { ok: true; value: unknown } | { ok: false; error: string };
  setupCalls: number;
  reveals: number;
}
async function installBridgeStub() {
  await app.evaluate(({ ipcMain }) => {
    const handlers = (ipcMain as unknown as { _invokeHandlers: Map<string, (...args: any[]) => any> })
      ._invokeHandlers;
    const fake = () => (globalThis as any).chromeFake as Fake | undefined;
    // `strip` also applies to the real server, so a real Chrome host can
    // meet Desktop's own fallback Auto.
    const info = (value: any) => {
      const current = fake();
      if (!current || !value?.capabilities) return value;
      let capabilities = value.capabilities.filter((item: string) => !(current.strip ?? []).includes(item));
      if (current.active && current.capable && !capabilities.includes("browser-extension.v1"))
        capabilities = [...capabilities, "browser-extension.v1"];
      return { ...value, capabilities };
    };
    const connect = handlers.get("haarness:connect")!;
    handlers.set("haarness:connect", async (event, ...args) => {
      const result = await connect(event, ...args);
      if (result.ok && result.value?.info) result.value = { ...result.value, info: info(result.value.info) };
      return result;
    });
    const api = handlers.get("haarness:api")!;
    handlers.set("haarness:api", async (event, raw) => {
      const current = fake();
      const { method, args } = raw as { method: string; args: any[] };
      if (!current?.active) {
        const result = await api(event, raw);
        if (result.ok && method === "info") result.value = info(result.value);
        return result;
      }
      if (method === "listBrowserHosts") {
        const thread = args[0]?.thread as string | undefined;
        return {
          ok: true,
          value: {
            hosts: current.hosts.map((host) => ({ ...host, threadTabs: thread ? (current.threadTabs[thread] ?? 0) : 0 })),
            chrome: {
              enabled: current.view.settings.enabled,
              defaultBrowser: current.view.settings.defaultBrowser,
              everConnected: current.hosts.length > 0,
            },
          },
        };
      }
      if (method === "manageResource" && args[0] === "browser-settings" && args[1]?.chrome) {
        if (args[1].scope !== "global")
          return { ok: false, error: "Chrome settings apply to every project on this computer; save them globally." };
        current.saves.push({ chrome: args[1].chrome, scope: args[1].scope });
        current.view = { ...current.view, settings: args[1].chrome, globalSettings: args[1].chrome };
        return { ok: true, value: { data: {} } };
      }
      if (method === "replyPermission" && current.permissions.some((item) => item.permissionId === args[0])) {
        current.replies.push({ id: args[0], decision: args[1].decision });
        current.permissions = current.permissions.filter((item) => item.permissionId !== args[0]);
        return { ok: true, value: null };
      }
      const result = await api(event, raw);
      if (!result.ok) return result;
      if (method === "info") result.value = info(result.value);
      if (method === "listPermissions") result.value = [...result.value, ...current.permissions];
      if (method === "manageResource" && args[0] === "browser-settings" && current.capable)
        result.value = { ...result.value, data: { ...result.value.data, chrome: current.view } };
      return result;
    });
    // Without the stub, setup runs the real `haarness browser setup --json`
    // (into HAARNESS_BROWSER_MANIFEST_DIR, never the user's own Chrome).
    const setup = handlers.get("haarness:chromeSetup")!;
    handlers.set("haarness:chromeSetup", async (event, ...args) => {
      const current = fake();
      if (!current?.active) return setup(event, ...args);
      current.setupCalls++;
      await new Promise((resolve) => setTimeout(resolve, 300));
      return current.setup;
    });
    const reveal = handlers.get("haarness:chromeReveal")!;
    handlers.set("haarness:chromeReveal", async (event, ...args) => {
      const current = fake();
      if (!current?.active) return reveal(event, ...args);
      current.reveals++;
      return { ok: true, value: undefined };
    });
  });
}
const defaults = (): ChromeSettings => ({
  enabled: true,
  defaults: { browsing: "ask", downloads: "ask", uploads: "ask", scripts: "ask" },
  defaultBrowser: "auto",
  sites: [],
});
const setupReport = {
  hostName: "ai.haarness.browser",
  extensionId: "accdjhmcgljbadpeanajllmcembhkomn",
  webStoreUrl: null,
  executable: "/Applications/Haarness.app/Contents/Resources/bin/haarness",
  wrapper: "/Users/fixture/.local/share/haarness/browser/chrome/haarness-browser-host",
  removed: false,
  warnings: [],
  browsers: [
    { browser: "chrome", name: "Google Chrome", extensionsPage: "chrome://extensions", manifest: "/m.json", detected: true, registered: true, supported: true, note: null },
    { browser: "chromium", name: "Chromium", extensionsPage: "chrome://extensions", manifest: null, detected: true, registered: false, supported: false, note: "Snap's Chromium can't start programs outside its sandbox." },
  ],
  extension: { path: "/Users/fixture/.local/share/haarness/browser/chrome/Haarness for Chrome", version: "0.1.1-preview.1" },
};
const chromeHost = (overrides: Partial<ChromeHost> = {}): ChromeHost => ({
  kind: "chrome",
  browserId: crypto.randomUUID(),
  name: "Google Chrome",
  label: null,
  version: "154.0.8037.98",
  extensionVersion: "0.1.1",
  paused: false,
  stopped: false,
  connectedAt: new Date().toISOString(),
  threads: 0,
  tabs: 0,
  ...overrides,
});
async function stub(change: Partial<Fake>) {
  await app.evaluate((_, change) => {
    const current = (globalThis as any).chromeFake ?? {};
    (globalThis as any).chromeFake = { ...current, ...change };
  }, change as Record<string, unknown>);
}
async function fake(): Promise<Fake> {
  return app.evaluate(() => (globalThis as any).chromeFake);
}
async function startStub(change: Partial<Fake> = {}) {
  await stub({
    active: true,
    capable: true,
    strip: [],
    hosts: [],
    threadTabs: {},
    view: { settings: defaults(), globalSettings: null, projectRestrictions: [] },
    saves: [],
    permissions: [],
    replies: [],
    setup: { ok: true, value: setupReport },
    setupCalls: 0,
    reveals: 0,
    ...change,
  });
  await reload();
}
async function stopStub() {
  await stub({ active: false, strip: [] });
  await reload();
}
/** The renderer reads capabilities when it connects, so a reload applies them. */
async function reload() {
  await page.reload();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
}

// ---------------------------------------------------------------- helpers
const settings = () => page.getByRole("dialog", { name: "Settings", exact: true });
const setupDialog = () => page.getByRole("dialog", { name: "Use Chrome with Haarness", exact: true });
async function openSettings(section = "Browser") {
  if (!(await settings().count()))
    await page.getByRole("button", { name: "Settings", exact: true }).click();
  await settings().getByRole("textbox", { name: "Filter settings" }).fill("");
  await settings().getByRole("button", { name: section, exact: true }).click();
  if (section === "Browser")
    await expect(settings().getByRole("heading", { name: "Chrome", exact: true })).toBeVisible();
}
async function closeSettings() {
  if (await settings().count())
    await settings().getByRole("button", { name: "Close settings", exact: true }).click();
}
async function choose(name: string, value: string) {
  await settings().getByRole("combobox", { name, exact: true }).click();
  await page.getByRole("option", { name: value, exact: true }).click();
  await expect(settings().getByRole("combobox", { name, exact: true })).toContainText(value);
}
async function globalFind(query: string, title: string) {
  await app.evaluate(({ BrowserWindow }) => {
    BrowserWindow.getAllWindows()[0].webContents.send("haarness:event", { kind: "menu", data: "palette" });
  });
  await page.getByRole("combobox", { name: "Search Haarness", exact: true }).fill(query);
  await page.getByRole("option", { name: title, exact: true }).click();
}
async function noHorizontalOverflow() {
  expect(
    await settings()
      .locator(".settings-content")
      .evaluate((element) => element.scrollWidth <= element.clientWidth + 1),
  ).toBe(true);
}
async function snap(name: string, target: { screenshot(options: { path: string }): Promise<unknown> } = page) {
  await target.screenshot({ path: evidence ? join(evidence, name) : test.info().outputPath(name) });
}

// -------------------------------------------------------------- any server
test("Chrome rows always render and explain why they are disabled on a server without Chrome", async () => {
  await openSettings();
  const content = settings().locator(".settings-content");
  const headings = await content.locator(".browser-settings > h3").allTextContents();
  // After the General rows and before the agent's own site permissions.
  expect(headings.indexOf("Chrome")).toBe(headings.indexOf("General") + 1);
  expect(headings.indexOf("Agent site permissions")).toBe(headings.indexOf("Chrome") + 1);
  await expect(content).toContainText("Chrome settings apply to every project on this computer.");
  for (const title of chromeTitles)
    await expect(content.locator(`[data-setting="${title}"]`)).toBeVisible();
  const toggle = settings().getByRole("switch", { name: "Let agents use Chrome", exact: true });
  const setup = settings().getByRole("button", { name: "Set up Chrome…", exact: true });
  if (chromeServer) {
    await expect(toggle).toBeEnabled();
    await expect(setup).toBeEnabled();
  } else {
    await expect(content).toContainText(
      "This project’s Haarness server can’t use Chrome. Update Haarness, then reopen the project.",
    );
    await expect(content.locator('[data-setting="Chrome"]')).toContainText("Not available");
    await expect(toggle).toBeDisabled();
    await expect(setup).toBeDisabled();
    await expect(settings().getByRole("combobox", { name: "Browser for new agent tabs", exact: true })).toBeDisabled();
    await expect(settings().getByRole("combobox", { name: "Chrome default scripts", exact: true })).toBeDisabled();
    await expect(settings().getByRole("button", { name: "Add site to Chrome", exact: true })).toBeDisabled();
  }
  // The Chrome table shares the agent table's design, but never its names.
  await expect(settings().getByRole("textbox", { name: "Site origin" })).toHaveCount(1);
  await expect(settings().getByRole("button", { name: "Add site", exact: true })).toHaveCount(1);
  await expect(settings().getByRole("combobox", { name: "Default browsing", exact: true })).toHaveCount(1);
  await closeSettings();
  expect(errors).toEqual([]);
});

test("Chrome settings are found from Settings search and global search, by name and by alias", async () => {
  await openSettings();
  const filter = settings().getByRole("textbox", { name: "Filter settings" });
  const results = settings().getByRole("navigation", { name: "Settings search results" });
  await filter.fill("chrome");
  for (const title of chromeTitles)
    await expect(
      results.getByRole("button", { name: `${title} Settings › ${settingsTitles.browser}`, exact: true }),
    ).toBeVisible();
  for (const title of chromeTitles) {
    await filter.fill(title);
    await results
      .getByRole("button", { name: `${title} Settings › ${settingsTitles.browser}`, exact: true })
      .click();
    await expect(settings().locator(`[data-setting="${title}"]`)).toBeVisible();
    await expect(settings().locator(`[data-setting="${title}"]`)).toHaveClass(/setting-search-target/);
  }
  await globalFind("google edge brave", "Chrome");
  await expect(settings().locator('[data-setting="Chrome"]')).toHaveClass(/setting-search-target/);
  await globalFind("signed in accounts", "Chrome");
  await expect(settings().locator('[data-setting="Chrome"]')).toBeVisible();
  await closeSettings();
  expect(errors).toEqual([]);
});

// ------------------------------------------------- stubbed Desktop bridge
test("with Chrome support, the status follows connected browsers and setup walks through three live steps", async () => {
  await startStub({ setup: { ok: false, error: "Haarness could not write Chrome's native host manifest." } });
  await openSettings();
  const row = settings().locator('[data-setting="Chrome"]');
  await expect(row).toContainText("Not connected");
  await expect(settings().locator(".settings-content")).not.toContainText("can’t use Chrome");
  await settings().getByRole("button", { name: "Set up Chrome…", exact: true }).click();
  await expect(setupDialog()).toBeVisible();
  await expect(setupDialog().getByRole("alert")).toContainText("native host manifest");
  await stub({ setup: { ok: true, value: setupReport } });
  await setupDialog().getByRole("button", { name: "Try again", exact: true }).click();
  await expect(setupDialog()).toContainText("Haarness is registered with:");
  await expect(setupDialog().locator(".chrome-browsers")).toContainText("Google Chrome");
  await expect(setupDialog().locator(".chrome-browsers")).toContainText("Snap's Chromium");
  expect((await fake()).setupCalls).toBe(2);
  // Step 2: the page to open, Developer mode, the folder to load, pinning.
  const steps = setupDialog().locator(".chrome-substeps");
  await expect(steps.locator("code").first()).toHaveText("chrome://extensions");
  await expect(steps).toContainText("Turn on Developer mode, and keep it on.");
  await expect(steps.locator("code").nth(1)).toHaveText(setupReport.extension.path);
  await expect(steps.locator("kbd")).toHaveText(process.platform === "darwin" ? "⌘⇧G" : "Ctrl+L");
  await expect(steps).toContainText("Pin Haarness to the toolbar.");
  await expect(setupDialog().getByRole("button", { name: "Copy page address", exact: true })).toBeVisible();
  await expect(setupDialog().getByRole("button", { name: "Copy folder path", exact: true })).toBeVisible();
  await setupDialog()
    .getByRole("button", { name: /^(Reveal in Finder|Show in Explorer|Show in file manager)$/ })
    .click();
  await expect.poll(async () => (await fake()).reveals).toBe(1);
  await expect(setupDialog()).toContainText("Waiting for Chrome…");
  for (const [theme, width, height] of [
    ["Dark", 1500, 980],
    ["Light", 1000, 740],
  ] as const) {
    await page.evaluate((theme) => window.haarness.preferences({ theme }), theme.toLowerCase() as "dark" | "light");
    await app.evaluate(({ BrowserWindow }, size) => {
      BrowserWindow.getAllWindows()[0].setContentSize(size.width, size.height);
    }, { width, height });
    expect(
      await setupDialog().evaluate((dialog) => dialog.scrollWidth <= dialog.clientWidth + 1),
    ).toBe(true);
    await snap(`chrome-setup-${theme.toLowerCase()}-${width}.png`, setupDialog());
  }
  await app.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows()[0].setContentSize(1500, 980));
  // Step 3 polls the server: the extension connecting completes setup.
  await stub({ hosts: [chromeHost()] });
  await expect(setupDialog()).toContainText("Connected to Google Chrome");
  await expect(setupDialog()).toContainText("Try it: ask an agent to open a website in Chrome.");
  await expect(setupDialog()).toContainText("Haarness for Chrome is installed.");
  await snap("chrome-setup-connected.png", setupDialog());
  await setupDialog().getByRole("button", { name: "Done", exact: true }).click();
  await expect(setupDialog()).toHaveCount(0);
  await expect(row).toContainText("Connected · Google Chrome");
  await expect(settings().getByRole("button", { name: "Manage…", exact: true })).toBeVisible();
  // The status sits beside its button, at the weight of other row values,
  // and leaves the description its full measure.
  const status = row.locator(".chrome-status");
  expect(await status.evaluate((element) => getComputedStyle(element).fontWeight)).toBe("400");
  expect(
    await row.evaluate((row) => {
      const status = row.querySelector(".chrome-status")!.getBoundingClientRect();
      const button = row.querySelector("button")!.getBoundingClientRect();
      return button.left - status.right;
    }),
  ).toBeLessThanOrEqual(24);
  await expect(status.locator(".chrome-status-text")).toHaveAttribute("title", "Connected · Google Chrome");
  await row.scrollIntoViewIfNeeded();
  await snap("chrome-rows.png", settings());
  await stub({ hosts: [chromeHost(), chromeHost({ name: "Microsoft Edge" })] });
  await expect(row).toContainText("2 browsers connected");
  await stub({ hosts: [chromeHost({ paused: true })] });
  await expect(row).toContainText("Paused in Chrome");
  await stub({ hosts: [chromeHost({ stopped: true })] });
  await expect(row).toContainText("Stopped in Chrome");
  await closeSettings();
  expect(errors).toEqual([]);
});

test("Chrome settings save whole to the global layer whatever scope the pane shows", async () => {
  await startStub({ hosts: [chromeHost()] });
  await openSettings();
  await expect(settings().getByRole("combobox", { name: "Browser settings scope", exact: true })).toContainText(
    "This project",
  );
  const toggle = settings().getByRole("switch", { name: "Let agents use Chrome", exact: true });
  await expect(toggle).toBeChecked();
  await toggle.uncheck();
  await expect(toggle).not.toBeChecked();
  await expect.poll(async () => (await fake()).saves.at(-1)?.chrome.enabled).toBe(false);
  await choose("Browser for new agent tabs", "Chrome");
  await expect.poll(async () => (await fake()).saves.at(-1)?.chrome.defaultBrowser).toBe("chrome");
  await settings().getByRole("textbox", { name: "New Chrome site", exact: true }).fill("https://mail.example.test");
  await settings().getByRole("button", { name: "Add site to Chrome", exact: true }).click();
  await expect(settings().getByRole("textbox", { name: "New Chrome site", exact: true })).toHaveValue("");
  await choose("https://mail.example.test scripts in Chrome", "Block");
  await choose("Chrome default downloads", "Always allow");
  const saved = (await fake()).saves;
  expect(saved.every((save) => save.scope === "global")).toBe(true);
  expect(saved.at(-1)?.chrome).toEqual({
    enabled: false,
    defaultBrowser: "chrome",
    defaults: { browsing: "ask", downloads: "allow", uploads: "ask", scripts: "ask" },
    sites: [{ origin: "https://mail.example.test", browsing: "ask", downloads: "ask", uploads: "ask", scripts: "deny" }],
  });
  // The agent's own table is untouched by any of it.
  await expect(settings().getByRole("combobox", { name: "Default downloads", exact: true })).not.toContainText("Block");
  await snap("chrome-settings-section.png", settings().locator('[data-setting="Chrome site permissions"]'));
  for (const [theme, width, height] of [
    ["Dark", 1500, 980],
    ["Light", 1500, 980],
    ["Dark", 1000, 740],
  ] as const) {
    await openSettings("General");
    await choose("Appearance", theme);
    await app.evaluate(({ BrowserWindow }, size) => {
      BrowserWindow.getAllWindows()[0].setContentSize(size.width, size.height);
    }, { width, height });
    await openSettings();
    await settings().locator('[data-setting="Chrome site permissions"]').scrollIntoViewIfNeeded();
    await expect(
      settings().getByRole("combobox", { name: "https://mail.example.test scripts in Chrome", exact: true }),
    ).toBeVisible();
    await noHorizontalOverflow();
    await snap(`chrome-settings-${theme.toLowerCase()}-${width}.png`);
  }
  await app.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows()[0].setContentSize(1500, 980));
  await settings()
    .getByRole("button", { name: "Remove Chrome site https://mail.example.test", exact: true })
    .click();
  await expect.poll(async () => (await fake()).saves.at(-1)?.chrome.sites.length).toBe(0);
  // A project that tightened Chrome says so above the rows.
  await stub({
    view: {
      ...(await fake()).view,
      projectRestrictions: ["Chrome is turned off for this project"],
    },
  });
  await closeSettings();
  await openSettings();
  await expect(settings().locator(".settings-content")).toContainText("Restricted by this project’s settings");
  await expect(settings().locator(".settings-content")).toContainText("Chrome is turned off for this project");
  await closeSettings();
  expect(errors).toEqual([]);
});

test("the Chrome section and setup read right to left in Arabic, with paths kept left to right", async () => {
  await startStub({ hosts: [] });
  await openSettings();
  await page.evaluate(() => window.haarness.preferences({ language: "ar" }));
  await expect.poll(() => page.evaluate(() => document.documentElement.dir)).toBe("rtl");
  // The dialog's own name is Arabic now; its class is not.
  const pane = page.locator("dialog.settings-dialog");
  const row = pane.locator('[data-setting="Chrome"]');
  await expect(row).toContainText("غير متصل");
  await row.scrollIntoViewIfNeeded();
  expect(
    await pane
      .locator(".settings-content")
      .evaluate((element) => element.scrollWidth <= element.clientWidth + 1),
  ).toBe(true);
  await snap("chrome-settings-ar.png", pane);
  await row.getByRole("button").click();
  const dialog = page.getByRole("dialog", { name: "استخدام Chrome مع Haarness", exact: true });
  await expect(dialog.locator(".chrome-substeps code").nth(1)).toHaveText(setupReport.extension.path);
  expect(
    await dialog
      .locator(".chrome-substeps code")
      .evaluateAll((codes) => codes.map((code) => getComputedStyle(code).direction)),
  ).toEqual(["ltr", "ltr"]);
  expect(await dialog.evaluate((element) => element.scrollWidth <= element.clientWidth + 1)).toBe(true);
  // The setup command's English note keeps its full stop at its own end.
  expect(
    await dialog.locator(".chrome-browsers small").evaluate((note) => getComputedStyle(note).unicodeBidi),
  ).toBe("plaintext");
  await snap("chrome-setup-ar.png", dialog);
  await dialog.getByRole("button", { name: "إغلاق", exact: true }).click();
  await page.evaluate(() => window.haarness.preferences({ language: "en" }));
  await expect.poll(() => page.evaluate(() => document.documentElement.dir)).toBe("ltr");
  await closeSettings();
  expect(errors).toEqual([]);
});

test("the browser panel says when the chat also has tabs in Chrome", async () => {
  await startStub({ hosts: [chromeHost()], threadTabs: { [thread.id]: 2 } });
  await app.evaluate(({ BrowserWindow }) =>
    BrowserWindow.getAllWindows()[0].webContents.send("haarness:event", { kind: "menu", data: "browser" }),
  );
  const panel = page.getByRole("region", { name: "Web browser", exact: true });
  await expect(panel).toBeVisible();
  await expect(panel.locator(".browser-status")).toContainText("This chat also uses Chrome · 2 tabs");
  await stub({ threadTabs: { [thread.id]: 1 } });
  await expect(panel.locator(".browser-status")).toContainText("This chat also uses Chrome · 1 tab");
  await stub({ threadTabs: {} });
  await expect(panel.locator(".browser-status")).toHaveCount(0);
  await app.evaluate(({ BrowserWindow }) =>
    BrowserWindow.getAllWindows()[0].webContents.send("haarness:event", { kind: "menu", data: "browser" }),
  );
  expect(errors).toEqual([]);
});

test("an explicit Chrome request docks as Allow in Chrome and Desktop's own Auto never answers it", async () => {
  const origin = "https://mail.example.test";
  const base = {
    threadId: thread.id,
    rootThreadId: thread.id,
    turnId: null,
    createdAt: new Date().toISOString(),
  };
  const explicit = {
    ...base,
    permissionId: crypto.randomUUID(),
    permission: "multiple",
    operation: `browser_site:chrome-browsing:${thread.id}:${origin}`,
    patterns: [
      `browser_site:chrome-browsing:${thread.id}:${origin}`,
      `browser_site:chrome-browsing:${thread.id}:https://www.mail.example.test`,
      `browser_site:chrome-scripts:${thread.id}:${origin}`,
    ],
    alwaysPatterns: [`browser_site:chrome-browsing:${thread.id}:${origin}`],
    allowAlways: true,
    detail: { tabID: "tab_" + crypto.randomUUID(), expression: "document.title", __browser: { lease: "x" } },
    explicit: true,
  } as PendingPermissionInfo;
  const ordinary = {
    ...base,
    permissionId: crypto.randomUUID(),
    permission: "bash",
    operation: "bash:printf chrome",
    patterns: ["printf chrome"],
    alwaysPatterns: ["bash:printf *"],
    detail: { command: "printf chrome" },
  } as PendingPermissionInfo;
  // Desktop's fallback Auto (for servers that cannot answer for a chat) is
  // on for this chat; it must answer the ordinary request and leave Chrome's.
  await page.evaluate(
    ({ connection, thread }) => window.haarness.storage("approval-modes:v1", { [connection]: { [thread]: true } }),
    { connection, thread: thread.id },
  );
  await startStub({ strip: ["approval-mode.v1"], permissions: [explicit, ordinary] });
  await expect
    .poll(async () => (await fake()).replies)
    .toEqual([{ id: ordinary.permissionId, decision: "once" }]);
  const dock = page.getByRole("alert", { name: "Permission request" });
  await expect(dock.locator(".prompt-title")).toHaveText("Allow in Chrome");
  await expect(dock.locator(".chrome-approval li")).toHaveText([
    `Use ${origin} in Chrome`,
    `Run scripts on ${origin} in Chrome`,
  ]);
  await expect(dock.locator(".prompt-detail")).toContainText("document.title");
  await expect(dock.locator(".prompt-detail")).not.toContainText("lease");
  await expect(dock.getByRole("button")).toHaveText(["Deny", "Allow once", "Allow in this chat"]);
  await expect(dock.getByRole("button", { name: /Always|Auto/ })).toHaveCount(0);
  await expect(dock.getByRole("button", { name: "Allow in this chat", exact: true })).toHaveClass(/primary/);
  await snap("chrome-approval-dock.png", dock);
  expect((await fake()).replies).toHaveLength(1);
  await dock.getByRole("button", { name: "Allow in this chat", exact: true }).click();
  await expect.poll(async () => (await fake()).replies.at(-1)).toEqual({
    id: explicit.permissionId,
    decision: "always",
  });
  await expect(dock).toHaveCount(0);
  await page.evaluate(() => window.haarness.storage("approval-modes:v1", {}));
  await stopStub();
  expect(errors).toEqual([]);
});

// ------------------------------------------------------- [Chrome server]
/** A fake Haarness for Chrome host on the real extension endpoint. */
async function fakeExtension(name = "Google Chrome") {
  const url = endpoint.url.replace(/^http/, "ws") + "/v1/browser/extension/host?version=1";
  // `ws` sends no Origin header unless asked to, like Chrome's native host.
  const socket = new WebSocket(url, { headers: { Authorization: "Bearer " + endpoint.token } });
  const tabs = new Map<string, { id: string; url: string; title: string; loading: boolean; canGoBack: boolean; canGoForward: boolean; generation: number }[]>();
  const commands: { thread: string; title?: string; command: { action: { type: string; url?: string; tabID?: string; script?: string }; inspect?: boolean } }[] = [];
  const send = (message: unknown) => socket.send(JSON.stringify(message));
  const attached = new Promise<void>((resolve, reject) => {
    socket.once("error", reject);
    socket.on("message", (data) => {
      const message = JSON.parse(String(data));
      if (message.type === "attached") {
        send({
          type: "hello",
          browser: { id: crypto.randomUUID(), kind: "chrome", name, label: null, version: "154.0.8037.98" },
          extension: { version: "0.1.1" },
          paused: false,
          stopped: false,
          focusedAt: Date.now(),
        });
        resolve();
      }
      if (message.type !== "command") return;
      commands.push(message);
      const { id, thread, command } = message;
      const action = command.action;
      const list = tabs.get(thread) ?? [];
      const current = list.find((tab) => tab.id === action.tabID);
      if (command.inspect) {
        // Like the extension: document origins, and a new address's www twin.
        const target = action.url ?? current?.url;
        const resources: string[] = [];
        if (target && /^https?:/.test(target)) {
          const url = new URL(target);
          resources.push(url.origin);
          if (action.url && /[a-z]\.[a-z]/i.test(url.hostname)) {
            url.hostname = url.hostname.startsWith("www.") ? url.hostname.slice(4) : "www." + url.hostname;
            resources.push(url.origin);
          }
        }
        send({
          type: "result",
          id,
          outcome: {
            type: "success",
            result: { value: { key: action.type + ":" + (target ?? "inventory"), resources }, files: [] },
          },
        });
        return;
      }
      if (action.type === "evaluate" && current) {
        send({ type: "result", id, outcome: { type: "success", result: { value: current.title, files: [] } } });
        return;
      }
      if (action.type === "tabs.open") {
        const tab = {
          id: "tab_" + crypto.randomUUID(),
          url: action.url ?? "about:blank",
          title: "Chrome fixture",
          loading: false,
          canGoBack: false,
          canGoForward: false,
          generation: 1,
        };
        list.push(tab);
        tabs.set(thread, list);
        // The inventory reaches the server before the result.
        send({ type: "state", thread, state: { tabs: list, focusedTabID: tab.id } });
        send({ type: "result", id, outcome: { type: "success", result: { value: tab, files: [] } } });
        return;
      }
      if (action.type === "tabs.list") {
        send({
          type: "result",
          id,
          outcome: { type: "success", result: { value: { tabs: list, focusedTabID: list.at(-1)?.id ?? null }, files: [] } },
        });
        return;
      }
      send({ type: "result", id, outcome: { type: "failure", code: "unsupported", message: "Not in this fake." } });
    });
  });
  await attached;
  return {
    socket,
    commands,
    /** Pause or stop it, as the user does from Chrome's toolbar. */
    status: (change: { paused?: boolean; stopped?: boolean }) =>
      send({ type: "status", paused: false, stopped: false, focusedAt: Date.now(), ...change }),
    close: () => socket.close(),
  };
}
async function chromeTool(name: string, args: object, chat = thread) {
  const sdk = new Haarness({ baseUrl: endpoint.url, token: endpoint.token });
  try {
    return await sdk.callBrowserTool(chat.id, { name: "browser_" + name, arguments: args as any }, { timeoutMs: 90_000 });
  } finally {
    sdk.close();
  }
}

test("[Chrome server] a connected Chrome shows in Settings, and Allow in this chat lets later calls through", async () => {
  test.skip(!chromeServer, "Needs a server with browser-extension.v1; runs once the server side is integrated.");
  test.setTimeout(180000);
  await openSettings();
  const row = settings().locator('[data-setting="Chrome"]');
  await expect(row).toContainText("Not connected");
  const extension = await fakeExtension();
  try {
    await expect(row).toContainText("Connected · Google Chrome");
    await expect(settings().getByRole("button", { name: "Manage…", exact: true })).toBeVisible();
    await closeSettings();
    // Chrome's defaults ask first, and only a person answers.
    const first = chromeTool("tabs_open", { url: siteURL + "/first", browser: "chrome" });
    const dock = page.getByRole("alert", { name: "Permission request" });
    await expect(dock.locator(".prompt-title")).toHaveText("Allow in Chrome");
    await expect(dock.locator(".chrome-approval")).toContainText(`Use ${siteURL} in Chrome`);
    await expect(dock.getByRole("button", { name: /Always|Auto/ })).toHaveCount(0);
    const pending = await client.listPermissions();
    expect(pending).toHaveLength(1);
    expect((pending[0] as { explicit?: boolean }).explicit).toBe(true);
    await dock.getByRole("button", { name: "Allow in this chat", exact: true }).click();
    const opened = await first;
    expect(opened.isError).toBe(false);
    expect((opened.metadata as { browser?: string }).browser).toBe("chrome");
    // The same site in the same chat needs no second answer.
    const second = await chromeTool("tabs_open", { url: siteURL + "/second", browser: "chrome" });
    expect(second.isError).toBe(false);
    expect(await client.listPermissions()).toHaveLength(0);
    expect(extension.commands.filter((item) => !item.command.inspect).map((item) => item.thread)).toEqual([
      thread.id,
      thread.id,
    ]);
    // A model-made call to that site shows the Chrome chip in the transcript.
    await page.getByRole("textbox", { name: "Message", exact: true }).fill(
      `!tool browser_tabs_open {"url":"${siteURL}/third","browser":"chrome"}`,
    );
    await page.keyboard.press("Enter");
    const step = page.locator(".tool-call", { hasText: "Opened a tab" }).last();
    await expect(step.locator(".tool-chip")).toHaveText("Chrome");
    await snap("chrome-transcript-chip.png");
    // The browser panel counts the chat's Chrome tabs.
    await app.evaluate(({ BrowserWindow }) =>
      BrowserWindow.getAllWindows()[0].webContents.send("haarness:event", { kind: "menu", data: "browser" }),
    );
    await expect(page.getByRole("region", { name: "Web browser", exact: true }).locator(".browser-status")).toContainText(
      "This chat also uses Chrome · 3 tabs",
    );
    await snap("chrome-browser-panel.png");
    await app.evaluate(({ BrowserWindow }) =>
      BrowserWindow.getAllWindows()[0].webContents.send("haarness:event", { kind: "menu", data: "browser" }),
    );
  } finally {
    extension.close();
  }
  await openSettings();
  await expect(row).toContainText("Not connected");
  await closeSettings();
  expect(errors).toEqual([]);
});

test("[Chrome server] the Chrome site table persists globally, even from the project scope", async () => {
  test.skip(!chromeServer, "Needs a server with browser-extension.v1; runs once the server side is integrated.");
  await openSettings();
  await expect(settings().getByRole("combobox", { name: "Browser settings scope", exact: true })).toContainText(
    "This project",
  );
  await settings().getByRole("textbox", { name: "New Chrome site", exact: true }).fill("https://example.com");
  await settings().getByRole("button", { name: "Add site to Chrome", exact: true }).click();
  await expect(
    settings().getByRole("combobox", { name: "https://example.com scripts in Chrome", exact: true }),
  ).toBeVisible();
  await choose("https://example.com scripts in Chrome", "Block");
  const read = async (scope: string) =>
    (await client.managementView("browser-settings", { action: "get", scope })).data as unknown as {
      chrome: ChromeSettingsView;
    };
  await expect.poll(async () => (await read("global")).chrome.globalSettings?.sites).toEqual([
    { origin: "https://example.com", browsing: "ask", downloads: "ask", uploads: "ask", scripts: "deny" },
  ]);
  expect((await read("project")).chrome.settings.sites[0]?.scripts).toBe("deny");
  await settings().getByRole("button", { name: "Remove Chrome site https://example.com", exact: true }).click();
  await expect.poll(async () => (await read("global")).chrome.globalSettings?.sites).toEqual([]);
  await closeSettings();
  expect(errors).toEqual([]);
});

test("[Chrome server] the Chrome row follows the real host: paused, stopped, back, and two browsers", async () => {
  test.skip(!chromeServer, "Needs a server with browser-extension.v1; runs once the server side is integrated.");
  test.setTimeout(180000);
  // A connected Chrome is re-read every 10 s.
  const later = { timeout: 25_000 };
  await openSettings();
  const row = settings().locator('[data-setting="Chrome"]');
  const status = row.locator(".chrome-status");
  await expect(row).toContainText("Not connected");
  const extension = await fakeExtension();
  try {
    await expect(row).toContainText("Connected · Google Chrome");
    await expect(status).toHaveClass(/live/);
    extension.status({ paused: true });
    await expect(row).toContainText("Paused in Chrome", later);
    await expect(status).toHaveClass(/paused/);
    // The server refuses before anything reaches a paused Chrome.
    const refused = await chromeTool("tabs_open", { url: siteURL + "/paused", browser: "chrome" }).catch(
      (error: unknown) => ({ isError: true, output: String(error) }),
    );
    expect(refused.isError).toBe(true);
    expect(refused.output).toContain("The user paused Haarness in Chrome.");
    extension.status({ stopped: true });
    await expect(row).toContainText("Stopped in Chrome", later);
    extension.status({});
    await expect(row).toContainText("Connected · Google Chrome", later);
    await expect(status).toHaveClass(/live/);
    const edge = await fakeExtension("Microsoft Edge");
    try {
      await expect(row).toContainText("2 browsers connected", later);
    } finally {
      edge.close();
    }
    await expect(row).toContainText("Connected · Google Chrome", later);
    expect(extension.commands).toEqual([]);
  } finally {
    extension.close();
  }
  await expect(row).toContainText("Not connected", later);
  await expect(settings().getByRole("button", { name: "Set up Chrome…", exact: true })).toBeVisible();
  await closeSettings();
  expect(errors).toEqual([]);
});

test("[Chrome server] Chrome rows write browser.chrome to the global file, and a project can only tighten them", async () => {
  test.skip(!chromeServer, "Needs a server with browser-extension.v1; runs once the server side is integrated.");
  const globalFile = join(temporary, "state", "config", "haarness.json");
  const projectFile = join(directory, ".haarness", "haarness.json");
  const readGlobal = async () =>
    JSON.parse(await readFile(globalFile, "utf8").catch(() => "{}")) as { browser?: { chrome?: ChromeSettings } };
  await openSettings();
  await expect(settings().getByRole("combobox", { name: "Browser settings scope", exact: true })).toContainText(
    "This project",
  );
  await choose("Browser for new agent tabs", "Chrome");
  await choose("Chrome default uploads", "Block");
  await expect.poll(async () => (await readGlobal()).browser?.chrome).toEqual({
    enabled: true,
    defaults: { browsing: "ask", downloads: "ask", uploads: "deny", scripts: "ask" },
    defaultBrowser: "chrome",
    sites: [],
  });
  // Nothing about Chrome lands in the project, whatever scope the pane shows.
  expect(await readFile(projectFile, "utf8").catch(() => "")).not.toContain("chrome");
  // A project that turns scripts off says so above the rows, and the rows
  // keep showing (and saving) the global values.
  await writeFile(projectFile, JSON.stringify({ browser: { chrome: { defaults: { scripts: "deny" } } } }));
  try {
    await closeSettings();
    await openSettings();
    const notice = settings().locator(".chrome-note", { hasText: "Restricted by this project’s settings" });
    await expect(notice).toBeVisible();
    await expect(notice.locator(".chrome-restriction")).toHaveText(["Scripts: Block by default"]);
    await expect(settings().getByRole("combobox", { name: "Chrome default scripts", exact: true })).toContainText(
      "Requires approval",
    );
    await settings().locator('[data-setting="Chrome"]').scrollIntoViewIfNeeded();
    await snap("chrome-settings-restricted.png", settings());
    const view = (await client.managementView("browser-settings", { action: "get", scope: "project" }))
      .data as unknown as { chrome: ChromeSettingsView };
    expect(view.chrome.settings.defaults.scripts).toBe("deny");
    expect(view.chrome.globalSettings?.defaults.scripts).toBe("ask");
  } finally {
    await rm(projectFile, { force: true });
  }
  // Put the global layer back for the tests that follow (re-read first: the
  // project file changed underneath the pane's revision).
  await closeSettings();
  await openSettings();
  await expect(settings().locator(".chrome-note", { hasText: "Restricted" })).toHaveCount(0);
  await choose("Chrome default uploads", "Requires approval");
  await choose("Browser for new agent tabs", "Automatic");
  await expect.poll(async () => (await readGlobal()).browser?.chrome?.defaultBrowser).toBe("auto");
  await closeSettings();
  expect(errors).toEqual([]);
});

test("[Chrome server] a real Chrome gate docks as Allow in Chrome, and neither server Auto nor Desktop's Auto answers it", async () => {
  test.skip(!chromeServer, "Needs a server with browser-extension.v1; runs once the server side is integrated.");
  test.setTimeout(180000);
  const origin = "https://mail.example.test";
  const chat = await client.createThread({ title: "Chrome Auto chat", provider: "echo", model: "echo-1" });
  await page.getByRole("button", { name: chat.title, exact: true }).click();
  await expect
    .poll(() => page.evaluate((id) => window.haarness.storage("session:" + id), connection))
    .toBe(chat.id);
  // The server answers for this chat in Auto, and Desktop's own fallback
  // would too.
  await client.setApprovalMode(chat.id, { mode: "auto" });
  await page.evaluate(
    ({ connection, chat }) => window.haarness.storage("approval-modes:v1", { [connection]: { [chat]: true } }),
    { connection, chat: chat.id },
  );
  const extension = await fakeExtension();
  const dock = page.getByRole("alert", { name: "Permission request" });
  try {
    const opening = chromeTool("tabs_open", { url: origin + "/inbox", browser: "chrome" }, chat);
    await expect(dock.locator(".prompt-title")).toHaveText("Allow in Chrome");
    // The site and its www twin are one line.
    await expect(dock.locator(".chrome-approval li")).toHaveText([`Use ${origin} in Chrome`]);
    await expect(dock.getByRole("button")).toHaveText(["Deny", "Allow once", "Allow in this chat"]);
    await expect(dock.getByRole("button", { name: "Allow in this chat", exact: true })).toHaveClass(/primary/);
    await page.waitForTimeout(3000);
    const waiting = await client.listPermissions();
    expect(waiting).toHaveLength(1);
    expect((waiting[0] as { explicit?: boolean }).explicit).toBe(true);
    expect((await client.getApprovalMode(chat.id)).mode).toBe("auto");
    // Desktop's fallback Auto (for servers that cannot answer for a chat)
    // leaves it waiting too.
    await stub({ strip: ["approval-mode.v1"] });
    await reload();
    await expect(dock.locator(".prompt-title")).toHaveText("Allow in Chrome");
    await page.waitForTimeout(3000);
    expect(await client.listPermissions()).toHaveLength(1);
    await dock.getByRole("button", { name: "Allow once", exact: true }).click();
    const opened = await opening;
    expect(opened.isError).toBe(false);
    const tab = JSON.parse(opened.output) as { id: string };
    // Scripts ask on their own, next to a site that was allowed only once.
    const evaluating = chromeTool("evaluate", { tabID: tab.id, script: "document.title" }, chat);
    await expect(dock.locator(".chrome-approval li")).toHaveText([
      `Use ${origin} in Chrome`,
      `Run scripts on ${origin} in Chrome`,
    ]);
    await expect(dock.locator(".prompt-detail")).toContainText("document.title");
    await expect(dock.locator(".prompt-detail")).not.toContainText("__browser");
    await snap("chrome-approval-real.png", dock);
    await dock.getByRole("button", { name: "Allow in this chat", exact: true }).click();
    expect((await evaluating).isError).toBe(false);
    // Allowed in this chat: the next script runs without asking.
    expect((await chromeTool("evaluate", { tabID: tab.id, script: "1 + 1" }, chat)).isError).toBe(false);
    expect(await client.listPermissions()).toHaveLength(0);
    expect(extension.commands.filter((item) => !item.command.inspect).map((item) => item.command.action.type)).toEqual([
      "tabs.open",
      "evaluate",
      "evaluate",
    ]);
  } finally {
    extension.close();
    await client.setApprovalMode(chat.id, { mode: "review" }).catch(() => {});
    await page.evaluate(() => window.haarness.storage("approval-modes:v1", {}));
    await stopStub();
  }
  await page.getByRole("button", { name: thread.title, exact: true }).click();
  expect(errors).toEqual([]);
});

test("[real setup] the setup dialog runs haarness browser setup into a test profile, never the user's Chrome", async () => {
  test.skip(!chromeServer, "Needs a server with browser-extension.v1; runs once the server side is integrated.");
  test.setTimeout(180000);
  const manifestDir = env.HAARNESS_BROWSER_MANIFEST_DIR;
  const manifest = join(manifestDir, "ai.haarness.browser.json");
  const before = await realRegistrations();
  // Reveal records instead of opening Finder.
  await app.evaluate(({ shell }) => {
    (globalThis as any).revealed = [];
    shell.showItemInFolder = (path: string) => void (globalThis as any).revealed.push(path);
  });
  // A test run without its own manifest folder refuses to register at all.
  await app.evaluate(() => {
    (globalThis as any).manifestDir = process.env.HAARNESS_BROWSER_MANIFEST_DIR;
    delete process.env.HAARNESS_BROWSER_MANIFEST_DIR;
  });
  await openSettings();
  await settings().getByRole("button", { name: "Set up Chrome…", exact: true }).click();
  await expect(setupDialog().getByRole("alert")).toContainText("HAARNESS_BROWSER_MANIFEST_DIR");
  await app.evaluate(() => {
    process.env.HAARNESS_BROWSER_MANIFEST_DIR = (globalThis as any).manifestDir;
  });
  await setupDialog().getByRole("button", { name: "Try again", exact: true }).click();
  // Step 1 ran the real binary: the test profile is registered.
  await expect(setupDialog().locator(".chrome-browsers")).toContainText(manifestDir);
  await expect(setupDialog().locator(".chrome-warning")).toHaveCount(0);
  const written = JSON.parse(await readFile(manifest, "utf8"));
  const data = join(temporary, "state", "data", "browser", "chrome");
  expect(written).toMatchObject({
    name: "ai.haarness.browser",
    path: join(data, "haarness-browser-host"),
    type: "stdio",
    allowed_origins: ["chrome-extension://accdjhmcgljbadpeanajllmcembhkomn/"],
  });
  expect(await readFile(join(data, "haarness-browser-host"), "utf8")).toContain(`'${binary}' browser native-host`);
  // Step 2 names the copied extension, and reveals only that folder.
  const folder = join(data, "Haarness for Chrome");
  await expect(setupDialog().locator(".chrome-substeps code").nth(1)).toHaveText(folder);
  expect(JSON.parse(await readFile(join(folder, "manifest.json"), "utf8")).version_name).toBe("0.1.1-preview.1");
  await setupDialog()
    .getByRole("button", { name: /^(Reveal in Finder|Show in Explorer|Show in file manager)$/ })
    .click();
  await expect.poll(() => app.evaluate(() => (globalThis as any).revealed)).toEqual([folder]);
  await expect(setupDialog().getByRole("alert")).toHaveCount(0);
  // Step 3: the extension connecting to this project finishes setup.
  const extension = await fakeExtension();
  try {
    await expect(setupDialog()).toContainText("Connected to Google Chrome");
    await setupDialog().getByRole("button", { name: "Done", exact: true }).click();
    await expect(settings().locator('[data-setting="Chrome"]')).toContainText("Connected · Google Chrome");
  } finally {
    extension.close();
  }
  // The CLI agrees, reading the same folder.
  const status = JSON.parse(
    execFileSync(binary, ["browser", "status", "--json"], { env, encoding: "utf8", timeout: 30000 }),
  ) as { browsers: { name: string; registered: boolean }[] };
  expect(status.browsers).toEqual([expect.objectContaining({ name: manifestDir, registered: true })]);
  expect(await realRegistrations()).toEqual(before);
  await closeSettings();
  expect(errors).toEqual([]);
});

/** Haarness's host manifest in every real browser profile on this computer:
 * whether it exists and when it last changed. Tests must leave these alone. */
async function realRegistrations() {
  const roots =
    process.platform === "darwin"
      ? [
          "Google/Chrome",
          "Google/Chrome Beta",
          "Google/Chrome Canary",
          "Google/Chrome for Testing",
          "Chromium",
          "Microsoft Edge",
          "BraveSoftware/Brave-Browser",
          "Vivaldi",
        ].map((name) => join(homedir(), "Library/Application Support", name))
      : [
          "google-chrome",
          "google-chrome-beta",
          "google-chrome-unstable",
          "chromium",
          "microsoft-edge",
          "BraveSoftware/Brave-Browser",
          "vivaldi",
        ].map((name) => join(homedir(), ".config", name));
  return Promise.all(
    roots.map(async (root) => {
      const file = join(root, "NativeMessagingHosts", "ai.haarness.browser.json");
      const info = await stat(file).catch(() => undefined);
      return { file, modified: info?.mtimeMs ?? null };
    }),
  );
}
