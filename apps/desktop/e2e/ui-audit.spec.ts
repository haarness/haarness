/* Visual audit harness: boots an isolated Desktop and captures the surfaces a
   spacing/theme/localisation review needs, in several palettes and languages. */
import { Haarness } from "@haarness/sdk";
import { test, expect, _electron as electron, type ElectronApplication, type Page } from "@playwright/test";
import { mkdtemp, mkdir, writeFile, rm, realpath } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";
import { stopChatHome } from "./fixtures/chat-home";

const binary = process.env.HAARNESS_BIN ? resolve(process.env.HAARNESS_BIN) : resolve("../../target/debug/haarness");
const shots = process.env.UI_AUDIT_OUT ? resolve(process.env.UI_AUDIT_OUT) : resolve("test-results/ui-audit");
let app: ElectronApplication, page: Page, temporary: string, workspace: string;
let env: Record<string, string>;

const titles = [
  "Search Google for Russian Blue Cats",
  "General Assistance",
  "HYPEUSDT 1H Elliott Wave Analysis",
  "Hello from Haarness",
  "Remote project ready",
  "Code Security Audit",
  "Getting Started",
];

async function shot(name: string) {
  await page.screenshot({ path: join(shots, name + ".png") });
}
async function prefs(values: Record<string, unknown>) {
  await page.evaluate(async (patch) => {
    await window.haarness.preferences(patch as never);
  }, values);
  await page.waitForTimeout(400);
}

test.beforeAll(async () => {
  test.setTimeout(600000);
  temporary = await realpath(await mkdtemp(join(tmpdir(), "haarness-ui-audit-")));
  workspace = join(temporary, "workspace");
  await mkdir(join(workspace, "src"), { recursive: true });
  await mkdir(join(workspace, ".haarness"), { recursive: true });
  await writeFile(join(workspace, ".haarness", "config.toml"),
    '[defaults]\nprovider="echo"\nmodel="echo-1"\n[permissions]\nbash="ask"\n');
  await writeFile(join(workspace, "README.md"), "# Fixture\n\nA workspace for the visual audit.\n");
  await writeFile(join(workspace, "src", "greeting.ts"), 'export const greeting = "hello";\n');
  for (const args of [["init", "-b", "main"], ["add", "."],
    ["-c", "user.name=Fixture", "-c", "user.email=f@example.test", "commit", "-m", "Initial"]])
    execFileSync("git", args, { cwd: workspace, stdio: "ignore" });
  await mkdir(join(temporary, "desktop"));
  await writeFile(join(temporary, "desktop", "desktop.json"),
    JSON.stringify({ preferences: {}, recent: [], values: {}, credentials: {} }));
  env = Object.fromEntries(Object.entries(process.env).filter((e): e is [string, string] => e[1] !== undefined));
  Object.assign(env, {
    HAARNESS_BIN: binary,
    HAARNESS_DATA_DIR: join(temporary, "state"),
    HAARNESS_DESKTOP_DATA: join(temporary, "desktop"),
    HAARNESS_TEST_PROVIDER: "1",
    HAARNESS_DESKTOP_TEST: "1",
  });
  delete env.ELECTRON_RUN_AS_NODE;
  delete env.ELECTRON_RENDERER_URL;
  const connect = JSON.parse(execFileSync(binary, ["--workspace", workspace, "--trust-project", "desktop"],
    { env, input: JSON.stringify({ action: "connect" }), encoding: "utf8", timeout: 240000 }));
  const client = new Haarness({ baseUrl: connect.url, token: connect.token });
  for (const title of titles) await client.createThread({ title, provider: "echo", model: "echo-1" });
  client.close();
  app = await electron.launch({ args: ["."], env, timeout: 240000 });
  page = await app.firstWindow();
  page.setDefaultTimeout(30000);
  await app.evaluate(({ BrowserWindow }) => {
    for (const win of BrowserWindow.getAllWindows()) win.setContentSize(1280, 880);
  });
  await app.evaluate(({ dialog }, directory) => {
    const original = dialog.showOpenDialog;
    dialog.showOpenDialog = async () => { dialog.showOpenDialog = original; return { canceled: false, filePaths: [directory] }; };
  }, workspace);
  await page.getByRole("button", { name: "Open another project", exact: true }).click();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 240000 });
  await page.waitForTimeout(1500);
});

test.afterAll(async () => {
  if (app) {
    await app.evaluate(({ BrowserWindow }) => { for (const w of BrowserWindow.getAllWindows()) w.destroy(); }).catch(() => {});
    await app.close().catch(() => {});
  }
  try { execFileSync(binary, ["--workspace", workspace, "--trust-project", "serve", "--stop"], { env, stdio: "ignore", timeout: 30000 }); } catch {}
  stopChatHome(binary, env);
  if (temporary) await rm(temporary, { recursive: true, force: true });
});

test("capture surfaces", async () => {
  test.setTimeout(600000);
  await shot("01-shell-dark");

  // Universal search — the palette in the report.
  await page.locator(".nav-group .nav-row").first().click();
  await expect(page.getByRole("dialog", { name: "Universal search" })).toBeVisible();
  await page.waitForTimeout(500);
  await shot("02-palette-dark");
  // What the palette actually offers, so "is anything too deep?" is reviewable.
  const inventory = await page.evaluate(() => {
    const rows: string[] = [];
    for (const node of document.querySelectorAll(
      ".palette-results > .palette-group, .palette-results > button",
    ))
      rows.push(
        node.matches(".palette-group")
          ? "\n## " + (node.textContent ?? "")
          : "  " +
              (node.getAttribute("aria-label") ?? "") +
              "  —  " +
              (node.getAttribute("aria-description") ?? ""),
      );
    return rows.join("\n");
  });
  await writeFile(join(shots, "palette-inventory.txt"), inventory);
  await page.keyboard.press("Escape");
  await page.waitForTimeout(300);

  // The composer's folder chip popover.
  const picker = page.locator("button.composer-folder").first();
  if (await picker.isVisible()) {
    await picker.click();
    await page.waitForTimeout(500);
    await shot("03-project-picker");
    await page.keyboard.press("Escape");
    await page.waitForTimeout(300);
  }

  // Settings.
  await page.locator(".sidebar-footer .nav-row").first().click();
  await page.waitForTimeout(1500);
  await shot("04-settings-dark");

  await prefs({ theme: "light" });
  await shot("05-settings-light");
  await page.keyboard.press("Escape");
  await page.waitForTimeout(400);
  await shot("06-shell-light");

  await page.locator(".nav-group .nav-row").first().click();
  await page.waitForTimeout(600);
  await shot("07-palette-light");
  await page.keyboard.press("Escape");
  await page.waitForTimeout(300);

  // Language coverage.
  await prefs({ theme: "dark", language: "de" });
  await shot("08-shell-de");
  await page.locator(".sidebar-footer .nav-row").first().click();
  await page.waitForTimeout(1500);
  await shot("09-settings-de");
  await page.keyboard.press("Escape");
  await page.waitForTimeout(400);

  await prefs({ language: "ja" });
  await shot("10-shell-ja");
  await page.locator(".sidebar-footer .nav-row").first().click();
  await page.waitForTimeout(1500);
  await shot("11-settings-ja");
  await page.keyboard.press("Escape");
  await page.waitForTimeout(400);

  await prefs({ language: "ar" });
  await shot("12-shell-ar");
  await page.locator(".sidebar-footer .nav-row").first().click();
  await page.waitForTimeout(1500);
  await shot("13-settings-ar");
  await page.keyboard.press("Escape");
  await page.waitForTimeout(400);
  await page.locator(".nav-group .nav-row").first().click();
  await page.waitForTimeout(600);
  await shot("14-palette-ar");
  await page.keyboard.press("Escape");

  await prefs({ language: "th" });
  await shot("15-shell-th");
  await page.locator(".nav-group .nav-row").first().click();
  await page.waitForTimeout(600);
  await shot("16-palette-th");
  await page.keyboard.press("Escape");
  await page.waitForTimeout(300);

  // A canvas preview's banners in the chat: their frames, buttons and a
  // comparison, and in the Files panel the version strip and the notice a
  // blocked link brings.
  await prefs({ theme: "light", language: "en" });
  await writeFile(join(workspace, "canvas.html"), '<h1>Canvas first</h1><a href="https://example.test/leave">Leave</a>\n');
  await page.getByRole("button", { name: "New chat", exact: true }).click();
  const composer = page.getByRole("textbox", { name: "Message", exact: true });
  const idle = () => expect(page.getByRole("button", { name: "Stop response", exact: true })).toHaveCount(0);
  await composer.fill('!tool preview {"filePath":"canvas.html","title":"Canvas demo"}');
  await composer.press("Enter");
  await expect(page.locator(".preview-banner")).toHaveCount(1);
  await idle();
  await writeFile(join(workspace, "canvas.html"), '<h1>Canvas second</h1><a href="https://example.test/leave">Leave</a>\n');
  await composer.fill('!tool preview {"filePath":"canvas.html","title":"Canvas updated"}');
  await composer.press("Enter");
  await expect(page.locator(".preview-banner")).toHaveCount(2);
  await idle();
  const earlier = page.locator(".preview-banner").first();
  await earlier.getByRole("button", { name: "Compare", exact: true }).click();
  await expect(earlier.locator(".preview-added")).toBeVisible();
  await earlier.getByRole("button", { name: "Open preview", exact: true }).click();
  await expect(page.locator(".preview-version-strip")).toBeVisible();
  const content = page.frameLocator(".file-preview .html-preview iframe").frameLocator("iframe").frameLocator("iframe");
  await content.getByRole("link", { name: "Leave" }).click();
  await expect(page.locator(".file-preview .preview-notice")).toBeVisible();
  await earlier.scrollIntoViewIfNeeded();
  await shot("17-preview-banner-light");
  await prefs({ theme: "dark" });
  await shot("18-preview-banner-dark");
  await prefs({ language: "ar" });
  await shot("19-preview-banner-ar");
  await prefs({ theme: "light" });
  await shot("20-preview-banner-ar-light");
  await prefs({ theme: "dark", language: "en", palette: "haarness" });
});
