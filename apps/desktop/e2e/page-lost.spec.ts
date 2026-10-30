import { test, expect, _electron as electron, type ElectronApplication, type Page } from "@playwright/test";
import { mkdtemp, readFile, realpath } from "node:fs/promises";
import { execFileSync } from "node:child_process";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { stopChatHome } from "./fixtures/chat-home";
import { isolatedEnv } from "./fixtures/isolated-env";

// A window whose page stops (a crash, or the system ending it) reloads on its
// own, and desktop.log beside the CLI's logs says what stopped; a page that
// stops again within a minute asks before it reloads.
let app: ElectronApplication, page: Page, root: string, logs: string;
let env: Record<string, string>;
const binary = resolve(process.env.HAARNESS_BIN ?? "../../target/debug/haarness");
test.describe.configure({ mode: "serial" });

const desktopLog = () => readFile(join(logs, "desktop.log"), "utf8").catch(() => "");
const pagePid = () => app.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows()[0].webContents.getOSProcessId());
const crashPage = () => app.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows()[0].webContents.forcefullyCrashRenderer());
/** Once the window shows a page again, the id of the process drawing it. */
const reloaded = () =>
  app.evaluate(({ BrowserWindow }) => {
    const contents = BrowserWindow.getAllWindows()[0]?.webContents;
    return contents && !contents.isCrashed() && !contents.isLoading() ? contents.getOSProcessId() : 0;
  });
/** Whether the window's page has drawn the app (Playwright's own handle on it ends with the crash). */
const drawn = () =>
  app.evaluate(({ BrowserWindow }) =>
    BrowserWindow.getAllWindows()[0].webContents.executeJavaScript("!!document.querySelector('.application')"),
  );
const boxes = () => app.evaluate(() => (globalThis as { boxes?: string[] }).boxes ?? []);

test.beforeAll(async () => {
  test.setTimeout(120000);
  root = await realpath(await mkdtemp(join(tmpdir(), "haarness-page-lost-")));
  env = isolatedEnv();
  Object.assign(env, {
    HAARNESS_BIN: binary,
    HAARNESS_HOME: join(root, "home"),
    HAARNESS_DATA_DIR: join(root, "data"),
    HAARNESS_DESKTOP_DATA: join(root, "desktop"),
    HAARNESS_TEST_PROVIDER: "1",
    HAARNESS_DESKTOP_TEST: "1",
  });
  delete env.ELECTRON_RUN_AS_NODE;
  delete env.ELECTRON_RENDERER_URL;
  logs = JSON.parse(execFileSync(binary, ["logs", "level", "--json"], { env, encoding: "utf8" })).logs;
  app = await electron.launch({ args: ["."], env, timeout: 60000 });
  page = await app.firstWindow();
  page.setDefaultTimeout(15000);
  await expect(page.locator(".application")).toBeVisible({ timeout: 60000 });
  // Message boxes are noted and answered "Reload window", never shown.
  await app.evaluate(({ dialog }) => {
    const record = globalThis as { boxes?: string[] };
    record.boxes = [];
    dialog.showMessageBox = (async (...args: unknown[]) => {
      record.boxes!.push((args.at(-1) as { message: string }).message);
      return { response: 0, checkboxChecked: false };
    }) as typeof dialog.showMessageBox;
  });
});
test.afterAll(async () => {
  if (app) {
    await app.evaluate(({ BrowserWindow }) => {
      for (const win of BrowserWindow.getAllWindows()) win.destroy();
    }).catch(() => {});
    await app.close().catch(() => {});
  }
  stopChatHome(binary, env);
  await test.info().attach("fixture", { body: root ?? "not started", contentType: "text/plain" });
});

test("a page that stops reloads on its own, and one that stops again soon asks", async () => {
  const first = await pagePid();
  await crashPage();
  await expect.poll(reloaded, { timeout: 30000 }).not.toBe(0);
  expect(await reloaded()).not.toBe(first);
  await expect.poll(drawn, { timeout: 30000 }).toBe(true);
  expect(await boxes()).toEqual([]);
  await expect.poll(desktopLog).toMatch(/^\d{4}-\d\d-\d\dT\S+ \d+ app  WARN haarness_desktop: a window's page stopped: [a-z-]+, exit code -?\d+$/m);

  // Within the minute it asks; "Reload window" brings the page back again.
  const second = await reloaded();
  await crashPage();
  await expect.poll(boxes).toEqual(["The window stopped responding."]);
  await expect.poll(reloaded, { timeout: 30000 }).not.toBe(0);
  expect(await reloaded()).not.toBe(second);
  await expect.poll(drawn, { timeout: 30000 }).toBe(true);
  expect((await desktopLog()).match(/a window's page stopped/g)).toHaveLength(2);
});

test("a Chromium helper that is ended is noted in desktop.log", async () => {
  const network = await app.evaluate(({ app }) =>
    app.getAppMetrics().find((metric) => metric.serviceName === "network.mojom.NetworkService")?.pid,
  );
  expect(network).toBeTruthy();
  process.kill(network!, "SIGKILL");
  await expect.poll(desktopLog).toMatch(/haarness_desktop: Network Service stopped: [a-z-]+, exit code -?\d+$/m);
});
