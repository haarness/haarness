import {
  test,
  expect,
  _electron as electron,
  type ElectronApplication,
  type Page,
} from "@playwright/test";
import { mkdtemp, mkdir, readFile, realpath, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";
import { stopChatHome } from "./fixtures/chat-home";

const binary = resolve(process.env.HAARNESS_BIN ?? "../../target/debug/haarness");
const evidence = process.env.HAARNESS_USAGE_EVIDENCE;
let app: ElectronApplication;
let page: Page;
let temporary: string;
let project: string;
let env: Record<string, string>;

test.describe.configure({ mode: "serial" });

async function launch(open = true) {
  app = await electron.launch({ args: ["."], env, timeout: 120000 });
  page = await app.firstWindow();
  await app.evaluate(({ BrowserWindow }) => {
    const win = BrowserWindow.getAllWindows()[0];
    win.setContentSize(1280, 820);
    win.show();
  });
  await app.evaluate(({ dialog }, directory) => {
    dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [directory] });
  }, project);
  // A relaunch reopens the last project by itself.
  if (open)
    await page
      .getByRole("button", { name: "Open another project", exact: true })
      .click();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
}

test.beforeAll(async () => {
  test.setTimeout(240000);
  temporary = await realpath(await mkdtemp(join(tmpdir(), "haarness-usage-statistics-")));
  project = join(temporary, "project");
  await mkdir(join(project, ".haarness"), { recursive: true });
  await writeFile(
    join(project, ".haarness/config.toml"),
    '[defaults]\nprovider="echo"\nmodel="echo-1"\n',
  );
  execFileSync("git", ["init", "-b", "main"], { cwd: project, stdio: "ignore" });
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
    // Test runs turn usage statistics off unless told otherwise; an empty
    // value leaves the decision to the setting, as for a real person.
    HAARNESS_TELEMETRY: "",
    // Nothing may reach a real Cloud from a test.
    HAARNESS_CLOUD_API_URL: "http://127.0.0.1:9",
  });
  delete env.ELECTRON_RUN_AS_NODE;
  delete env.DO_NOT_TRACK;
  if (evidence) await mkdir(evidence, { recursive: true });
  await launch();
});

test.afterAll(async () => {
  await app?.close();
  try {
    execFileSync(binary, ["--workspace", project, "serve", "--stop"], { env, stdio: "ignore", timeout: 30000 });
  } catch {
    // Already stopped.
  }
  stopChatHome(binary, env);
  await rm(temporary, { recursive: true, force: true });
});

test("the setting is on by default and can be found from Settings", async () => {
  // No notice interrupts anyone; the setting is where people look for it.
  await expect(page.locator(".usage-notice")).toHaveCount(0);
  await page.getByRole("button", { name: "Settings" }).first().click();
  const row = page.locator('[data-setting="Share usage statistics"]');
  await expect(row).toBeVisible();
  const toggle = row.getByRole("switch", { name: "Share usage statistics" });
  await expect(toggle).toBeChecked();
  await expect(toggle).toBeEnabled();
  if (evidence) await row.screenshot({ path: join(evidence, "setting.png") });
});

test("the preview shows the exact report", async () => {
  await page.getByRole("button", { name: "See what's shared" }).click();
  const dialog = page.getByRole("dialog", { name: "What Haarness shares" });
  await expect(dialog).toBeVisible();
  await expect(dialog.locator(".usage-preview")).toContainText('"schemaVersion": 1');
  await expect(dialog.locator(".usage-preview")).toContainText('"installId"');
  if (evidence) await page.screenshot({ path: join(evidence, "preview.png") });
  await dialog.getByRole("button", { name: "Close dialog" }).click();
  await expect(dialog).toBeHidden();
});

test("the switch writes the global setting, which a relaunch keeps", async () => {
  const toggle = page.getByRole("switch", { name: "Share usage statistics" });
  await toggle.click();
  await expect(toggle).not.toBeChecked();
  // The write lands after the switch moves; wait for the global file.
  await expect
    .poll(async () => {
      try {
        return JSON.parse(await readFile(join(temporary, "state/config/haarness.json"), "utf8"))
          .telemetry?.usage;
      } catch {
        return undefined;
      }
    })
    .toBe(false);
  await app.close();
  await launch(false);
  await page.getByRole("button", { name: "Settings" }).first().click();
  await expect(page.getByRole("switch", { name: "Share usage statistics" })).not.toBeChecked();
});
