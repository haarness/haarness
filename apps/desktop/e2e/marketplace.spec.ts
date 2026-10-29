import { test, expect, _electron as electron } from "@playwright/test";
import { Haarness } from "@haarness/sdk";
import { createServer } from "node:http";
import { randomUUID } from "node:crypto";
import { execFileSync } from "node:child_process";
import { mkdtemp, mkdir, readFile, writeFile, realpath, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { stopChatHome } from "./fixtures/chat-home";

// Icons live on Haarness Cloud's public host. The catalog fixture names that
// host as its web origin, and the app's own requests to it are answered here,
// so nothing reaches production.
const ICONS = "https://haarness.ai/api/v1/marketplace/fixture-icons";
// A dark mark on a transparent ground, as many vendor logos are, and an
// avatar that fills its square.
const MARK = `<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64" viewBox="0 0 64 64"><path d="M32 6 58 52H6Z" fill="#111"/><circle cx="32" cy="38" r="8" fill="#fff"/></svg>`;
const AVATAR = `<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64" viewBox="0 0 64 64"><rect width="64" height="64" fill="#e8613c"/><circle cx="32" cy="26" r="11" fill="#fff"/><path d="M12 64a20 20 0 0 1 40 0Z" fill="#fff"/></svg>`;

test("Marketplace and plugin settings install, disable and remove a scoped resource", async () => {
  const root = await realpath(await mkdtemp(join(tmpdir(), "haarness-desktop-marketplace-")));
  const workspace = join(root, "project");
  const binary = resolve(process.env.HAARNESS_BIN ?? "../../target/debug/haarness");
  const id = randomUUID(), versionId = randomUUID();
  const resource = { id, kind: "mcp", slug: "desktop-proof", name: "Desktop proof", visibility: "private", summary: "Marketplace fixture", iconUrl: `${ICONS}/proof.svg`, publisher: { slug: "fixture", displayName: "Fixture" } };
  // One icon that does not load, one from another host (left out by the
  // server), and one resource without any: none shows an empty frame.
  const others = [
    { ...resource, id: randomUUID(), slug: "avatar-icon", name: "Avatar icon", iconUrl: `${ICONS}/avatar.svg` },
    { ...resource, id: randomUUID(), slug: "broken-icon", name: "Broken icon", iconUrl: `${ICONS}/missing.svg` },
    { ...resource, id: randomUUID(), slug: "foreign-icon", name: "Foreign icon", iconUrl: "https://elsewhere.example/icon.svg" },
    { ...resource, id: randomUUID(), slug: "no-icon", name: "No icon", iconUrl: null },
  ];
  const manifest = { manifestVersion: 1, resource, version: { id: versionId, number: 1, status: "approved", yanked: false }, lock: { resourceId: id, versionId }, capabilities: {}, compatibility: {}, install: { mcp: { config: { type: "remote", url: "http://127.0.0.1:9/mcp", enabled: false }, env_placeholders: [] } } };
  const catalog = createServer((request, response) => {
    response.setHeader("content-type", "application/json");
    response.end(JSON.stringify(request.url?.includes("install-manifest") ? manifest : { items: [resource, ...others], nextCursor: null }));
  });
  await new Promise<void>((done) => catalog.listen(0, "127.0.0.1", done));
  const address = catalog.address() as { port: number };
  const env = { ...process.env, HOME: join(root, "home"), HAARNESS_HOME: join(root, "home"), HAARNESS_DATA_DIR: join(root, "state"), HAARNESS_DESKTOP_DATA: join(root, "desktop"), HAARNESS_BIN: binary, HAARNESS_TEST_PROVIDER: "1", HAARNESS_DESKTOP_TEST: "1", HAARNESS_CLOUD_API_URL: `http://127.0.0.1:${address.port}`, HAARNESS_CLOUD_WEB_ORIGIN: "https://haarness.ai" };
  delete (env as NodeJS.ProcessEnv).ELECTRON_RUN_AS_NODE;
  let app: Awaited<ReturnType<typeof electron.launch>> | undefined;
  let client: Haarness | undefined;
  try {
    for (const path of [workspace, env.HOME, env.HAARNESS_DESKTOP_DATA]) await mkdir(path, { recursive: true });
    await writeFile(join(env.HAARNESS_DESKTOP_DATA, "desktop.json"), JSON.stringify({ preferences: {}, recent: [], values: {}, credentials: {} }));
    const endpoint = JSON.parse(execFileSync(binary, ["--workspace", workspace, "desktop"], { env, input: '{"action":"connect"}', encoding: "utf8", timeout: 120000 }));
    client = new Haarness({ baseUrl: endpoint.url, token: endpoint.token });
    app = await electron.launch({ args: ["."], env, timeout: 120000 });
    const page = await app.firstWindow();
    // Answered as images. Unlike answers from the network, which the app's
    // page may read as it shows them, these are not, so here the copy
    // fetched in CORS mode is what tells the avatar from the mark.
    await app.evaluate(({ protocol, net }, [icons, mark, avatar]) => {
      protocol.handle("https", (request) => {
        if (!request.url.startsWith(icons + "/")) return net.fetch(request, { bypassCustomProtocolHandlers: true });
        const body = { [icons + "/proof.svg"]: mark, [icons + "/avatar.svg"]: avatar }[request.url];
        return body
          ? new Response(body, { headers: { "content-type": "image/svg+xml", "x-content-type-options": "nosniff" } })
          : new Response("missing", { status: 404 });
      });
    }, [ICONS, MARK, AVATAR]);
    await app.evaluate(({ dialog }, directory) => { dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [directory] }); }, workspace);
    await page.getByRole("button", { name: "Open another project", exact: true }).click();
    await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
    await page.getByRole("button", { name: "Settings", exact: true }).click();
    const settings = page.getByRole("dialog", { name: "Settings", exact: true });
    await settings.getByRole("button", { name: "Marketplace", exact: true }).click();
    const row = (name: string) => settings.getByRole("button", { name, exact: true });
    const loaded = (tile: ReturnType<typeof settings.locator>) =>
      tile.evaluate((element) => {
        const image = element.querySelector("img");
        return !!image && image.complete && image.naturalWidth > 0 && element.classList.contains("loaded");
      });
    // The tile's ground and the image's share of it.
    const fit = (tile: ReturnType<typeof settings.locator>) =>
      tile.evaluate((element) => ({
        ground: getComputedStyle(element).backgroundColor,
        share: Math.round((element.querySelector("img")!.getBoundingClientRect().width / element.clientWidth) * 100),
      }));
    const proofTile = row("Desktop proof · mcp · private").locator(".management-icon");
    const proofIcon = proofTile.locator("img");
    await expect(proofIcon).toHaveAttribute("src", `${ICONS}/proof.svg`);
    await expect(proofIcon).toHaveAttribute("alt", "");
    await expect(proofIcon).toHaveAttribute("loading", "lazy");
    await expect.poll(() => loaded(proofTile)).toBe(true);
    // The avatar fills its tile once measured; the transparent mark keeps a margin.
    const avatarTile = row("Avatar icon · mcp · private").locator(".management-icon");
    await expect(avatarTile).toHaveClass(/\bfills\b/);
    expect(await fit(avatarTile)).toEqual({ ground: "rgb(253, 252, 250)", share: 100 });
    await expect(proofTile).not.toHaveClass(/\bfills\b/);
    expect((await fit(proofTile)).share).toBeLessThan(85);
    // The failed icon is taken away; the others never had one to show.
    for (const name of ["Broken icon", "Foreign icon", "No icon"])
      await expect(row(`${name} · mcp · private`).locator(".management-icon, img")).toHaveCount(0);
    // Labels line up whether or not their row has an icon.
    const left = async (name: string) => (await row(name).locator(".management-entry-label").boundingBox())!.x;
    expect(await left("No icon · mcp · private")).toBe(await left("Desktop proof · mcp · private"));
    expect(await left("Installed resources")).toBe(await left("Desktop proof · mcp · private"));
    // The page's policy loads no image from another host.
    expect(
      await page.evaluate(
        () =>
          new Promise<string>((done) => {
            document.addEventListener("securitypolicyviolation", (event) => done(event.violatedDirective), { once: true });
            const image = new Image();
            image.src = "https://elsewhere.example/probe.png";
          }),
      ),
    ).toBe("img-src");
    // The same light tile in both themes.
    for (const scheme of ["light", "dark"] as const) {
      await app.evaluate(({ nativeTheme }, value) => { nativeTheme.themeSource = value; }, scheme);
      await page.emulateMedia({ colorScheme: scheme });
      await expect.poll(() => page.evaluate(() => matchMedia("(prefers-color-scheme: dark)").matches)).toBe(scheme === "dark");
      await page.waitForTimeout(300);
      expect((await fit(proofTile)).ground).toBe("rgb(253, 252, 250)");
      await settings.screenshot({ path: test.info().outputPath(`marketplace-icons-${scheme}.png`) });
    }
    await row("Desktop proof · mcp · private").click();
    const titleIcon = settings.locator(".management-title-icon");
    await expect(titleIcon.locator("img")).toHaveAttribute("src", `${ICONS}/proof.svg`);
    await expect.poll(() => loaded(titleIcon)).toBe(true);
    await expect(settings.getByLabel("Installation scope")).toBeVisible();
    await page.waitForTimeout(300);
    await settings.screenshot({ path: test.info().outputPath("marketplace-install-icon.png") });
    await settings.getByRole("button", { name: "Install", exact: true }).click();
    await expect.poll(() => client!.marketplace({ action: "installed" })).toMatchObject([{ id, scope: "local", iconUrl: `${ICONS}/proof.svg` }]);
    await settings.getByRole("button", { name: "Plugins", exact: true }).click();
    const proof = settings.getByRole("switch", { name: "Enable plugin mcp-desktop-proof", exact: true });
    await expect(proof).toBeChecked();
    await proof.click();
    await expect(proof).not.toBeChecked();
    await settings.getByRole("button", { name: "Marketplace", exact: true }).click();
    await settings.getByRole("button", { name: "Installed resources", exact: true }).click();
    // The installed list shows the icon its receipt recorded, once.
    // Its first row, the switch, whichever way the Plugins page left it.
    const installedIcon = settings.getByRole("button", { name: /^(Enable|Disable) desktop-proof$/ }).locator(".management-icon");
    await expect.poll(() => loaded(installedIcon)).toBe(true);
    await expect(settings.locator(".management-entries img")).toHaveCount(1);
    await page.waitForTimeout(300);
    await settings.screenshot({ path: test.info().outputPath("marketplace-installed-icon.png") });
    await settings.getByRole("button", { name: "Remove desktop-proof", exact: true }).click();
    await settings.getByRole("alertdialog").getByRole("button", { name: "Remove desktop-proof", exact: true }).click();
    await expect.poll(() => client!.marketplace({ action: "installed" })).toEqual([]);
    await expect(settings.getByRole("alert")).toHaveCount(0);
  } finally {
    client?.close();
    if (app) await app.close().catch(() => {});
    try { execFileSync(binary, ["--workspace", workspace, "serve", "--stop"], { env, stdio: "ignore", timeout: 30000 }); } catch {}
    stopChatHome(binary, env);
    catalog.close();
    await rm(root, { recursive: true, force: true });
  }
});

test("Plugin settings list Git marketplaces with Update and an automatic update switch", async () => {
  const root = await realpath(await mkdtemp(join(tmpdir(), "haarness-desktop-plugin-markets-")));
  const workspace = join(root, "project");
  const upstream = join(root, "upstream");
  const bare = join(root, "upstream.git");
  const binary = resolve(process.env.HAARNESS_BIN ?? "../../target/debug/haarness");
  const env = { ...process.env, HOME: join(root, "home"), HAARNESS_HOME: join(root, "home"), HAARNESS_DATA_DIR: join(root, "state"), HAARNESS_DESKTOP_DATA: join(root, "desktop"), HAARNESS_BIN: binary, HAARNESS_TEST_PROVIDER: "1", HAARNESS_DESKTOP_TEST: "1", HAARNESS_CLOUD_API_URL: "http://127.0.0.1:9" };
  delete (env as NodeJS.ProcessEnv).ELECTRON_RUN_AS_NODE;
  const git = (cwd: string, ...args: string[]) =>
    execFileSync("git", ["-c", "init.defaultBranch=main", "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", ...args], { cwd, stdio: "ignore" });
  const commit = async (text: string) => {
    await mkdir(join(upstream, ".claude-plugin"), { recursive: true });
    await mkdir(join(upstream, "kit/.claude-plugin"), { recursive: true });
    await mkdir(join(upstream, "kit/commands"), { recursive: true });
    await writeFile(join(upstream, ".claude-plugin/marketplace.json"), JSON.stringify({ name: "team-tools", plugins: [{ name: "kit", source: "./kit", description: "Fixture kit" }] }));
    await writeFile(join(upstream, "kit/.claude-plugin/plugin.json"), JSON.stringify({ name: "kit", description: "Fixture kit" }));
    await writeFile(join(upstream, "kit/commands/proof.md"), text);
    git(upstream, "add", "-A");
    git(upstream, "commit", "-qm", text);
    git(upstream, "push", "-q", "origin", "HEAD:main");
  };
  let app: Awaited<ReturnType<typeof electron.launch>> | undefined;
  try {
    for (const path of [workspace, upstream, env.HOME, env.HAARNESS_DESKTOP_DATA]) await mkdir(path, { recursive: true });
    git(root, "init", "-q", "--bare", bare);
    git(upstream, "init", "-q");
    git(upstream, "remote", "add", "origin", bare);
    await commit("One");
    // A Git marketplace and a folder one, as the CLI registers them.
    const cli = (...args: string[]) => execFileSync(binary, ["--workspace", workspace, ...args], { env, encoding: "utf8", timeout: 120000 });
    cli("plugin", "marketplace", "add", `file://${bare}`);
    cli("plugin", "install", "kit@team-tools");
    const folder = join(root, "folder-market");
    await mkdir(join(folder, ".claude-plugin"), { recursive: true });
    await writeFile(join(folder, ".claude-plugin/marketplace.json"), JSON.stringify({ name: "local-tools", plugins: [] }));
    cli("plugin", "marketplace", "add", folder);
    await writeFile(join(env.HAARNESS_DESKTOP_DATA, "desktop.json"), JSON.stringify({ preferences: {}, recent: [], values: {}, credentials: {} }));
    app = await electron.launch({ args: ["."], env, timeout: 120000 });
    const page = await app.firstWindow();
    await app.evaluate(({ dialog }, directory) => { dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [directory] }); }, workspace);
    await page.getByRole("button", { name: "Open another project", exact: true }).click();
    await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
    await page.getByRole("button", { name: "Settings", exact: true }).click();
    const settings = page.getByRole("dialog", { name: "Settings", exact: true });
    await settings.getByRole("button", { name: "Plugins", exact: true }).click();
    const list = settings.getByRole("region", { name: "Marketplaces", exact: true });
    await expect(list.getByRole("heading", { name: "Marketplaces", exact: true })).toBeVisible();
    const row = list.locator('[data-setting="marketplace:team-tools"]');
    await expect(row).toContainText(`file://${bare}`);
    await expect(row).toContainText(/Fetched/);
    const follow = row.getByRole("switch", { name: "Update team-tools automatically", exact: true });
    await expect(follow).toBeChecked();
    // A folder is read as it is: its row has neither control.
    const local = list.locator('[data-setting="marketplace:local-tools"]');
    await expect(local).toContainText("folder-market");
    await expect(local.getByRole("switch")).toHaveCount(0);
    await expect(local.getByRole("button", { name: "Update", exact: true })).toHaveCount(0);

    const update = row.getByRole("button", { name: "Update", exact: true });
    await update.click();
    await expect(row).toContainText("Up to date");
    const before = await row.boundingBox();
    await follow.click();
    await expect(follow).not.toBeChecked();
    const listed = () =>
      (JSON.parse(cli("marketplace", "request", JSON.stringify({ action: "plugin_marketplaces" }))) as { name: string; autoUpdate: boolean }[])
        .find((marketplace) => marketplace.name === "team-tools");
    await expect.poll(() => listed()?.autoUpdate).toBe(false);
    // Switching never moves the row.
    expect((await row.boundingBox())?.height).toBe(before?.height);

    await commit("Two");
    await update.click();
    await expect(row).toContainText("Updated just now · Plugins updated: kit");
    const installed = execFileSync("find", [root, "-path", "*/plugins/cache/team-tools/kit/commands/proof.md"], { encoding: "utf8" }).trim();
    expect(await readFile(installed, "utf8")).toBe("Two");
    await expect(settings.getByRole("alert")).toHaveCount(0);
    for (const scheme of ["light", "dark"] as const) {
      await app.evaluate(({ nativeTheme }, value) => { nativeTheme.themeSource = value; }, scheme);
      await page.emulateMedia({ colorScheme: scheme });
      await expect.poll(() => page.evaluate(() => matchMedia("(prefers-color-scheme: dark)").matches)).toBe(scheme === "dark");
      await page.waitForTimeout(300);
      await settings.screenshot({ path: test.info().outputPath(`plugin-marketplaces-${scheme}.png`) });
    }
  } finally {
    if (app) await app.close().catch(() => {});
    try { execFileSync(binary, ["--workspace", workspace, "serve", "--stop"], { env, stdio: "ignore", timeout: 30000 }); } catch {}
    stopChatHome(binary, env);
    await rm(root, { recursive: true, force: true });
  }
});
