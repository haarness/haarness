import { test, expect, _electron as electron, type ElectronApplication, type Page } from "@playwright/test";
import { createHash } from "node:crypto";
import { copyFile, mkdir, mkdtemp, readFile, realpath, rm, stat, writeFile } from "node:fs/promises";
import { createServer, type Server } from "node:http";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";
import { stopChatHome } from "./fixtures/chat-home";

// Desktop finds a newer version on a stand-in for the Haarness release
// service, downloads and verifies it, offers the restart in the sidebar, and
// restarting replaces a stand-in for the installed app (a signed macOS
// bundle, or an AppImage) and opens it again. Failures say why in Settings →
// About and nothing in the sidebar; with automatic downloads off the sidebar
// offers the update and shows its download; quitting installs quietly.
test.describe.configure({ mode: "serial" });
test.skip(process.platform === "win32", "the Windows setup is not run in tests");

const binary = resolve(process.env.HAARNESS_BIN ?? "../../target/debug/haarness");
const shots = process.env.HAARNESS_DIAGNOSTICS_SCREENSHOTS;
const os = process.platform === "darwin" ? "macos" : "linux";
const arch = process.arch === "arm64" ? "arm64" : "x64";
const kind = os === "macos" ? "zip" : "AppImage";
const NEW = "9.9.9";

let temporary: string;
let service: Server;
let releaseUrl: string;
/** What the stand-in service does: answer, fail, or send damaged bytes. */
const behaviour = { mode: "ok" as "ok" | "down" | "damaged", slow: false, requests: [] as string[] };
let artifact: Buffer;
let target: string;
let app: ElectronApplication | undefined;
let page: Page;
let env: Record<string, string>;
const errors: string[] = [];

/** A stand-in macOS app: a signed bundle that says its version. */
async function macApp(folder: string, version: string) {
  const bundle = join(folder, "Haarness.app");
  await rm(bundle, { recursive: true, force: true });
  await mkdir(join(bundle, "Contents", "MacOS"), { recursive: true });
  await copyFile("/usr/bin/true", join(bundle, "Contents", "MacOS", "Haarness"));
  await writeFile(
    join(bundle, "Contents", "Info.plist"),
    `<?xml version="1.0" encoding="UTF-8"?>\n<plist version="1.0"><dict><key>CFBundleExecutable</key><string>Haarness</string><key>CFBundleIdentifier</key><string>dev.haarness.update-test</string><key>CFBundleShortVersionString</key><string>${version}</string></dict></plist>`,
  );
  execFileSync("/usr/bin/codesign", ["--force", "--deep", "--sign", "-", bundle]);
  return bundle;
}
/** The version the stand-in installed app is now. */
async function installed() {
  if (os === "macos")
    return execFileSync("/usr/bin/plutil", ["-extract", "CFBundleShortVersionString", "raw", "-o", "-", join(target, "Contents", "Info.plist")], { encoding: "utf8" }).trim();
  return (await readFile(target, "utf8")).trim();
}
async function reinstallOld() {
  if (os === "macos") await macApp(join(temporary, "Applications"), "0.1.5");
  else await writeFile(target, "0.1.5\n", { mode: 0o755 });
}

function manifest(port: number) {
  return {
    channel: "stable",
    manifest: {
      version: NEW,
      products: {
        desktop: {
          version: NEW,
          notes_url: "https://haarness.ai/docs",
          artifacts: [
            {
              id: "a1",
              os,
              arch,
              kind,
              file: `Haarness-${NEW}-${os}-${arch}.${kind}`,
              url: `http://127.0.0.1:${port}/edge/Haarness-${NEW}.${kind}`,
              size: artifact.length,
              digest: { sha256: createHash("sha256").update(artifact).digest("hex") },
            },
          ],
        },
      },
    },
  };
}

async function launch(extra: Record<string, string> = {}) {
  const data = await mkdtemp(join(temporary, "run-"));
  env = {
    ...Object.fromEntries(Object.entries(process.env).filter((x): x is [string, string] => x[1] !== undefined)),
    HAARNESS_BIN: binary,
    HAARNESS_HOME: join(data, "home"),
    HAARNESS_DATA_DIR: join(data, "state"),
    HAARNESS_DESKTOP_DATA: join(data, "desktop"),
    HAARNESS_TEST_PROVIDER: "1",
    HAARNESS_DESKTOP_TEST: "1",
    HAARNESS_RELEASE_URL: releaseUrl,
    HAARNESS_UPDATE_TARGET: target,
    HAARNESS_UPDATE_REOPEN_LOG: join(temporary, "reopened"),
    ...extra,
  };
  delete env.ELECTRON_RUN_AS_NODE;
  delete env.ELECTRON_RENDERER_URL;
  app = await electron.launch({ args: ["."], env, timeout: 120_000 });
  page = await app.firstWindow();
  page.on("pageerror", (error) => errors.push(error.message));
  await app.evaluate(({ BrowserWindow }) => {
    const win = BrowserWindow.getAllWindows()[0];
    win.setContentSize(1280, 860);
    win.show();
  });
  await expect(page.locator(".sidebar-footer")).toBeVisible({ timeout: 90_000 });
  return data;
}
async function quit() {
  if (!app) return;
  const process = app.process();
  const exited = new Promise((resolve) => (process.exitCode !== null ? resolve(null) : process.once("exit", resolve)));
  await app.close().catch(() => {});
  await exited;
  stopChatHome(binary, env);
  app = undefined;
}
const settings = () => page.getByRole("dialog", { name: "Settings", exact: true });
async function about() {
  if (!(await settings().isVisible())) await page.getByRole("button", { name: "Settings" }).first().click();
  await settings().getByRole("button", { name: "About", exact: true }).click();
  return settings().locator('[data-setting="Updates"]');
}
async function closeSettings() {
  await page.keyboard.press("Escape");
  await expect(settings()).toBeHidden();
}
const updateRow = () => page.locator(".sidebar-footer .update-row");
async function theme(value: "light" | "dark") {
  await app!.evaluate(({ nativeTheme }, value) => {
    nativeTheme.themeSource = value;
  }, value);
  await page.emulateMedia({ colorScheme: value });
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
  temporary = await realpath(await mkdtemp(join(process.env.HAARNESS_E2E_DIR ?? tmpdir(), "haarness-updates-")));
  if (os === "macos") {
    const built = await macApp(join(temporary, "build"), NEW);
    execFileSync("/usr/bin/ditto", ["-c", "-k", "--keepParent", built, join(temporary, "new.zip")]);
    artifact = await readFile(join(temporary, "new.zip"));
    target = join(temporary, "Applications", "Haarness.app");
  } else {
    artifact = Buffer.from(`${NEW}\n`);
    target = join(temporary, "bin", "Haarness.AppImage");
    await mkdir(join(temporary, "bin"), { recursive: true });
  }
  await reinstallOld();
  service = createServer(async (request, response) => {
    const url = request.url ?? "";
    behaviour.requests.push(url);
    if (behaviour.mode === "down") {
      response.writeHead(503).end("maintenance");
      return;
    }
    if (url === "/api/v1/releases/stable/desktop") {
      response.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify(manifest((service.address() as { port: number }).port)));
      return;
    }
    if (url.startsWith("/edge/") || url === "/api/v1/releases/artifacts/a1") {
      const bytes = behaviour.mode === "damaged" ? Buffer.concat([artifact.subarray(0, -1), Buffer.from([artifact.at(-1)! ^ 1])]) : artifact;
      response.writeHead(200, { "content-length": String(bytes.length), etag: '"a1"' });
      if (!behaviour.slow) return void response.end(bytes);
      // Slow enough for the progress to show.
      const step = Math.ceil(bytes.length / 30);
      for (let at = 0; at < bytes.length; at += step) {
        response.write(bytes.subarray(at, at + step));
        await new Promise((resolve) => setTimeout(resolve, 120));
      }
      response.end();
      return;
    }
    response.writeHead(404).end();
  });
  await new Promise<void>((done) => service.listen(0, "127.0.0.1", done));
  releaseUrl = `http://127.0.0.1:${(service.address() as { port: number }).port}`;
});

test.afterAll(async () => {
  await quit();
  service?.close();
  if (temporary) await rm(temporary, { recursive: true, force: true }).catch(() => {});
  expect(errors).toEqual([]);
});

test("a failed check says why in About, not in the sidebar; once the service is back the update downloads and Restart installs it", async () => {
  test.setTimeout(240_000);
  behaviour.mode = "down";
  await launch();
  const updates = await about();
  await updates.getByRole("button", { name: "Check for updates" }).click();
  await expect(updates.getByText("The update service gave an unexpected answer. Haarness will try again.")).toBeVisible();
  await expect(updateRow()).toHaveCount(0);
  await shoot("updates-failed", settings());
  // The service is back: a check finds the release, which downloads by itself.
  behaviour.mode = "ok";
  await updates.getByRole("button", { name: "Check for updates" }).click();
  await expect(updates.getByText(`Haarness ${NEW} is ready to install.`)).toBeVisible({ timeout: 60_000 });
  await expect(updates.getByText("The update service gave an unexpected answer.", { exact: false })).toHaveCount(0);
  await expect(updates.getByRole("link", { name: "Release notes" })).toBeVisible();
  await expect(settings().getByRole("switch", { name: "Download updates automatically" })).toBeChecked();
  await shoot("updates-ready-about", settings());
  await closeSettings();
  await expect(updateRow()).toHaveText("Restart to update");
  await shoot("updates-ready-sidebar", page.locator(".sidebar-footer"));
  // The staged app is verified and waiting; the installed one is untouched.
  expect(await installed()).toBe("0.1.5");
  // Restart: Haarness quits, the stand-in app is replaced, and opened again.
  const process = app!.process();
  const exited = new Promise((resolve) => process.once("exit", resolve));
  await updateRow().click();
  await exited;
  stopChatHome(binary, env);
  app = undefined;
  await expect.poll(installed, { timeout: 30_000 }).toBe(NEW);
  await expect.poll(() => stat(join(temporary, "reopened")).then(() => true, () => false), { timeout: 10_000 }).toBe(true);
});

test("with automatic downloads off the sidebar offers the update and shows its download; quitting installs it quietly", async () => {
  test.setTimeout(240_000);
  await reinstallOld();
  await rm(join(temporary, "reopened"), { force: true });
  behaviour.mode = "ok";
  behaviour.slow = true;
  behaviour.requests = [];
  const data = await mkdtemp(join(temporary, "pre-"));
  await mkdir(join(data, "desktop"), { recursive: true });
  await writeFile(join(data, "desktop", "desktop.json"), JSON.stringify({ preferences: { autoUpdate: false } }));
  await launch({ HAARNESS_DESKTOP_DATA: join(data, "desktop") });
  const updates = await about();
  await expect(settings().getByRole("switch", { name: "Download updates automatically" })).not.toBeChecked();
  await updates.getByRole("button", { name: "Check for updates" }).click();
  await expect(updates.getByText(`Haarness ${NEW} is available.`)).toBeVisible();
  await expect(updates.getByRole("button", { name: `Download ${NEW}` })).toBeVisible();
  await closeSettings();
  // Nothing downloads until asked.
  expect(behaviour.requests.filter((url) => url.startsWith("/edge/"))).toEqual([]);
  await expect(updateRow()).toHaveText(`Update to ${NEW}`);
  await shoot("updates-available-sidebar", page.locator(".sidebar-footer"));
  await updateRow().click();
  await expect(updateRow()).toContainText(/Downloading \d+%/);
  await expect(page.locator(".sidebar-footer .update-progress")).toBeVisible();
  await shoot("updates-downloading-sidebar", page.locator(".sidebar-footer"));
  await expect(updateRow()).toHaveText("Restart to update", { timeout: 60_000 });
  // A plain quit installs it, without opening Haarness again.
  await quit();
  await expect.poll(installed, { timeout: 30_000 }).toBe(NEW);
  await new Promise((resolve) => setTimeout(resolve, 1000));
  expect(await stat(join(temporary, "reopened")).catch(() => null)).toBeNull();
  behaviour.slow = false;
});

test("a download damaged from every source is not installed; the sidebar offers to try again and About says why", async () => {
  test.setTimeout(240_000);
  await reinstallOld();
  behaviour.mode = "damaged";
  await launch();
  const updates = await about();
  await updates.getByRole("button", { name: "Check for updates" }).click();
  await expect(updates.getByText("The download was damaged. Haarness will try again.")).toBeVisible({ timeout: 60_000 });
  await expect(updates.getByRole("button", { name: `Download ${NEW}` })).toBeVisible();
  await closeSettings();
  await expect(updateRow()).toHaveText(`Update to ${NEW}`);
  await expect(updateRow()).toHaveClass(/failed/);
  // Fixed at the source: the row's click downloads it again.
  behaviour.mode = "ok";
  await updateRow().click();
  await expect(updateRow()).toHaveText("Restart to update", { timeout: 60_000 });
  await expect(updateRow()).not.toHaveClass(/failed/);
  expect(await installed()).toBe("0.1.5");
  await quit();
});

test("Restart to update with unsaved file edits asks first; keeping them calls the restart off and nothing shuts down", async () => {
  test.setTimeout(300_000);
  await reinstallOld();
  await rm(join(temporary, "reopened"), { force: true });
  behaviour.mode = "ok";
  const project = join(temporary, "project");
  await mkdir(join(project, ".haarness"), { recursive: true });
  await writeFile(join(project, ".haarness/config.toml"), '[defaults]\nprovider="echo"\nmodel="echo-1"\n');
  await writeFile(join(project, "notes.txt"), "Original notes");
  await launch();
  await app!.evaluate(({ dialog }, { directory }) => {
    dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [directory] });
    const record = globalThis as { unloadAnswer?: number; unloadAsked?: string[] };
    record.unloadAnswer = 0;
    record.unloadAsked = [];
    dialog.showMessageBoxSync = ((...args: unknown[]) => {
      const options = args.find((value) => value && typeof value === "object" && "buttons" in (value as object)) as { message?: string };
      record.unloadAsked!.push(String(options?.message));
      return record.unloadAnswer!;
    }) as typeof dialog.showMessageBoxSync;
  }, { directory: project });
  // Electron asks about the unload itself (stubbed above); Playwright must not answer it too.
  page.on("dialog", () => {});
  await page.getByRole("button", { name: "Open another project", exact: true }).click();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 90_000 });
  // An unsaved edit in the Files panel.
  const button = (name: string) => page.getByRole("button", { name, exact: true });
  if (await button("Show side panel").isVisible()) await button("Show side panel").click();
  if (await button("Show Files panel").isVisible()) await button("Show Files panel").click();
  else if (!(await page.locator(".explorer").isVisible())) {
    await button("Open panel").click();
    await button("Open Files").click();
  }
  await page.locator(".file-entry").filter({ hasText: "notes.txt" }).click();
  await page.locator(".cm-content").fill("Unsaved notes");
  // The update is ready.
  const updates = await about();
  await updates.getByRole("button", { name: "Check for updates" }).click();
  await expect(updates.getByRole("button", { name: "Restart to update" })).toBeVisible({ timeout: 60_000 });
  await closeSettings();
  // Keep editing: the restart is called off, the window and its project stay.
  await updateRow().click();
  await expect.poll(() => app!.evaluate(() => (globalThis as { unloadAsked?: string[] }).unloadAsked)).toEqual(["There are unsaved file edits."]);
  await expect(updateRow()).toHaveText("Restart to update");
  await expect(page.locator(".cm-content")).toHaveText("Unsaved notes");
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
  expect(await app!.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows().length)).toBe(1);
  expect(await installed()).toBe("0.1.5");
  // The project still answers: its connection was not closed.
  await page.locator(".cm-content").press("ControlOrMeta+s");
  await expect.poll(() => readFile(join(project, "notes.txt"), "utf8")).toBe("Unsaved notes");
  // Saved now: Restart goes through without asking again.
  const process = app!.process();
  const exited = new Promise((resolve) => process.once("exit", resolve));
  // The window closes under the click.
  await updateRow().click({ noWaitAfter: true }).catch(() => {});
  await exited;
  expect(await app!.evaluate(() => 0).catch(() => "gone")).toBe("gone");
  app = undefined;
  try {
    execFileSync(binary, ["--workspace", project, "serve", "--stop"], { env, stdio: "ignore", timeout: 30_000 });
  } catch {}
  stopChatHome(binary, env);
  await expect.poll(installed, { timeout: 30_000 }).toBe(NEW);
});
