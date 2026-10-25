import { test, expect, _electron, type Page } from "@playwright/test";
import { mkdtemp, mkdir, realpath, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { tmpdir } from "node:os";
import { execFileSync } from "node:child_process";
import { stopChatHome } from "./fixtures/chat-home";

/**
 * `/remote-control` from the composer. The signed-out case needs nothing but
 * the app. The live case shares a chat through a local Haarness Cloud:
 *
 *   HAARNESS_REMOTE_CONTROL_API=http://127.0.0.1:29080 \
 *   HAARNESS_REMOTE_CONTROL_WEB=http://127.0.0.1:29173 \
 *   HAARNESS_REMOTE_CONTROL_ACCOUNT=<signed-in HAARNESS_CLOUD_CONFIG_DIR> \
 *   npx playwright test e2e/remote-control.spec.ts
 */
const binary = resolve(process.env.HAARNESS_BIN ?? "../../target/debug/haarness");
const api = process.env.HAARNESS_REMOTE_CONTROL_API;
const web = process.env.HAARNESS_REMOTE_CONTROL_WEB;
const account = process.env.HAARNESS_REMOTE_CONTROL_ACCOUNT;

async function launch(cloud: Record<string, string>) {
  const root = await realpath(await mkdtemp(join(tmpdir(), "haarness-remote-control-")));
  const project = join(root, "project");
  await mkdir(join(project, ".haarness"), { recursive: true });
  await writeFile(join(project, ".haarness/config.toml"), '[defaults]\nprovider="echo"\nmodel="echo-1"\n[lsp]\nenabled=false\n');
  const env: Record<string, string> = {
    ...Object.fromEntries(Object.entries(process.env).filter((entry): entry is [string, string] => entry[1] !== undefined)),
    HAARNESS_BIN: binary,
    HAARNESS_HOME: join(root, "home"),
    HAARNESS_DATA_DIR: join(root, "data"),
    HAARNESS_DESKTOP_DATA: join(root, "desktop"),
    HAARNESS_CLOUD_CONFIG_DIR: join(root, "account"),
    HAARNESS_TEST_PROVIDER: "1",
    HAARNESS_DESKTOP_TEST: "1",
    ...cloud,
  };
  delete env.ELECTRON_RUN_AS_NODE;
  delete env.ELECTRON_RENDERER_URL;
  const app = await _electron.launch({ args: ["."], env, timeout: 60000 });
  await app.evaluate(({ dialog, shell }, path) => {
    dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [path] });
    (globalThis as { opened?: string[] }).opened = [];
    shell.openExternal = async (url: string) => {
      (globalThis as unknown as { opened: string[] }).opened.push(url);
    };
  }, project);
  const page = await app.firstWindow();
  page.setDefaultTimeout(15000);
  await page.getByRole("button", { name: "Open another project", exact: true }).click();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 60000 });
  const close = async () => {
    await app.evaluate(({ BrowserWindow }) => {
      for (const win of BrowserWindow.getAllWindows()) win.destroy();
    }).catch(() => {});
    await app.close().catch(() => {});
    try { execFileSync(binary, ["--workspace", project, "serve", "--stop"], { env, stdio: "ignore", timeout: 30000 }); } catch {}
    stopChatHome(binary, env);
  };
  return { app, page, env, project, close };
}

async function chat(page: Page, text: string) {
  const message = page.getByRole("textbox", { name: "Message" });
  await message.fill(text);
  await message.press("Enter");
  await expect(page.locator(".transcript")).toContainText("Echo: " + text, { timeout: 30000 });
}

/** Type a command the way a person does: Enter completes it from the slash
 * list when that is open, and Enter again sends it. */
async function command(page: Page, text: string) {
  const message = page.getByRole("textbox", { name: "Message" });
  await message.fill(text);
  if (await page.locator("#slash-commands").isVisible()) await message.press("Enter");
  await message.press("Enter");
}

test("remote control asks a signed-out user to sign in, and the short form works", async () => {
  const { page, close } = await launch({ HAARNESS_CLOUD_API_URL: "http://127.0.0.1:9" });
  try {
    const message = page.getByRole("textbox", { name: "Message" });
    const titlebar = page.locator(".titlebar").getByRole("button", { name: "Remote control" });
    // Before the first message there is nothing to share, and the titlebar
    // offers nothing.
    await expect(titlebar).toHaveCount(0);
    await command(page, "/remote-control");
    await expect(page.getByText("Send a first message, then share the chat with /remote-control.")).toBeVisible();
    await chat(page, "hello");
    // The short form finds the command in the slash list.
    await message.fill("/rc");
    await expect(page.locator("#slash-commands")).toContainText("remote-control");
    await message.press("Enter");
    await expect(message).toHaveValue("/remote-control ");
    await message.press("Enter");
    const dialog = page.getByRole("dialog", { name: "Remote control" });
    await expect(dialog).toBeVisible();
    await expect(dialog.getByRole("button", { name: "Sign in to Haarness Cloud" })).toBeVisible();
    await expect(message).toHaveValue("");
    await dialog.getByRole("button", { name: "Cancel" }).click();
    await expect(dialog).toBeHidden();
    await command(page, "/rc later");
    await expect(page.getByText("Use /remote-control, /remote-control status or /remote-control stop.")).toBeVisible();
    // The titlebar's button opens the same dialog; nothing is live yet.
    await expect(titlebar).toBeVisible();
    await expect(titlebar).toHaveAttribute("aria-pressed", "false");
    await expect(titlebar.locator(".icon-button-dot")).toHaveCount(0);
    await titlebar.click();
    await expect(dialog.getByRole("button", { name: "Sign in to Haarness Cloud" })).toBeVisible();
    await dialog.getByRole("button", { name: "Cancel" }).click();
    await expect(dialog).toBeHidden();
  } finally {
    await close();
  }
});

test("remote control shares a chat through Haarness Cloud and stops sharing it", async () => {
  test.skip(!api || !web || !account, "Set HAARNESS_REMOTE_CONTROL_API, _WEB and _ACCOUNT to run against a local Cloud");
  test.setTimeout(180000);
  const { app, page, env, project, close } = await launch({
    HAARNESS_CLOUD_API_URL: api!,
    HAARNESS_CLOUD_WEB_ORIGIN: web!,
    HAARNESS_CLOUD_CONFIG_DIR: account!,
  });
  const published = () =>
    JSON.parse(execFileSync(binary, ["--workspace", project, "remote", "sessions"], { env, encoding: "utf8", timeout: 30000 })).items as { id: string; connected: boolean }[];
  try {
    await chat(page, "hello from desktop");
    await command(page, "/rc");
    const dialog = page.getByRole("dialog", { name: "Remote control" });
    await expect(dialog.getByText("Live in Haarness Cloud")).toBeVisible({ timeout: 60000 });
    const link = (await dialog.locator(".remote-link code").textContent())!;
    expect(link).toMatch(new RegExp("^" + web!.replace(/[.*+?^${}()|[\]\\]/g, "\\$&") + "/app/remote/sessions/[0-9a-f-]{36}$"));
    const id = link.split("/").pop()!;
    expect(published().find((item) => item.id === id)?.connected).toBe(true);
    // A real code: one path of dark modules on a white square.
    await expect(dialog.locator("svg.remote-qr path")).toHaveAttribute("d", /^M\d+ \d+h\d+/);
    await dialog.locator("svg.remote-qr").screenshot({ path: test.info().outputPath("qr.png") });
    await dialog.screenshot({ path: test.info().outputPath("dialog.png") });
    await dialog.getByRole("button", { name: "Open in browser" }).click();
    await expect.poll(() => app.evaluate(() => (globalThis as unknown as { opened: string[] }).opened)).toContain(link);
    await dialog.getByRole("button", { name: "Done" }).click();

    // The composer says the chat is reachable, and reopens the same link.
    const pill = page.locator(".composer").getByRole("button", { name: "Remote control" });
    await expect(pill).toBeVisible();
    await page.locator(".composer").screenshot({ path: test.info().outputPath("composer.png") });
    await pill.click();
    await expect(dialog.locator(".remote-link code")).toHaveText(link);
    await dialog.getByRole("button", { name: "Done" }).click();
    // So does the titlebar, with its dot lit.
    const titlebar = page.locator(".titlebar").getByRole("button", { name: "Remote control" });
    await expect(titlebar).toHaveAttribute("aria-pressed", "true");
    await expect(titlebar).toHaveAttribute("aria-description", "Live in Haarness Cloud");
    await expect(titlebar.locator(".icon-button-dot")).toHaveCount(1);
    await page.locator(".titlebar").screenshot({ path: test.info().outputPath("titlebar-live.png") });
    await titlebar.click();
    await expect(dialog.locator(".remote-link code")).toHaveText(link);
    await dialog.getByRole("button", { name: "Stop sharing" }).click();
    await expect(dialog.getByRole("button", { name: "Share this chat" })).toBeVisible({ timeout: 30000 });
    await expect(pill).toBeHidden();
    await expect(titlebar).toHaveAttribute("aria-pressed", "false");
    await expect(titlebar.locator(".icon-button-dot")).toHaveCount(0);
    expect(published().some((item) => item.id === id)).toBe(false);
  } finally {
    await command(page, "/remote-control stop").catch(() => {});
    await close();
  }
});
