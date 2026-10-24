import { test, expect, _electron as electron, type ElectronApplication, type Page } from "@playwright/test";
import { mkdtemp, mkdir, realpath, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";
import { stopChatHome } from "./fixtures/chat-home";

// The update confirmation shows the feed's release notes, rendered and
// sanitized, in a scroll area of its own, and stays as it was when the feed
// has none.
test.describe.configure({ mode: "serial" });

const binary = resolve(process.env.HAARNESS_BIN ?? "../../target/debug/haarness");
const shots = process.env.HAARNESS_DIAGNOSTICS_SCREENSHOTS;
let app: ElectronApplication;
let page: Page;
let temporary: string;
let project: string;
let env: Record<string, string>;
const errors: string[] = [];
const settings = () => page.getByRole("dialog", { name: "Settings", exact: true });

async function theme(value: "light" | "dark") {
  await app.evaluate(({ nativeTheme }, value) => {
    nativeTheme.themeSource = value;
  }, value);
  await page.emulateMedia({ colorScheme: value });
  await expect.poll(() => page.evaluate(() => matchMedia("(prefers-color-scheme: dark)").matches)).toBe(value === "dark");
  await page.waitForTimeout(300);
}
async function shoot(name: string, target: { screenshot: Page["screenshot"] } = page) {
  if (!shots) return;
  await mkdir(shots, { recursive: true });
  for (const value of ["light", "dark"] as const) {
    await theme(value);
    await target.screenshot({ path: join(shots, `${name}-${value}.png`) });
  }
}

test.beforeAll(async () => {
  test.setTimeout(240_000);
  temporary = await realpath(await mkdtemp(join(process.env.HAARNESS_E2E_DIR ?? tmpdir(), "haarness-update-notes-")));
  project = join(temporary, "project");
  await mkdir(join(project, ".haarness"), { recursive: true });
  await writeFile(join(project, ".haarness/config.toml"), '[defaults]\nprovider="echo"\nmodel="echo-1"\n');
  env = {
    ...Object.fromEntries(Object.entries(process.env).filter((x): x is [string, string] => x[1] !== undefined)),
    HAARNESS_BIN: binary,
    HAARNESS_HOME: join(temporary, "home"),
    HAARNESS_DATA_DIR: join(temporary, "state"),
    HAARNESS_DESKTOP_DATA: join(temporary, "desktop"),
    HAARNESS_TEST_PROVIDER: "1",
    HAARNESS_DESKTOP_TEST: "1",
    // A feed is configured; the test's own updater answers for it.
    HAARNESS_UPDATE_URL: "https://updates.example.test/haarness/",
  };
  delete env.ELECTRON_RUN_AS_NODE;
  delete env.ELECTRON_RENDERER_URL;
  app = await electron.launch({ args: ["."], env, timeout: 120_000 });
  page = await app.firstWindow();
  page.on("pageerror", (error) => errors.push(error.message));
  await app.evaluate(({ BrowserWindow, dialog }, directory) => {
    const win = BrowserWindow.getAllWindows()[0];
    win.setContentSize(1280, 860);
    win.show();
    dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [directory] });
  }, project);
  await page.getByRole("button", { name: "Open another project", exact: true }).click();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 90_000 });
});

test.afterAll(async () => {
  await app?.close().catch(() => {});
  try {
    execFileSync(binary, ["--workspace", project, "serve", "--stop"], { env, stdio: "ignore", timeout: 30_000 });
  } catch {}
  stopChatHome(binary, env);
  if (temporary) await rm(temporary, { recursive: true, force: true }).catch(() => {});
});

/** The feed's updater, as electron-updater answers: `notes` are the release notes it carries. */
async function feed(notes: unknown) {
  await app.evaluate((_electron, notes) => {
    const record = globalThis as { haarnessUpdater?: unknown; installed?: number; feedNotes?: unknown };
    record.feedNotes = notes;
    // Desktop listens to the updater it first met, as to electron-updater's one.
    if (record.haarnessUpdater) return;
    const listeners: Record<string, ((value: unknown) => void)[]> = {};
    record.installed = 0;
    const info = () => ({ version: "9.9.9", files: [], path: "", sha512: "", releaseDate: "2026-10-22", releaseNotes: record.feedNotes });
    record.haarnessUpdater = {
      autoDownload: true,
      setFeedURL() {},
      on(name: string, listener: (value: unknown) => void) {
        (listeners[name] ??= []).push(listener);
        return this;
      },
      async checkForUpdates() {
        return { isUpdateAvailable: true, updateInfo: info() };
      },
      async downloadUpdate() {
        for (const listener of listeners["download-progress"] ?? []) listener({ percent: 50 });
        for (const listener of listeners["update-downloaded"] ?? []) listener(info());
        return [];
      },
      quitAndInstall() {
        record.installed!++;
      },
    };
  }, notes);
}

test("the update confirmation shows the release notes, rendered and sanitized, in their own scroll area", async () => {
  await feed(
    "## Highlights\n\n- Pick an element in the browser\n- Export logs from **Settings**\n- [Read the guide](https://haarness.ai/docs/desktop) · [the changelog](./CHANGELOG.md) · [a file](file:///etc/hosts)\n\n<img src=x onerror=\"window.pwned=1\"><script>window.pwned=2</script>\n\n" +
      Array.from({ length: 40 }, (_, index) => `- Fix ${index + 1}`).join("\n"),
  );
  await page.getByRole("button", { name: "Settings" }).first().click();
  await settings().getByRole("button", { name: "About", exact: true }).click();
  await settings().getByRole("button", { name: "Check for updates" }).click();
  await settings().getByRole("button", { name: "Download 9.9.9" }).click();
  await settings().getByRole("button", { name: "Restart & install" }).click();
  const confirm = page.getByRole("dialog", { name: "Install update?" });
  await expect(confirm).toBeVisible();
  const notes = confirm.getByRole("region", { name: "Release notes" });
  await expect(notes).toContainText("What’s new in 9.9.9");
  await expect(notes.locator("h2")).toHaveText("Highlights");
  await expect(notes.locator("strong")).toHaveText("Settings");
  await expect(notes.locator("script")).toHaveCount(0);
  expect(await notes.locator("img").evaluateAll((images) => images.map((image) => image.getAttribute("onerror")))).not.toContain('window.pwned=1');
  expect(await page.evaluate(() => (window as { pwned?: number }).pwned)).toBeUndefined();
  // A long changelog scrolls inside its well; the buttons stay in view.
  const body = notes.locator(".release-notes-body");
  expect(await body.evaluate((element) => element.scrollHeight > element.clientHeight)).toBe(true);
  await expect(confirm.getByRole("button", { name: "Restart & install" })).toBeInViewport();
  // Enter never installs by accident: the dialog starts on Cancel.
  await expect(confirm.getByRole("button", { name: "Cancel" })).toBeFocused();
  await shoot("update-notes", confirm);
  // A web link opens in the system browser; a relative or file link does nothing.
  await app.evaluate(({ shell }) => {
    const record = globalThis as { opened?: string[] };
    record.opened = [];
    shell.openExternal = (async (url: string) => void record.opened!.push(url)) as typeof shell.openExternal;
  });
  for (const name of ["Read the guide", "the changelog", "a file"]) await notes.getByRole("link", { name }).click();
  await expect.poll(() => app.evaluate(() => (globalThis as { opened?: string[] }).opened)).toEqual(["https://haarness.ai/docs/desktop"]);
  await expect(confirm).toBeVisible();
  await confirm.getByRole("button", { name: "Cancel" }).click();
  await expect(confirm).toBeHidden();
  expect(await app.evaluate(() => (globalThis as { installed?: number }).installed)).toBe(0);
});

test("without release notes the confirmation is as it was, and installing still restarts", async () => {
  await feed(null);
  await settings().getByRole("button", { name: "Check for updates" }).click();
  await settings().getByRole("button", { name: "Download 9.9.9" }).click();
  await settings().getByRole("button", { name: "Restart & install" }).click();
  const confirm = page.getByRole("dialog", { name: "Install update?" });
  await expect(confirm).toBeVisible();
  await expect(confirm).toContainText("Haarness will restart. Save your file edits before continuing.");
  await expect(confirm.getByRole("region", { name: "Release notes" })).toHaveCount(0);
  await shoot("update-no-notes", confirm);
  await confirm.getByRole("button", { name: "Restart & install" }).click();
  await expect.poll(() => app.evaluate(() => (globalThis as { installed?: number }).installed)).toBe(1);
  expect(errors).toEqual([]);
});
