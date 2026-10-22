import { test, expect, _electron as electron, type ElectronApplication, type Page } from "@playwright/test";
import { mkdtemp, mkdir, readFile, realpath, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";

// Settings → General → Diagnostics exports this computer's logs (save dialog,
// then the zip shown in the file manager) and turns Detailed logging on for
// 30 minutes; Help → Export logs… and the palette do the same.
test.describe.configure({ mode: "serial" });

const binary = resolve(process.env.HAARNESS_BIN ?? "../../target/debug/haarness");
const shots = process.env.HAARNESS_DIAGNOSTICS_SCREENSHOTS;
let app: ElectronApplication;
let page: Page;
let temporary: string;
let project: string;
let env: Record<string, string>;
const errors: string[] = [];
const data = () => join(temporary, "state/data");
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
/** Saves land at `path`; the file manager is asked to show what was saved. */
async function stubSave(path: string) {
  await app.evaluate(({ dialog, shell }, path) => {
    const record = globalThis as { saveDialogs?: unknown[]; revealed?: string[] };
    record.saveDialogs = [];
    record.revealed = [];
    dialog.showSaveDialog = (async (...args: unknown[]) => {
      record.saveDialogs!.push(args.at(-1));
      return { canceled: false, filePath: path };
    }) as typeof dialog.showSaveDialog;
    shell.showItemInFolder = (path: string) => void record.revealed!.push(path);
  }, path);
}
const saved = () =>
  app.evaluate(() => {
    const record = globalThis as { saveDialogs?: { defaultPath?: string }[]; revealed?: string[] };
    return { dialogs: record.saveDialogs ?? [], revealed: record.revealed ?? [] };
  });
const zipText = (path: string) => execFileSync("unzip", ["-p", path], { encoding: "utf8" });

test.beforeAll(async () => {
  test.setTimeout(240_000);
  temporary = await realpath(await mkdtemp(join(process.env.HAARNESS_E2E_DIR ?? tmpdir(), "haarness-logs-export-")));
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
  };
  delete env.ELECTRON_RUN_AS_NODE;
  delete env.ELECTRON_RENDERER_URL;
  // A log line holding a credential, which an export must not carry.
  await mkdir(join(data(), "logs"), { recursive: true });
  await writeFile(
    join(data(), "logs/haarness.log.2026-10-21"),
    "2026-10-21T08:00:00.000Z 4242 serve  WARN haarness_core::provider: request failed Authorization: Bearer sk-live-SECRETVALUE123456789\n",
  );
  app = await electron.launch({ args: ["."], env, timeout: 120_000 });
  page = await app.firstWindow();
  page.on("pageerror", (error) => errors.push(error.message));
  await app.evaluate(({ BrowserWindow, dialog }, directory) => {
    const win = BrowserWindow.getAllWindows()[0];
    win.setContentSize(1280, 860);
    win.show();
    dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [directory] });
  }, project);
  await page.getByRole("region", { name: "Project home" }).getByRole("button", { name: "Open project", exact: true }).click();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 90_000 });
});

test.afterAll(async () => {
  await app?.close().catch(() => {});
  try {
    execFileSync(binary, ["--workspace", project, "serve", "--stop"], { env, stdio: "ignore", timeout: 30_000 });
  } catch {}
  if (temporary) await rm(temporary, { recursive: true, force: true }).catch(() => {});
});

test("Detailed logging turns on for 30 minutes, says how long it has left, and turns off", async () => {
  await page.getByRole("button", { name: "Settings" }).first().click();
  const row = settings().locator('[data-setting="Detailed logging"]');
  const toggle = row.getByRole("switch", { name: "Detailed logging" });
  await expect(toggle).toBeEnabled();
  await expect(toggle).not.toBeChecked();
  await expect(row).toContainText("for 30 minutes, then turns itself off");
  await toggle.click();
  await expect(toggle).toBeChecked();
  await expect(row).toContainText(/On · turns off in (30|29) min/);
  // Every Haarness process here follows the same control file the CLI writes.
  const control = JSON.parse(await readFile(join(data(), "log-level.json"), "utf8"));
  expect(control.level).toBe("debug");
  expect(Date.parse(control.until) - Date.now()).toBeGreaterThan(29 * 60_000);
  const cli = JSON.parse(execFileSync(binary, ["logs", "level", "--json"], { env, encoding: "utf8" }));
  expect(cli.level).toBe("debug");
  await shoot("diagnostics-on", settings().locator('[data-setting="Diagnostics"]').locator(".."));
  await toggle.click();
  await expect(toggle).not.toBeChecked();
  await expect(stat(join(data(), "log-level.json"))).rejects.toThrow();
  // A level raised elsewhere shows as soon as Settings opens.
  execFileSync(binary, ["logs", "level", "trace", "--until-stopped"], { env });
  await page.keyboard.press("Escape");
  await expect(settings()).toBeHidden();
  await page.getByRole("button", { name: "Settings" }).first().click();
  await expect(settings().locator('[data-setting="Detailed logging"]')).toContainText("On until stopped");
  await expect(settings().getByRole("switch", { name: "Detailed logging" })).toBeChecked();
  await settings().getByRole("switch", { name: "Detailed logging" }).click();
  await expect(settings().getByRole("switch", { name: "Detailed logging" })).not.toBeChecked();
  await expect(stat(join(data(), "log-level.json"))).rejects.toThrow();
  await shoot("diagnostics-off", settings().locator('[data-setting="Diagnostics"]').locator(".."));
  // Reading the level runs `haarness logs` from the home folder, which must
  // not become a project.
  const registry = await readFile(join(data(), "workspaces.json"), "utf8").catch(() => "");
  expect(registry).not.toContain(JSON.stringify(await app.evaluate(({ app }) => app.getPath("home"))));
});

test("Export logs… saves a scrubbed zip named for today and shows it in the file manager", async () => {
  const target = join(temporary, "exports", "from-settings.zip");
  await mkdir(join(temporary, "exports"), { recursive: true });
  await stubSave(target);
  await settings().locator('[data-setting="Diagnostics"]').getByRole("button", { name: "Export logs…" }).click();
  await expect.poll(async () => (await saved()).revealed).toEqual([target]);
  const { dialogs } = await saved();
  const today = new Date();
  const pad = (value: number) => String(value).padStart(2, "0");
  const downloads = await app.evaluate(({ app }) => app.getPath("downloads"));
  expect(dialogs[0]?.defaultPath).toBe(
    join(downloads, `haarness-logs-${today.getFullYear()}-${pad(today.getMonth() + 1)}-${pad(today.getDate())}.zip`),
  );
  const text = zipText(target);
  expect(text).toContain("request failed Authorization:");
  expect(text).not.toContain("SECRETVALUE");
  expect(text).toContain("[redacted]");
  // No success notice: the file manager showing the zip says it worked.
  await expect(page.locator(".notifications > *")).toHaveCount(0);
  await page.keyboard.press("Escape");
  await expect(settings()).toBeHidden();
});

test("Help → Export logs… and the palette export the same way, and a cancelled save does nothing", async () => {
  const fromMenu = join(temporary, "exports", "from-menu.zip");
  await stubSave(fromMenu);
  const label = await app.evaluate(({ Menu, BrowserWindow }) => {
    const help = Menu.getApplicationMenu()!.items.find((item) => item.label === "Help")!;
    const item = help.submenu!.items.find((entry) => entry.label.startsWith("Export logs"))!;
    item.click(undefined, BrowserWindow.getAllWindows()[0], undefined as never);
    return item.label;
  });
  expect(label).toBe("Export logs…");
  await expect.poll(async () => (await saved()).revealed).toEqual([fromMenu]);
  expect(zipText(fromMenu)).not.toContain("SECRETVALUE");
  const fromPalette = join(temporary, "exports", "from-palette.zip");
  await stubSave(fromPalette);
  await page.getByRole("button", { name: "Search", exact: true }).click();
  await page.getByRole("combobox", { name: "Search Haarness", exact: true }).fill("Export logs");
  await page.getByRole("option", { name: /Export logs…/ }).click();
  await expect.poll(async () => (await saved()).revealed).toEqual([fromPalette]);
  await app.evaluate(({ dialog }) => {
    dialog.showSaveDialog = (async () => ({ canceled: true, filePath: "" })) as typeof dialog.showSaveDialog;
  });
  await stat(fromPalette);
  await page.getByRole("button", { name: "Search", exact: true }).click();
  await page.getByRole("combobox", { name: "Search Haarness", exact: true }).fill("Export logs");
  await page.getByRole("option", { name: /Export logs…/ }).click();
  await page.waitForTimeout(500);
  expect((await saved()).revealed).toEqual([fromPalette]);
  await expect(page.locator(".notifications > *")).toHaveCount(0);
});
