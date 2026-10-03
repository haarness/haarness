import { test, expect, _electron as electron } from "@playwright/test";
import { Haarness } from "@haarness/sdk";
import { createServer } from "node:http";
import { randomUUID } from "node:crypto";
import { execFileSync } from "node:child_process";
import { mkdtemp, mkdir, writeFile, realpath, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

test("Marketplace and plugin settings install, disable and remove a scoped resource", async () => {
  const root = await realpath(await mkdtemp(join(tmpdir(), "haarness-desktop-marketplace-")));
  const workspace = join(root, "project");
  const binary = resolve(process.env.HAARNESS_BIN ?? "../../target/debug/haarness");
  const id = randomUUID(), versionId = randomUUID();
  const resource = { id, kind: "mcp", slug: "desktop-proof", name: "Desktop proof", visibility: "private", summary: "Marketplace fixture", publisher: { slug: "fixture", displayName: "Fixture" } };
  const manifest = { manifestVersion: 1, resource, version: { id: versionId, number: 1, status: "approved", yanked: false }, lock: { resourceId: id, versionId }, capabilities: {}, compatibility: {}, install: { mcp: { config: { type: "remote", url: "http://127.0.0.1:9/mcp", enabled: false }, env_placeholders: [] } } };
  const catalog = createServer((request, response) => {
    response.setHeader("content-type", "application/json");
    response.end(JSON.stringify(request.url?.includes("install-manifest") ? manifest : { items: [resource], nextCursor: null }));
  });
  await new Promise<void>((done) => catalog.listen(0, "127.0.0.1", done));
  const address = catalog.address() as { port: number };
  const env = { ...process.env, HOME: join(root, "home"), HAARNESS_HOME: join(root, "home"), HAARNESS_DATA_DIR: join(root, "state"), HAARNESS_DESKTOP_DATA: join(root, "desktop"), HAARNESS_BIN: binary, HAARNESS_TEST_PROVIDER: "1", HAARNESS_DESKTOP_TEST: "1", HAARNESS_CLOUD_API_URL: `http://127.0.0.1:${address.port}` };
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
    await app.evaluate(({ dialog }, directory) => { dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [directory] }); }, workspace);
    await page.getByRole("region", { name: "Project home" }).getByRole("button", { name: "Open project", exact: true }).click();
    await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
    await page.getByRole("button", { name: "Settings", exact: true }).click();
    const settings = page.getByRole("dialog", { name: "Settings", exact: true });
    await settings.getByRole("button", { name: "Marketplace", exact: true }).click();
    await settings.getByRole("button", { name: "Desktop proof · mcp · private", exact: true }).click();
    await expect(settings.getByLabel("Installation scope")).toBeVisible();
    await settings.getByRole("button", { name: "Install", exact: true }).click();
    await expect.poll(() => client!.marketplace({ action: "installed" })).toMatchObject([{ id, scope: "local" }]);
    await settings.getByRole("button", { name: "Plugins", exact: true }).click();
    const proof = settings.getByRole("switch", { name: "Enable plugin mcp-desktop-proof", exact: true });
    await expect(proof).toBeChecked();
    await proof.click();
    await expect(proof).not.toBeChecked();
    await settings.getByRole("button", { name: "Marketplace", exact: true }).click();
    await settings.getByRole("button", { name: "Installed resources", exact: true }).click();
    await settings.getByRole("button", { name: "remove desktop-proof", exact: true }).click();
    await settings.getByRole("alertdialog").getByRole("button", { name: "Confirm", exact: true }).click();
    await expect.poll(() => client!.marketplace({ action: "installed" })).toEqual([]);
    await expect(settings.getByRole("alert")).toHaveCount(0);
  } finally {
    client?.close();
    if (app) await app.close().catch(() => {});
    try { execFileSync(binary, ["--workspace", workspace, "serve", "--stop"], { env, stdio: "ignore", timeout: 30000 }); } catch {}
    catalog.close();
    await rm(root, { recursive: true, force: true });
  }
});
