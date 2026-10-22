import { test, expect, _electron, type ElectronApplication, type Page } from "@playwright/test";
import { mkdtemp, mkdir, readFile, realpath, rm, writeFile, chmod } from "node:fs/promises";
import { createServer, request as forward, type IncomingMessage, type ServerResponse } from "node:http";
import { once } from "node:events";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";

/**
 * Your other computers in Desktop, through a Haarness Cloud this test plays:
 * the account's host list and the relay to a real `haarness serve` host
 * gateway standing in for another computer. The live case uses a real local
 * Cloud with real connectors:
 *
 *   HAARNESS_REMOTE_HOSTS_API=http://127.0.0.1:19080 \
 *   HAARNESS_REMOTE_HOSTS_ACCOUNT=<signed-in HAARNESS_CLOUD_CONFIG_DIR> \
 *   HAARNESS_REMOTE_HOSTS_DATA=<this computer's data dir, with its connector> \
 *   HAARNESS_REMOTE_HOSTS_OTHER=<name of another computer's connector, running> \
 *   HAARNESS_REMOTE_HOSTS_SELF=<name of this computer's connector> \
 *   npx playwright test e2e/remote-hosts.spec.ts
 *
 * Screenshots go to HAARNESS_REMOTE_HOSTS_SHOTS when it is set.
 */
const binary = resolve(process.env.HAARNESS_BIN ?? "../../target/debug/haarness");
const shots = process.env.HAARNESS_REMOTE_HOSTS_SHOTS;
const account = "00000000-0000-4000-8000-00000000c001";
const studio = "6a0c1b2e-0000-4000-8000-000000000001";
const away = "6a0c1b2e-0000-4000-8000-000000000002";
const self = "6a0c1b2e-0000-4000-8000-000000000003";
const shared = "6a0c1b2e-0000-4000-8000-000000000004";

async function shoot(page: Page, name: string, theme?: "light" | "dark") {
  if (!shots) return;
  await mkdir(shots, { recursive: true });
  for (const mode of theme ? [theme] : (["light", "dark"] as const)) {
    await page.evaluate((value) => window.haarness.preferences({ theme: value }), mode);
    await page.waitForTimeout(250);
    await page.screenshot({ path: join(shots, `${name}-${mode}.png`) });
  }
  await page.evaluate(() => window.haarness.preferences({ theme: "light" }));
}

/** The Cloud of this test: what it was asked, and the computer it relays to. */
async function fakeCloud(gateway: { url: string; token: string }) {
  const seen: { method: string; path: string; authorization?: string }[] = [];
  const state = { offline: false, issued: 0 };
  const json = (response: ServerResponse, status: number, body: unknown) => {
    response.writeHead(status, { "Content-Type": "application/json" });
    response.end(JSON.stringify(body));
  };
  const server = createServer((request: IncomingMessage, response: ServerResponse) => {
    const url = new URL(request.url!, "http://cloud");
    seen.push({ method: request.method!, path: url.pathname, authorization: request.headers.authorization });
    if (url.pathname === "/oauth/token") {
      state.issued++;
      return json(response, 200, { access_token: `renewed-${state.issued}`, refresh_token: `refresh-${state.issued}`, expires_in: 600 });
    }
    if (url.pathname === "/api/v1/me")
      return json(response, 200, {
        user: { id: "u1", displayName: "Remote hosts test", email: "hosts@fixture.invalid" },
        accounts: [{ id: account, name: "Remote hosts test", role: "owner", kind: "personal" }],
        defaultAccountId: account,
      });
    if (url.pathname === "/api/v1/remote-hosts") {
      const host = (id: string, name: string, connected: boolean, mode = "host", hoursAgo = 0) => ({
        id, accountId: account, name, mode, connected,
        metadata: { platform: "macos", version: "0.1.3" },
        lastSeenAt: new Date(Date.now() - hoursAgo * 3_600_000).toISOString(),
        createdAt: "2026-10-01T00:00:00Z",
      });
      return json(response, 200, {
        items: [
          host(away, "Old Mac", false, "host", 3),
          host(studio, "Studio", true),
          host(self, "This Mac", true),
          host(shared, "Shared investigation", true, "sessions"),
        ],
        nextCursor: null,
      });
    }
    const runtime = /^\/api\/v1\/remote-hosts\/([^/]+)\/runtime\/(.+)$/.exec(url.pathname);
    if (runtime && runtime[1] === studio && !state.offline) {
      const upstream = new URL(gateway.url);
      const proxied = forward(
        {
          host: upstream.hostname,
          port: upstream.port,
          method: request.method,
          path: "/" + runtime[2] + url.search,
          headers: {
            ...(request.headers["content-type"] ? { "content-type": request.headers["content-type"] } : {}),
            ...(request.headers.accept ? { accept: request.headers.accept } : {}),
            ...(request.headers["last-event-id"] ? { "last-event-id": request.headers["last-event-id"] } : {}),
            authorization: `Bearer ${gateway.token}`,
          },
        },
        (answer) => {
          response.writeHead(answer.statusCode ?? 502, { "content-type": answer.headers["content-type"] ?? "application/json", "cache-control": "no-store" });
          answer.pipe(response);
        },
      );
      proxied.on("error", () => response.destroy());
      request.pipe(proxied);
      response.on("close", () => proxied.destroy());
      return;
    }
    if (runtime && (runtime[1] === away || runtime[1] === studio))
      return json(response, 409, { error: { code: "conflict", message: "Remote host is offline or its connection changed. Reconnect and check the current state before retrying.", retryable: false } });
    json(response, 404, { error: { code: "not_found", message: "Not found" } });
  }).listen(0, "127.0.0.1");
  await once(server, "listening");
  const { port } = server.address() as { port: number };
  return {
    url: `http://127.0.0.1:${port}`,
    seen,
    state,
    close: async () => {
      server.closeAllConnections();
      await new Promise<void>((done) => server.close(() => done()));
    },
  };
}

async function fixture() {
  const root = await realpath(await mkdtemp(join(tmpdir(), "haarness-remote-hosts-")));
  const folders = Object.fromEntries(
    ["home", "data", "desktop", "account", "local", "studio-data", "studio-home", "studio-project"].map((name) => [name, join(root, name)]),
  );
  for (const folder of Object.values(folders)) await mkdir(folder, { recursive: true });
  for (const project of [folders.local, folders["studio-project"]]) {
    await mkdir(join(project, ".haarness"), { recursive: true });
    await writeFile(join(project, ".haarness/config.toml"), '[defaults]\nprovider="echo"\nmodel="echo-1"\n[lsp]\nenabled=false\n');
  }
  await writeFile(join(folders["studio-project"], "README.md"), "# Studio project\n");
  // This computer shares itself too: its own connector never lists it.
  await mkdir(join(folders.data, "config"), { recursive: true });
  await writeFile(join(folders.data, "config/remote-self.json"), JSON.stringify({ id: self, name: "This Mac" }));
  const token = crypto.randomUUID();
  const base = Object.fromEntries(Object.entries(process.env).filter((entry): entry is [string, string] => entry[1] !== undefined));
  delete base.ELECTRON_RUN_AS_NODE;
  delete base.ELECTRON_RENDERER_URL;
  delete base.HAARNESS_API_KEY;
  const studioEnv = {
    ...base,
    HAARNESS_HOME: folders["studio-home"],
    HAARNESS_DATA_DIR: folders["studio-data"],
    HAARNESS_SERVER_TOKEN: token,
    HAARNESS_TEST_PROVIDER: "1",
  };
  execFileSync(binary, ["--workspace", folders["studio-project"], "serve", "--daemon", "--host", "127.0.0.1", "--port", "0"], { env: studioEnv, encoding: "utf8", timeout: 60000 });
  const registry = JSON.parse(await readFile(join(folders["studio-data"], "data/workspaces.json"), "utf8"));
  const workspace = registry.workspaces.find((item: { path: string }) => item.path === folders["studio-project"]);
  const runtime = JSON.parse(await readFile(join(folders["studio-data"], "cache/workspaces", workspace.id, "runtime.json"), "utf8"));
  const cloud = await fakeCloud({ url: runtime.url, token });
  const credentials = join(folders.account, "credentials.json");
  await writeFile(credentials, JSON.stringify({ apiUrl: cloud.url + "/", accessToken: "first", accessExpiresAt: new Date(Date.now() + 600_000).toISOString(), refreshToken: "refresh-0" }));
  await chmod(credentials, 0o600);
  const env = {
    ...base,
    HAARNESS_BIN: binary,
    HAARNESS_HOME: folders.home,
    HAARNESS_DATA_DIR: folders.data,
    HAARNESS_DESKTOP_DATA: folders.desktop,
    HAARNESS_CLOUD_CONFIG_DIR: folders.account,
    HAARNESS_CLOUD_API_URL: cloud.url,
    HAARNESS_CLOUD_WEB_ORIGIN: cloud.url,
    HAARNESS_TEST_PROVIDER: "1",
    HAARNESS_DESKTOP_TEST: "1",
  };
  const stop = async () => {
    for (const [folder, environment] of [[folders["studio-project"], studioEnv], [folders.local, env]] as const)
      try { execFileSync(binary, ["--workspace", folder, "serve", "--stop"], { env: environment, stdio: "ignore", timeout: 30000 }); } catch {}
    await cloud.close();
    await rm(root, { recursive: true, force: true }).catch(() => {});
  };
  return { root, folders, env, cloud, credentials, stop };
}

async function launch(env: Record<string, string>, folder: string) {
  const app = await _electron.launch({ args: ["."], env, timeout: 60000 });
  await app.evaluate(({ dialog }, path) => {
    dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [path] });
  }, folder);
  const page = await app.firstWindow();
  page.setDefaultTimeout(20000);
  await expect(page.getByRole("button", { name: "Switch server", exact: true })).toBeVisible();
  return { app, page };
}
async function close(app: ElectronApplication) {
  await app.evaluate(({ BrowserWindow }) => {
    for (const win of BrowserWindow.getAllWindows()) win.destroy();
  }).catch(() => {});
  await app.close().catch(() => {});
}
const listed = (seen: { path: string }[]) => seen.filter((item) => item.path === "/api/v1/remote-hosts").length;

test("your other computers open from the server switcher through Haarness Cloud, never at launch", async () => {
  test.setTimeout(300000);
  const { folders, env, cloud, stop } = await fixture();
  let app: ElectronApplication | undefined;
  try {
    let page: Page;
    ({ app, page } = await launch(env, folders.local));
    await page.getByRole("region", { name: "Project home" }).getByRole("button", { name: "Open project", exact: true }).click();
    await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 60000 });
    await page.waitForTimeout(1500);
    // Starting the app and opening a project never asks Cloud for computers.
    expect(listed(cloud.seen)).toBe(0);

    // The switcher lists them when it opens: connected first, away ones
    // quieter with when they were last seen; never this computer or a shared chat.
    await page.getByRole("button", { name: "Switch server", exact: true }).click();
    let menu = page.getByRole("menu", { name: "Switch server" });
    await expect(menu.getByText("Your computers", { exact: true })).toBeVisible();
    const studioItem = menu.getByRole("menuitem", { name: "Studio", exact: true });
    await expect(studioItem).toBeEnabled();
    await expect(studioItem).toHaveAttribute("aria-description", "macOS");
    const awayItem = menu.getByRole("menuitem", { name: "Old Mac", exact: true });
    await expect(awayItem).toBeDisabled();
    await expect(awayItem).toHaveAttribute("aria-description", /^Last seen 3/);
    await expect(menu.getByRole("menuitem", { name: "This Mac" })).toHaveCount(0);
    await expect(menu.getByRole("menuitem", { name: "Shared investigation" })).toHaveCount(0);
    expect(await menu.getByRole("menuitem").evaluateAll((items) => items.map((item) => item.getAttribute("aria-label"))))
      .toEqual(["This computer", "Studio", "Old Mac", "Manage servers"]);
    expect(listed(cloud.seen)).toBe(1);
    await shoot(page, "switcher-menu");
    // Keyboard: the arrows reach the computer, Enter opens it. A list read
    // once shows at once when the menu opens again.
    const switcher = page.getByRole("button", { name: "Switch server", exact: true });
    // Changing the theme for the screenshots took focus out of the menu.
    if ((await switcher.getAttribute("aria-expanded")) === "true") await switcher.click();
    await expect(switcher).toHaveAttribute("aria-expanded", "false");
    await switcher.click();
    await expect(studioItem).toBeVisible();
    await expect(menu.getByRole("menuitem", { name: "This computer", exact: true })).toBeFocused();
    await page.keyboard.press("ArrowDown");
    await expect(studioItem).toBeFocused();
    await page.keyboard.press("Enter");

    // Its home lists the projects it offers, read through the relay.
    let home = page.getByRole("region", { name: "Project home" });
    await expect(home.getByRole("heading", { name: "Choose a project on Studio" })).toBeVisible();
    await expect(home.getByText("Prompts, tools and terminals run on Studio.")).toBeVisible();
    await expect(page.locator(".server-switcher-button")).toHaveText("Studio");
    const row = home.getByRole("button", { name: /studio-project/ });
    await expect(row).toBeVisible();
    await shoot(page, "computer-home");
    await row.click();
    await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 60000 });
    // Where prompts run is named on every screen.
    await expect(page.locator(".titlebar .host-badge")).toHaveText("on Studio");

    const message = page.getByRole("textbox", { name: "Message" });
    await message.fill("hello from the other desk");
    await message.press("Enter");
    await expect(page.locator(".transcript")).toContainText("Echo: hello from the other desk", { timeout: 30000 });
    await shoot(page, "computer-chat");
    // Everything went through Cloud with the account's own sign-in.
    const relayed = cloud.seen.filter((item) => item.path.startsWith(`/api/v1/remote-hosts/${studio}/runtime/`));
    expect(relayed.some((item) => item.path.endsWith("/v1/events"))).toBe(true);
    expect(relayed.some((item) => /\/v1\/threads\/[^/]+\/turns$/.test(item.path))).toBe(true);
    expect(new Set(relayed.map((item) => item.authorization))).toEqual(new Set(["Bearer first"]));
    // The saved project names the computer and holds no credential.
    const saved = await page.evaluate(() => window.haarness.boot());
    const location = saved.recent.find((item) => item.remoteHost);
    expect(location).toMatchObject({ remoteHost: studio, remote: true, trust: false, label: "studio-project" });
    expect(JSON.stringify(saved)).not.toContain("first");
    expect(saved.servers).toContainEqual({ id: "host:" + studio, label: "Studio", host: studio });

    // The terminal bar says where its commands run.
    await page.getByRole("button", { name: "Toggle terminal", exact: true }).click();
    await expect(page.locator(".terminal-tabs .host-badge")).toHaveText("on Studio");
    await page.getByRole("button", { name: "Hide terminal", exact: true }).click();

    // Settings → Servers lists them too, below the servers you added.
    await page.getByRole("button", { name: "Settings", exact: true }).click();
    const dialog = page.getByRole("dialog", { name: "Settings", exact: true });
    await dialog.getByRole("button", { name: "Servers", exact: true }).click();
    const section = dialog.getByRole("region", { name: "Your computers" });
    await expect(section.locator(".computer-row").filter({ hasText: "Studio" }).getByRole("button")).toHaveText("Selected");
    const awayRow = section.locator(".computer-row").filter({ hasText: "Old Mac" });
    await expect(awayRow).toHaveClass(/away/);
    await expect(awayRow.getByRole("button", { name: "Switch" })).toBeDisabled();
    await expect(dialog.locator(".setting-row").filter({ hasText: "Studio" })).toHaveCount(1);
    await shoot(page, "settings-servers");
    await dialog.getByRole("button", { name: "Close settings", exact: true }).click();

    // Back to this computer and to the other one: each reopens its project.
    await page.getByRole("button", { name: "Switch server", exact: true }).click();
    await menu.getByRole("menuitem", { name: "This computer", exact: true }).click();
    await expect(page.locator(".server-switcher-button")).toHaveText("This computer");
    await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
    await expect(page.locator(".titlebar .host-badge")).toHaveCount(0);
    await page.getByRole("button", { name: "Switch server", exact: true }).click();
    await menu.getByRole("menuitem", { name: "Studio", exact: true }).click();
    await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
    await expect(page.locator(".titlebar .host-badge")).toHaveText("on Studio");

    // Relaunched, Desktop comes back to that computer without listing them.
    const before = listed(cloud.seen);
    await close(app);
    ({ app, page } = await launch(env, folders.local));
    menu = page.getByRole("menu", { name: "Switch server" });
    home = page.getByRole("region", { name: "Project home" });
    await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 60000 });
    await expect(page.locator(".titlebar .host-badge")).toHaveText("on Studio");
    await page.waitForTimeout(1000);
    expect(listed(cloud.seen)).toBe(before);

    // A computer that went away since says so, and Retry opens it once it is back.
    cloud.state.offline = true;
    await page.getByRole("button", { name: "Switch server", exact: true }).click();
    await menu.getByRole("menuitem", { name: "This computer", exact: true }).click();
    await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
    await page.getByRole("button", { name: "Switch server", exact: true }).click();
    await menu.getByRole("menuitem", { name: "Studio", exact: true }).click();
    await expect(home.getByRole("alert")).toHaveText("This computer is offline. Try again when it is awake and connected.");
    await shoot(page, "computer-offline");
    cloud.state.offline = false;
    await home.getByRole("button", { name: "Retry", exact: true }).click();
    await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 60000 });
    await expect(page.locator(".titlebar .host-badge")).toHaveText("on Studio");
  } finally {
    if (app) await close(app);
    await stop();
  }
});

test("signed out, the switcher shows no computers and Settings says how to see them", async () => {
  const { folders, env, cloud, credentials, stop } = await fixture();
  await rm(credentials);
  let app: ElectronApplication | undefined;
  try {
    let page: Page;
    ({ app, page } = await launch(env, folders.local));
    await page.getByRole("button", { name: "Switch server", exact: true }).click();
    const menu = page.getByRole("menu", { name: "Switch server" });
    await expect(menu.getByRole("menuitem", { name: "Manage servers" })).toBeVisible();
    await page.waitForTimeout(1500);
    await expect(menu.getByText("Your computers", { exact: true })).toHaveCount(0);
    await page.keyboard.press("Escape");
    await page.getByRole("button", { name: "Settings", exact: true }).click();
    const dialog = page.getByRole("dialog", { name: "Settings", exact: true });
    await dialog.getByRole("button", { name: "Servers", exact: true }).click();
    const section = dialog.getByRole("region", { name: "Your computers" });
    await expect(section.getByText("Sign in to Haarness to open your other computers here.")).toBeVisible();
    await expect(section.getByRole("button", { name: "Sign in", exact: true })).toBeVisible();
    await expect(section.getByRole("button", { name: "Refresh" })).toHaveCount(0);
    await shoot(page, "settings-signed-out");
    // Signed out, nothing was asked of Cloud for them.
    expect(listed(cloud.seen)).toBe(0);
    // Sign in opens the account's own sign-in, above Settings.
    await section.getByRole("button", { name: "Sign in", exact: true }).click();
    await expect(page.getByRole("dialog", { name: "Haarness account" })).toBeVisible();
  } finally {
    if (app) await close(app);
    await stop();
  }
});
