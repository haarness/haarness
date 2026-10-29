import { test, expect, _electron, type ElectronApplication, type Page } from "@playwright/test";
import { mkdtemp, mkdir, readdir, readFile, realpath, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { homedir, tmpdir } from "node:os";
import { execFileSync } from "node:child_process";
import { stopChatHome } from "./fixtures/chat-home";

/**
 * The title bar's remote access control: one press makes this whole computer
 * reachable through Haarness Cloud, with no code or link, and another press
 * ends that. The signed-out case needs nothing but the app. The live case
 * turns access on for a signed-in account and opens the computer's projects
 * through Cloud's relay, as a phone or a browser does:
 *
 *   HAARNESS_REMOTE_ACCESS_API=https://haarness.ai \
 *   HAARNESS_REMOTE_ACCESS_ACCOUNT=<signed-in HAARNESS_CLOUD_CONFIG_DIR> \
 *   npx playwright test e2e/remote-access.spec.ts
 *
 * It registers a computer of its own in that account and removes it again.
 */
const binary = resolve(process.env.HAARNESS_BIN ?? "../../target/debug/haarness");
const api = process.env.HAARNESS_REMOTE_ACCESS_API;
const account = process.env.HAARNESS_REMOTE_ACCESS_ACCOUNT;

interface Launched {
  app: ElectronApplication;
  page: Page;
  env: Record<string, string>;
  close: () => Promise<void>;
}

async function scratch() {
  const root = await realpath(await mkdtemp(join(tmpdir(), "haarness-remote-access-")));
  const project = join(root, "project");
  await mkdir(join(project, ".haarness"), { recursive: true });
  await writeFile(join(project, ".haarness/config.toml"), '[defaults]\nprovider="echo"\nmodel="echo-1"\n[lsp]\nenabled=false\n');
  return { root, project };
}

async function launch(root: string, project: string, cloud: Record<string, string>, open = true): Promise<Launched> {
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
  await app.evaluate(({ dialog }, path) => {
    dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [path] });
  }, project);
  const page = await app.firstWindow();
  page.setDefaultTimeout(15000);
  if (open) {
    await page.getByRole("button", { name: "Open another project", exact: true }).click();
    await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 60000 });
    await expect(page.locator(".titlebar-caption")).toHaveText("project");
  }
  const close = async () => {
    await app.evaluate(({ BrowserWindow }) => {
      for (const win of BrowserWindow.getAllWindows()) win.destroy();
    }).catch(() => {});
    await app.close().catch(() => {});
    try { execFileSync(binary, ["--workspace", project, "serve", "--stop"], { env, stdio: "ignore", timeout: 30000 }); } catch {}
    stopChatHome(binary, env);
  };
  return { app, page, env, close };
}

const control = (page: Page) => page.locator(".titlebar").getByRole("button", { name: "Remote access" });

async function chat(page: Page, text: string) {
  const message = page.getByRole("textbox", { name: "Message" });
  await message.fill(text);
  await message.press("Enter");
  await expect(page.locator(".transcript")).toContainText("Echo: " + text, { timeout: 60000 });
}

/** Every Haarness log line written under `root`. */
async function logs(root: string) {
  const found: string[] = [];
  const walk = async (folder: string) => {
    for (const entry of await readdir(folder, { withFileTypes: true }).catch(() => [])) {
      const path = join(folder, entry.name);
      if (entry.isDirectory()) await walk(path);
      else if (entry.name.startsWith("haarness.log")) found.push(await readFile(path, "utf8"));
    }
  };
  await walk(root);
  return found.join("\n");
}

test("remote access belongs to the computer and asks a signed-out user to sign in first", async () => {
  const { root, project } = await scratch();
  const { page, close } = await launch(root, project, { HAARNESS_CLOUD_API_URL: "http://127.0.0.1:9" });
  try {
    // There from the start, before any chat: it is about this computer.
    await expect(control(page)).toBeVisible();
    await expect(control(page)).toHaveAttribute("aria-pressed", "false");
    await expect(control(page)).toHaveAttribute("title", /Open this computer's projects from your phone or any browser/);
    await expect(control(page).locator(".icon-button-dot")).toHaveCount(0);
    await control(page).click();
    await expect(page.getByRole("dialog", { name: "Haarness account" })).toBeVisible();
    await expect(control(page)).toHaveAttribute("aria-pressed", "false");
    await page.locator(".titlebar").screenshot({ path: test.info().outputPath("titlebar-signed-out.png") });
  } finally {
    await close();
  }
});

test("remote access makes the whole computer reachable through Cloud, stays as left, and comes back after a restart", async () => {
  test.skip(!api || !account, "Set HAARNESS_REMOTE_ACCESS_API and _ACCOUNT to run against a Cloud");
  test.setTimeout(420000);
  const { root, project } = await scratch();
  const cloud = { HAARNESS_CLOUD_API_URL: api!, HAARNESS_CLOUD_CONFIG_DIR: account! };
  let run = await launch(root, project, cloud);
  const cli = (args: string[]) =>
    execFileSync(binary, ["--workspace", project, ...args], { env: run.env, encoding: "utf8", timeout: 60000 });
  const connectors = () =>
    (JSON.parse(cli(["remote", "status", "--json"])).items as { id: string; chatId: string | null; running: boolean; workspace: string; pid: number }[])
      .filter((item) => !item.chatId);
  const rpc = (input: object) =>
    JSON.parse(execFileSync(binary, ["account", "--rpc"], { env: run.env, encoding: "utf8", input: JSON.stringify(input), timeout: 60000 }));
  /** A request through Cloud's relay to this computer, as a phone makes it. */
  const relay = async (host: string, path: string) => {
    const { token } = rpc({ action: "remote-auth" });
    const response = await fetch(`${api!.replace(/\/$/, "")}/api/v1/remote-hosts/${host}/runtime/${path}`, {
      headers: { authorization: `Bearer ${token}` },
    });
    expect(response.status, path).toBe(200);
    return response.json();
  };
  const listed = (host: string) =>
    (JSON.parse(cli(["remote", "hosts"])).items as { id: string; connected: boolean; mode: string; name: string }[]).find((item) => item.id === host);
  let host = "";
  try {
    // The project is open in Desktop, with its own server.
    await chat(run.page, "hello from desktop");
    await control(run.page).click();
    await expect(run.page.locator(".toast").filter({ hasText: "Remote access is on." })).toBeVisible({ timeout: 90000 });
    await expect(control(run.page)).toHaveAttribute("aria-pressed", "true");
    await expect(control(run.page)).toHaveClass(/\bon\b/, { timeout: 60000 });
    await expect(control(run.page)).toHaveAttribute("aria-description", /can open/);
    await run.page.locator(".titlebar").screenshot({ path: test.info().outputPath("titlebar-on.png") });

    // One connector for the whole computer, routed through Desktop's chat
    // home, and Cloud lists it as a computer, connected.
    const [connector] = connectors();
    expect(connector?.running).toBe(true);
    expect(connector.workspace).toBe(await realpath(join(root, "desktop", "Chat folders")));
    host = connector.id;
    expect(listed(host)).toMatchObject({ connected: true, mode: "host" });

    // A phone or browser sees the project and browses from the home folder.
    const catalog = await relay(host, "v1/host/projects");
    expect((catalog.workspaces as { path: string }[]).map((item) => item.path)).toContain(project);
    const folders = await relay(host, "v1/host/directories");
    expect(folders.path).toBe(await realpath(homedir()));
    const workspace = (catalog.workspaces as { id: string; path: string }[]).find((item) => item.path === project)!;
    const threads = await relay(host, `v1/host/workspaces/${workspace.id}/v1/threads`);
    expect(JSON.stringify(threads)).toContain("hello from desktop");
    // Reaching the project from Cloud used Desktop's server as it runs:
    // nothing was restarted, and Desktop keeps working on it.
    expect(await logs(root)).not.toContain("restarting incompatible Haarness workspace server");
    await chat(run.page, "still answering");

    // Off: the connector stops, Cloud keeps the computer, shown offline.
    await control(run.page).click();
    await expect(run.page.locator(".toast").filter({ hasText: "Remote access is off" })).toBeVisible({ timeout: 60000 });
    await expect(control(run.page)).toHaveAttribute("aria-pressed", "false");
    expect(connectors().find((item) => item.id === host)?.running).toBe(false);
    await expect.poll(() => listed(host)?.connected, { timeout: 60000 }).toBe(false);

    // Turned off, it stays off when Desktop starts again.
    await run.close();
    run = await launch(root, project, cloud, false);
    await expect(control(run.page)).toBeVisible({ timeout: 60000 });
    await run.page.waitForTimeout(8000);
    await expect(control(run.page)).toHaveAttribute("aria-pressed", "false");
    expect(connectors().find((item) => item.id === host)?.running).toBe(false);

    // Left on, it comes back after the computer restarts: the connector is
    // gone (as after a restart) and Desktop starts again.
    await control(run.page).click();
    await expect(control(run.page)).toHaveClass(/\bon\b/, { timeout: 90000 });
    expect(connectors().find((item) => item.id === host)?.running).toBe(true);
    await run.close();
    // Closing Desktop leaves the computer reachable.
    const left = connectors().find((item) => item.id === host)!;
    expect(left.running).toBe(true);
    process.kill(left.pid, "SIGKILL");
    await expect.poll(() => connectors().find((item) => item.id === host)?.running, { timeout: 30000 }).toBe(false);
    run = await launch(root, project, cloud, false);
    await expect(control(run.page)).toHaveClass(/\bon\b/, { timeout: 90000 });
    expect(connectors().find((item) => item.id === host)?.running).toBe(true);
    await expect.poll(() => listed(host)?.connected, { timeout: 60000 }).toBe(true);

    await control(run.page).click();
    await expect(control(run.page)).toHaveAttribute("aria-pressed", "false", { timeout: 60000 });
  } finally {
    // Whatever happened, the test's computer leaves the account.
    try { rpc({ action: "remote-access", change: "off" }); } catch {}
    if (host) try { cli(["remote", "forget", host]); } catch {}
    await run.close();
  }
});
