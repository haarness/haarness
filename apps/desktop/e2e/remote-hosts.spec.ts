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
 * gateway standing in for another computer. The live case runs against a real
 * local Cloud with real connectors, set up by scripts/test-remote-hosts-live.py
 * (HAARNESS_REMOTE_HOSTS_LONG=1 also waits past a relayed sign-in's lifetime).
 *
 * Screenshots go to HAARNESS_REMOTE_HOSTS_SHOTS when it is set.
 */
const binary = resolve(process.env.HAARNESS_BIN ?? "../../target/debug/haarness");
const shots = process.env.HAARNESS_REMOTE_HOSTS_SHOTS;
/** The live case also waits past a relayed sign-in's ten minutes. */
const long = process.env.HAARNESS_REMOTE_HOSTS_LONG === "1";
const account = "00000000-0000-4000-8000-00000000c001";
const studio = "6a0c1b2e-0000-4000-8000-000000000001";
const away = "6a0c1b2e-0000-4000-8000-000000000002";
const self = "6a0c1b2e-0000-4000-8000-000000000003";
const shared = "6a0c1b2e-0000-4000-8000-000000000004";
const otherAccount = "00000000-0000-4000-8000-00000000c002";
const session = "6a0c1b2e-0000-4000-8000-0000000000c5";

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
  const seen: { method: string; path: string; authorization?: string; away?: boolean }[] = [];
  const state = { offline: false, issued: 0, renamed: false, rejectFirst: false, revoked: false, listDelay: 0 };
  const streams = new Set<ServerResponse>();
  const json = (response: ServerResponse, status: number, body: unknown) => {
    response.writeHead(status, { "Content-Type": "application/json" });
    response.end(JSON.stringify(body));
  };
  const server = createServer((request: IncomingMessage, response: ServerResponse) => {
    const url = new URL(request.url!, "http://cloud");
    seen.push({ method: request.method!, path: url.pathname, authorization: request.headers.authorization, away: state.offline });
    if (url.pathname === "/oauth/token") {
      state.issued++;
      return json(response, 200, { access_token: `renewed-${state.issued}`, refresh_token: `refresh-${state.issued}`, expires_in: 600 });
    }
    if (url.pathname === "/oauth/revoke") return json(response, 200, {});
    if (url.pathname === "/api/v1/me")
      return json(response, 200, {
        user: { id: "u1", displayName: "Remote hosts test", email: "hosts@fixture.invalid" },
        accounts: [
          { id: account, name: "Remote hosts test", role: "owner", kind: "personal" },
          { id: otherAccount, name: "Team", role: "owner", kind: "team" },
        ],
        defaultAccountId: account,
      });
    // A Cloud session: its ticket and its gateway, which wants the ticket.
    if (url.pathname === `/api/v1/sessions/${session}`)
      return json(response, 200, { id: session, title: "Fake session", workspace: { id: session } });
    if (url.pathname === `/api/v1/sessions/${session}/attach`)
      return json(response, 200, { ticket: "ticket-1", gatewayUrl: `http://${request.headers.host}/gateway/${session}`, expiresAt: new Date(Date.now() + 600_000).toISOString() });
    const gatewayPath = url.pathname.startsWith(`/gateway/${session}/`) ? url.pathname.slice(`/gateway/${session}`.length) : undefined;
    if (gatewayPath && request.headers.authorization !== "Bearer ticket-1")
      return json(response, 401, { error: { code: "unauthorized", message: "Missing ticket" } });
    if (url.pathname === "/api/v1/remote-hosts") {
      const host = (id: string, name: string, connected: boolean, mode = "host", hoursAgo = 0) => ({
        id, accountId: account, name, mode, connected,
        metadata: { platform: "macos", version: "0.1.3" },
        lastSeenAt: new Date(Date.now() - hoursAgo * 3_600_000).toISOString(),
        createdAt: "2026-10-01T00:00:00Z",
      });
      const items = [
        host(away, "Old Mac", false, "host", 3),
        ...(state.revoked ? [] : [host(studio, state.renamed ? "Studio Mac" : "Studio", true)]),
        host(self, "This Mac", true),
        host(shared, "Shared investigation", true, "sessions"),
      ];
      setTimeout(() => json(response, 200, { items, nextCursor: null }), state.listDelay);
      return;
    }
    const runtime = /^\/api\/v1\/remote-hosts\/([^/]+)\/runtime\/(.+)$/.exec(url.pathname);
    // An expired sign-in, as Cloud refuses it.
    if (runtime && state.rejectFirst && request.headers.authorization === "Bearer first")
      return json(response, 401, { error: { code: "unauthorized", message: "Access token expired" } });
    const target = gatewayPath ?? (runtime && runtime[1] === studio && !state.offline ? "/" + runtime[2] : undefined);
    if (target) {
      if (url.pathname.endsWith("/v1/events")) {
        streams.add(response);
        response.on("close", () => streams.delete(response));
      }
      const upstream = new URL(gateway.url);
      const proxied = forward(
        {
          host: upstream.hostname,
          port: upstream.port,
          method: request.method,
          path: target + url.search,
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
    /** Ends every relayed event stream at once, as Cloud does when the
     * sign-in a stream opened with expires. */
    cutStreams: () => {
      for (const stream of streams) stream.destroy();
    },
    /** How many relayed event streams are open now. */
    openStreams: () => streams.size,
    close: async () => {
      server.closeAllConnections();
      await new Promise<void>((done) => server.close(() => done()));
    },
  };
}

async function fixture(options: { expiresInMs?: number } = {}) {
  const root = await realpath(await mkdtemp(join(tmpdir(), "haarness-remote-hosts-")));
  const folders = Object.fromEntries(
    ["home", "data", "desktop", "account", "local", "studio-data", "studio-home", "studio-project", "studio-second"].map((name) => [name, join(root, name)]),
  );
  for (const folder of Object.values(folders)) await mkdir(folder, { recursive: true });
  for (const project of [folders.local, folders["studio-project"], folders["studio-second"]]) {
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
  // A second project of that computer, with a server of its own that can
  // start again.
  const opened = await fetch(runtime.url + "/v1/host/open", {
    method: "POST",
    headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json" },
    body: JSON.stringify({ path: folders["studio-second"] }),
  });
  expect(opened.ok).toBe(true);
  const cloud = await fakeCloud({ url: runtime.url, token });
  const credentials = join(folders.account, "credentials.json");
  await writeFile(credentials, JSON.stringify({ apiUrl: cloud.url + "/", accessToken: "first", accessExpiresAt: new Date(Date.now() + (options.expiresInMs ?? 600_000)).toISOString(), refreshToken: "refresh-0" }));
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
    // A relayed stream counts as having worked after 2 s, not 30.
    HAARNESS_DESKTOP_RELAY_RESUME_MS: "2000",
  };
  const stop = async () => {
    for (const [folder, environment] of [[folders["studio-second"], studioEnv], [folders["studio-project"], studioEnv], [folders.local, env]] as const)
      try { execFileSync(binary, ["--workspace", folder, "serve", "--stop"], { env: environment, stdio: "ignore", timeout: 30000 }); } catch {}
    await cloud.close();
    await rm(root, { recursive: true, force: true }).catch(() => {});
  };
  return { root, folders, env, studioEnv, cloud, credentials, stop };
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

    // Settings → Servers lists them too, below the servers you added; search
    // finds the place by its name.
    await page.locator(".sidebar").getByRole("button", { name: "Search", exact: true }).click();
    await page.getByRole("combobox", { name: "Search Haarness", exact: true }).fill("other computers");
    await page.getByRole("option", { name: "Your computers", exact: true }).click();
    const dialog = page.getByRole("dialog", { name: "Settings", exact: true });
    await expect(dialog.getByRole("button", { name: "Servers", exact: true })).toHaveClass(/active/);
    const section = dialog.getByRole("region", { name: "Your computers" });
    // The computer shown is marked Current, with nothing to press.
    const studioRow = section.locator(".computer-row").filter({ hasText: "Studio" });
    await expect(studioRow.locator(".current-tag")).toHaveText("Current");
    await expect(studioRow.getByRole("button")).toHaveCount(0);
    const awayRow = section.locator(".computer-row").filter({ hasText: "Old Mac" });
    await expect(awayRow).toHaveClass(/away/);
    await expect(awayRow.getByRole("button", { name: "Switch" })).toBeDisabled();
    await expect(dialog.locator(".setting-row").filter({ hasText: "Studio" })).toHaveCount(1);
    // The place search found is marked for a moment.
    await expect(section).not.toHaveClass(/setting-search-target/, { timeout: 10000 });
    await shoot(page, "settings-servers");
    await dialog.getByRole("button", { name: "Close settings", exact: true }).click();

    // Right to left, the tag and the computers read in the reader's order.
    if (shots) {
      await page.evaluate(() => window.haarness.preferences({ language: "ar" }));
      await expect(page.locator(".titlebar .host-badge")).toHaveText("على Studio");
      await page.getByRole("button", { name: "تبديل الخادم", exact: true }).click();
      await expect(page.getByRole("menu").getByText("حواسيبك", { exact: true })).toBeVisible();
      await shoot(page, "switcher-menu-ar", "light");
      await page.evaluate(() => window.haarness.preferences({ language: "en" }));
      await expect(page.locator(".titlebar .host-badge")).toHaveText("on Studio");
      if ((await switcher.getAttribute("aria-expanded")) === "true") await switcher.click();
      await expect(switcher).toHaveAttribute("aria-expanded", "false");
    }

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

    // Renamed in Cloud, the computer takes its new name once listed again.
    cloud.state.renamed = true;
    const toggle = page.getByRole("button", { name: "Switch server", exact: true });
    await toggle.click();
    await expect(menu.getByRole("menuitem", { name: "Studio Mac", exact: true })).toBeVisible();
    await expect(page.locator(".server-switcher-button")).toHaveText("Studio Mac");
    if ((await toggle.getAttribute("aria-expanded")) === "true") await toggle.click();
    await expect(toggle).toHaveAttribute("aria-expanded", "false");
    await expect(page.locator(".titlebar .host-badge")).toHaveText("on Studio Mac");

    // Back on this computer, a relaunch reaches nothing through Cloud: the
    // other computer's projects stay quiet until it is shown again.
    await page.getByRole("button", { name: "Switch server", exact: true }).click();
    await menu.getByRole("menuitem", { name: "This computer", exact: true }).click();
    await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
    await close(app);
    const quiet = cloud.seen.length;
    ({ app, page } = await launch(env, folders.local));
    await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 60000 });
    await page.waitForTimeout(3000);
    expect(cloud.seen.slice(quiet).filter((item) => item.path.startsWith("/api/v1/remote-hosts"))).toEqual([]);
  } finally {
    if (app) await close(app);
    await stop();
  }
});

test("a relayed event stream Cloud ends when a sign-in expires follows on without a reconnect", async () => {
  test.setTimeout(120000);
  const { folders, env, cloud, stop } = await fixture();
  let app: ElectronApplication | undefined;
  try {
    let page: Page;
    ({ app, page } = await launch(env, folders.local));
    await page.getByRole("button", { name: "Switch server", exact: true }).click();
    await page.getByRole("menu", { name: "Switch server" }).getByRole("menuitem", { name: "Studio", exact: true }).click();
    await page.getByRole("region", { name: "Project home" }).getByRole("button", { name: /studio-project/ }).click();
    await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 60000 });
    await page.evaluate(() => {
      const target = document.querySelector(".application")!;
      const flag = window as unknown as { dropped: number };
      flag.dropped = 0;
      new MutationObserver(() => {
        if (target.getAttribute("data-project-ready") !== "true") flag.dropped++;
      }).observe(target, { attributes: true, attributeFilter: ["data-project-ready"] });
    });
    // A stream that has worked a while (2 s here) is cut, the sign-in it
    // used is refused from now on, and the computer is away for a moment, as
    // when its connector renews its own sign-in.
    await page.waitForTimeout(3_000);
    cloud.state.rejectFirst = true;
    cloud.state.offline = true;
    cloud.cutStreams();
    setTimeout(() => (cloud.state.offline = false), 2500);
    // The stream comes back once the computer does, with the renewed sign-in.
    await expect.poll(() => cloud.seen.filter((item) => item.path.endsWith("/v1/events") && item.authorization === "Bearer renewed-1" && !item.away).length, { timeout: 30000 }).toBeGreaterThan(0);
    const message = page.getByRole("textbox", { name: "Message" });
    await message.fill("still connected");
    await message.press("Enter");
    await expect(page.locator(".transcript")).toContainText("Echo: still connected", { timeout: 30000 });
    expect(await page.evaluate(() => (window as unknown as { dropped: number }).dropped)).toBe(0);
    expect(cloud.state.issued).toBe(1);
  } finally {
    if (app) await close(app);
    await stop();
  }
});

/** Opens a project of Studio from the switcher: the one it had open, or the
 * one its project home lists. */
async function openStudio(page: Page, project: RegExp) {
  await page.getByRole("button", { name: "Switch server", exact: true }).click();
  await page.getByRole("menu", { name: "Switch server" }).getByRole("menuitem", { name: "Studio", exact: true }).click();
  const row = page.getByRole("region", { name: "Project home" }).getByRole("button", { name: project });
  const tag = page.locator(".titlebar .host-badge");
  await expect(row.or(tag)).toBeVisible({ timeout: 60000 });
  if (await row.isVisible()) await row.click();
  await expect(tag).toHaveText("on Studio");
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 60000 });
}
async function chat(page: Page, text: string) {
  const message = page.getByRole("textbox", { name: "Message" });
  await message.fill(text);
  await message.press("Enter");
  await expect(page.locator(".transcript")).toContainText("Echo: " + text, { timeout: 30000 });
}

test("a project server that starts again while its stream follows on is read again, not waited for", async () => {
  test.setTimeout(180000);
  const { folders, env, studioEnv, cloud, stop } = await fixture();
  let app: ElectronApplication | undefined;
  try {
    let page: Page;
    ({ app, page } = await launch(env, folders.local));
    await openStudio(page, /studio-second/);
    // Enough events that a server starting again numbers its next ones
    // below where this window has read to.
    for (const text of ["one", "two", "three", "four"]) await chat(page, text);
    await page.waitForTimeout(3_000);
    execFileSync(binary, ["--workspace", folders["studio-second"], "serve", "--stop"], { env: studioEnv, stdio: "ignore", timeout: 30000 });
    cloud.cutStreams();
    // The stream follows on with the server that runs now, and the window
    // reads the chat again: a new reply shows.
    await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 60000 });
    await chat(page, "after the restart");
    await expect(page.locator(".transcript")).toContainText("Echo: four");
  } finally {
    if (app) await close(app);
    await stop();
  }
});

test("signing out or switching accounts leaves another computer and stops reaching it; so does showing another server", async () => {
  test.setTimeout(240000);
  // The sign-in has four minutes left: it is renewed before Studio is reached.
  const { folders, env, cloud, stop } = await fixture({ expiresInMs: 4 * 60_000 });
  let app: ElectronApplication | undefined;
  const relayed = (from: number) => cloud.seen.slice(from).filter((item) => item.path.startsWith("/api/v1/remote-hosts/") || item.path === "/oauth/token");
  try {
    let page: Page;
    ({ app, page } = await launch(env, folders.local));
    await page.getByRole("region", { name: "Project home" }).getByRole("button", { name: "Open project", exact: true }).click();
    await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 60000 });
    await openStudio(page, /studio-project/);
    const studioRequests = cloud.seen.filter((item) => item.path.startsWith(`/api/v1/remote-hosts/${studio}/runtime/`));
    expect(studioRequests.length).toBeGreaterThan(0);
    expect(new Set(studioRequests.map((item) => item.authorization))).toEqual(new Set(["Bearer renewed-1"]));
    // The Browser belongs to projects on this computer and your servers.
    await page.locator(".sidebar").getByRole("button", { name: "Search", exact: true }).click();
    await page.getByRole("combobox", { name: "Search Haarness", exact: true }).fill("Open browser");
    await expect(page.getByRole("option", { name: "Open browser", exact: true })).toHaveAttribute("aria-disabled", "true");
    await page.keyboard.press("Escape");

    // Shown another server, the computer's event streams end and stay ended.
    await page.getByRole("button", { name: "Switch server", exact: true }).click();
    await page.getByRole("menu", { name: "Switch server" }).getByRole("menuitem", { name: "This computer", exact: true }).click();
    await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
    await expect.poll(() => cloud.openStreams(), { timeout: 15000 }).toBe(0);
    let mark = cloud.seen.length;
    await page.waitForTimeout(4_000);
    expect(relayed(mark)).toEqual([]);

    // Switching accounts with Studio's project open: back to this computer,
    // and nothing reaches Cloud for it again.
    await openStudio(page, /studio-project/);
    await page.evaluate((id) => window.haarness.account({ action: "select", id }), otherAccount);
    await expect(page.locator(".server-switcher-button")).toHaveText("This computer");
    await expect.poll(() => cloud.openStreams(), { timeout: 15000 }).toBe(0);
    mark = cloud.seen.length;
    await page.waitForTimeout(35_000);
    expect(relayed(mark)).toEqual([]);

    // The same after a sign-out.
    await page.evaluate((id) => window.haarness.account({ action: "select", id }), account);
    await openStudio(page, /studio-project/);
    await page.evaluate(() => window.haarness.account({ action: "logout" }));
    await expect(page.locator(".server-switcher-button")).toHaveText("This computer");
    await expect.poll(() => cloud.openStreams(), { timeout: 15000 }).toBe(0);
    mark = cloud.seen.length;
    await page.waitForTimeout(35_000);
    expect(relayed(mark)).toEqual([]);
  } finally {
    if (app) await close(app);
    await stop();
  }
});

test("the switcher holds a row while it reads your computers, and one the account no longer has is forgotten", async () => {
  test.setTimeout(180000);
  const { folders, env, cloud, stop } = await fixture();
  let app: ElectronApplication | undefined;
  try {
    let page: Page;
    ({ app, page } = await launch(env, folders.local));
    // Signed in, the first read takes a moment: one quiet row holds the place.
    cloud.state.listDelay = shots ? 4000 : 1500;
    await expect.poll(() => page.locator(".sidebar").textContent()).toContain("Remote hosts test");
    await page.getByRole("button", { name: "Switch server", exact: true }).click();
    const menu = page.getByRole("menu", { name: "Switch server" });
    const holder = menu.getByRole("menuitem", { name: "Looking for your computers…" });
    await expect(holder).toBeDisabled();
    await shoot(page, "switcher-loading");
    await expect(menu.getByRole("menuitem", { name: "Studio", exact: true })).toBeVisible();
    await expect(holder).toHaveCount(0);
    cloud.state.listDelay = 0;
    await menu.getByRole("menuitem", { name: "Studio", exact: true }).click();
    await page.getByRole("region", { name: "Project home" }).getByRole("button", { name: /studio-project/ }).click();
    await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 60000 });
    // An older Desktop reads the saved project as a Cloud workspace, never as
    // a folder of its own computer.
    const saved = (await page.evaluate(() => window.haarness.boot())).recent.find((item) => item.remoteHost);
    expect(saved).toMatchObject({ remoteHost: studio, cloudWorkspace: studio, remote: true });
    expect(saved?.url).toBeUndefined();

    // Removed from the account: the next list forgets it here, with its
    // saved projects, and the window goes to this computer.
    cloud.state.revoked = true;
    await page.getByRole("button", { name: "Switch server", exact: true }).click();
    await expect(menu.getByRole("menuitem", { name: "Old Mac", exact: true })).toBeVisible();
    await expect(menu.getByRole("menuitem", { name: "Studio", exact: true })).toHaveCount(0);
    await expect(page.locator(".server-switcher-button")).toHaveText("This computer");
    await expect.poll(async () => {
      const boot = await page.evaluate(() => window.haarness.boot());
      return [boot.servers.some((item) => item.host), boot.recent.some((item) => item.remoteHost)];
    }).toEqual([false, false]);
    // Search leads only to places that are there.
    if ((await page.getByRole("button", { name: "Switch server", exact: true }).getAttribute("aria-expanded")) === "true")
      await page.getByRole("button", { name: "Switch server", exact: true }).click();
    await page.locator(".sidebar").getByRole("button", { name: "Search", exact: true }).click();
    await page.getByRole("combobox", { name: "Search Haarness", exact: true }).fill("Studio");
    await expect(page.getByRole("option", { name: "Studio", exact: true })).toHaveCount(0);
  } finally {
    if (app) await close(app);
    await stop();
  }
});

test("a Cloud session's event stream and file reads carry its ticket", async () => {
  const { folders, env, cloud, stop } = await fixture();
  let app: ElectronApplication | undefined;
  try {
    let page: Page;
    ({ app, page } = await launch(env, folders.local));
    const view = await page.evaluate((id) => window.haarness.connect({ cloudSession: id }), session);
    expect(view?.cloudSession).toBe(session);
    const key = await page.evaluate((id) => window.haarness.subscribe(id, 0), view!.id);
    await page.evaluate((id) => window.haarness.files(id, "list", { path: "" }), view!.id);
    await expect.poll(() => cloud.seen.filter((item) => item.path === `/gateway/${session}/v1/events`).map((item) => item.authorization)).toContain("Bearer ticket-1");
    expect(cloud.seen.filter((item) => item.path.startsWith(`/gateway/${session}/`)).every((item) => item.authorization === "Bearer ticket-1")).toBe(true);
    expect(cloud.seen.some((item) => item.path === `/gateway/${session}/v1/files/list`)).toBe(true);
    await page.evaluate((id) => window.haarness.unsubscribe(id), key);
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
    // One quiet line: the only place a signed-out person learns computers
    // open here.
    const line = dialog.locator(".settings-computers-signed-out");
    await expect(line).toContainText("Sign in to Haarness to open your other computers here.");
    await expect(dialog.getByRole("region", { name: "Your computers" })).toHaveCount(0);
    await shoot(page, "settings-signed-out");
    // Signed out, nothing was asked of Cloud for them.
    expect(listed(cloud.seen)).toBe(0);
    // Sign in opens the account's own sign-in, above Settings.
    await line.getByRole("button", { name: "Sign in", exact: true }).click();
    await expect(page.getByRole("dialog", { name: "Haarness account" })).toBeVisible();
  } finally {
    if (app) await close(app);
    await stop();
  }
});

test("live: Desktop opens another computer through a real local Cloud and works on it", async () => {
  const api = process.env.HAARNESS_REMOTE_HOSTS_API;
  test.skip(!api, "Run scripts/test-remote-hosts-live.py, which sets up a local Cloud's computers for this");
  test.setTimeout(long ? 1_200_000 : 300_000);
  const root = await realpath(await mkdtemp(join(tmpdir(), "haarness-remote-hosts-live-")));
  const env = Object.fromEntries(Object.entries(process.env).filter((entry): entry is [string, string] => entry[1] !== undefined));
  delete env.ELECTRON_RUN_AS_NODE;
  delete env.ELECTRON_RENDERER_URL;
  Object.assign(env, {
    HAARNESS_BIN: binary,
    HAARNESS_HOME: process.env.HAARNESS_REMOTE_HOSTS_HOME!,
    HAARNESS_DATA_DIR: process.env.HAARNESS_REMOTE_HOSTS_DATA!,
    HAARNESS_CLOUD_CONFIG_DIR: process.env.HAARNESS_REMOTE_HOSTS_ACCOUNT!,
    HAARNESS_CLOUD_API_URL: api!,
    HAARNESS_CLOUD_WEB_ORIGIN: api!,
    HAARNESS_DESKTOP_DATA: join(root, "desktop"),
    HAARNESS_DESKTOP_TEST: "1",
  });
  const project = process.env.HAARNESS_REMOTE_HOSTS_PROJECT!;
  let app: ElectronApplication | undefined;
  try {
    let page: Page;
    ({ app, page } = await launch(env, root));
    const switcher = page.getByRole("button", { name: "Switch server", exact: true });
    await switcher.click();
    const menu = page.getByRole("menu", { name: "Switch server" });
    const studioItem = menu.getByRole("menuitem", { name: "Studio", exact: true });
    await expect(studioItem).toBeEnabled({ timeout: 30000 });
    await expect(menu.getByRole("menuitem", { name: "Old box", exact: true })).toBeDisabled();
    await expect(menu.getByRole("menuitem", { name: "Old box", exact: true })).toHaveAttribute("aria-description", /^Last seen/);
    // Desktop runs on Desk, whose own connector shares it: never listed.
    await expect(menu.getByRole("menuitem", { name: "Desk" })).toHaveCount(0);
    await shoot(page, "live-switcher-menu");
    if ((await switcher.getAttribute("aria-expanded")) !== "true") await switcher.click();
    await studioItem.click();

    const home = page.getByRole("region", { name: "Project home" });
    await expect(home.getByRole("heading", { name: "Choose a project on Studio" })).toBeVisible();
    await home.getByRole("button", { name: new RegExp(project.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")) }).click();
    await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 60000 });
    await expect(page.locator(".titlebar .host-badge")).toHaveText("on Studio");
    const message = page.getByRole("textbox", { name: "Message" });
    await message.fill("hello over the relay");
    await message.press("Enter");
    await expect(page.locator(".transcript")).toContainText("Echo: hello over the relay", { timeout: 60000 });
    await shoot(page, "live-chat");

    // Its terminal runs there, through the relay.
    await page.getByRole("button", { name: "Toggle terminal", exact: true }).click();
    await expect(page.locator(".terminal-tabs .host-badge")).toHaveText("on Studio");
    const terminal = page.locator(".xterm-helper-textarea");
    await expect(terminal).toBeFocused({ timeout: 30000 });
    await terminal.pressSequentially("echo relay-$((6*7)) && pwd\n");
    await expect(page.locator(".terminal-view .xterm-rows")).toContainText("relay-42", { timeout: 30000 });
    await expect(page.locator(".terminal-view .xterm-rows")).toContainText("Studio/project");
    await shoot(page, "live-terminal");

    if (long) {
      // Past the ten minutes a relayed sign-in lives, with the terminal open:
      // the project stays connected throughout, the terminal never says it
      // lost its shell, and both still work.
      await page.evaluate(() => {
        const target = document.querySelector(".application")!;
        const flags = window as unknown as { dropped: number; terminalNotes: number };
        flags.dropped = 0;
        flags.terminalNotes = 0;
        new MutationObserver(() => {
          if (target.getAttribute("data-project-ready") !== "true") flags.dropped++;
        }).observe(target, { attributes: true, attributeFilter: ["data-project-ready"] });
        new MutationObserver(() => {
          if (document.querySelector(".terminal-reconnecting, .terminal-disconnected")) flags.terminalNotes++;
        }).observe(document.body, { subtree: true, childList: true });
      });
      await page.waitForTimeout(11 * 60_000);
      await message.fill("still here after the sign-in renewed");
      await message.press("Enter");
      await expect(page.locator(".transcript")).toContainText("Echo: still here after the sign-in renewed", { timeout: 60000 });
      await page.locator(".xterm-helper-textarea").pressSequentially("echo renewed-$((7*6))\n");
      await expect(page.locator(".terminal-view .xterm-rows")).toContainText("renewed-42", { timeout: 30000 });
      expect(await page.evaluate(() => (window as unknown as { dropped: number }).dropped)).toBe(0);
      expect(await page.evaluate(() => (window as unknown as { terminalNotes: number }).terminalNotes)).toBe(0);
    }
    await page.getByRole("button", { name: "Hide terminal", exact: true }).click();

    // Another of its folders opens through its own folder browser.
    await page.locator(".sidebar").getByRole("button", { name: "Open another project", exact: true }).click();
    const picker = page.getByRole("dialog", { name: "Open remote project" });
    await expect(picker).toBeVisible();
    await expect(picker.getByText("Choose a folder on Studio.")).toBeVisible();
    const address = picker.getByRole("combobox", { name: "Remote folder" });
    await address.fill(project + "/second");
    await address.press("Enter");
    // The folder as that computer sees it, read through the relay.
    await expect(picker.getByRole("list", { name: "Remote folders" }).getByRole("button", { name: ".haarness" })).toBeVisible({ timeout: 30000 });
    await shoot(page, "live-folder-browser");
    await picker.getByRole("button", { name: "Open project", exact: true }).click();
    await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 60000 });
    await expect(page.locator(".titlebar .titlebar-caption")).toHaveText("second");
    await expect(page.locator(".titlebar .host-badge")).toHaveText("on Studio");

    // Settings shows the away computer quieter, and this one selected.
    await page.getByRole("button", { name: "Settings", exact: true }).click();
    const dialog = page.getByRole("dialog", { name: "Settings", exact: true });
    await dialog.getByRole("button", { name: "Servers", exact: true }).click();
    const section = dialog.getByRole("region", { name: "Your computers" });
    // The computer shown is marked Current, with nothing to press.
    const studioRow = section.locator(".computer-row").filter({ hasText: "Studio" });
    await expect(studioRow.locator(".current-tag")).toHaveText("Current");
    await expect(studioRow.getByRole("button")).toHaveCount(0);
    await expect(section.locator(".computer-row").filter({ hasText: "Old box" })).toHaveClass(/away/);
    await expect(section.locator(".computer-row").filter({ hasText: "Desk" })).toHaveCount(0);
    await shoot(page, "live-settings-servers");
  } finally {
    if (app) await close(app);
    await rm(root, { recursive: true, force: true }).catch(() => {});
  }
});
