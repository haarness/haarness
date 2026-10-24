import { test, expect, _electron as electron } from "@playwright/test";
import { Haarness } from "@haarness/sdk";
import { execFileSync } from "node:child_process";
import { mkdtemp, mkdir, writeFile, realpath, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { stopChatHome } from "./fixtures/chat-home";

const binary = resolve(process.env.HAARNESS_BIN ?? "../../target/debug/haarness");

test("Claude Code sources stay absent while Haarness plugins work", async () => {
  const root = await realpath(await mkdtemp(join(tmpdir(), "haarness-plugin-ownership-")));
  const home = join(root, "home");
  const workspace = join(root, "project");
  const external = join(home, ".claude/plugins/cache/clangd-lsp/1.0.0");
  const managed = join(root, "managed-plugin");
  const projectPlugin = join(workspace, ".haarness/plugins/local-source");
  const screenshot = join(process.env.HAARNESS_PLUGIN_EVIDENCE ?? root, "no-claude-import.png");
  const env: Record<string, string> = {
    ...Object.fromEntries(Object.entries(process.env).filter((entry): entry is [string, string] => entry[1] !== undefined)),
    HOME: home,
    HAARNESS_HOME: home,
    HAARNESS_DATA_DIR: join(root, "state"),
    HAARNESS_DESKTOP_DATA: join(root, "desktop"),
    HAARNESS_BIN: binary,
    HAARNESS_TEST_PROVIDER: "1",
    HAARNESS_DESKTOP_TEST: "1",
  };
  delete env.ELECTRON_RUN_AS_NODE;
  let app: Awaited<ReturnType<typeof electron.launch>> | undefined;
  let client: Haarness | undefined;
  try {
    await mkdir(join(external, ".claude-plugin"), { recursive: true });
    await mkdir(join(managed, ".claude-plugin"), { recursive: true });
    await mkdir(join(projectPlugin, ".claude-plugin"), { recursive: true });
    await mkdir(workspace, { recursive: true });
    await mkdir(join(root, "desktop"), { recursive: true });
    await writeFile(join(home, ".claude/settings.json"), '{"enabledPlugins":{"clangd-lsp@official":true}}');
    await mkdir(join(home, ".claude/commands"), { recursive: true });
    await mkdir(join(home, ".claude/skills/home-skill"), { recursive: true });
    await writeFile(join(home, ".claude/commands/home-command.md"), "CLAUDE_HOME_COMMAND");
    await writeFile(join(home, ".claude/skills/home-skill/SKILL.md"), "---\nname: home-skill\ndescription: Claude fixture\n---\nCLAUDE_HOME_SKILL");
    await writeFile(join(home, ".claude/plugins/installed_plugins.json"), JSON.stringify({
      plugins: { "clangd-lsp@official": [{ scope: "user", installPath: external, version: "1.0.0" }] },
    }));
    await writeFile(join(external, ".claude-plugin/plugin.json"), '{"name":"clangd-lsp","description":"Claude fixture"}');
    await mkdir(join(external, "commands"), { recursive: true });
    await writeFile(join(external, "commands/claude-plugin-command.md"), "CLAUDE_PLUGIN_COMMAND");
    await mkdir(join(workspace, ".claude/skills/claude-skill"), { recursive: true });
    await mkdir(join(workspace, ".claude/commands"), { recursive: true });
    await mkdir(join(workspace, ".claude/agents"), { recursive: true });
    await writeFile(join(workspace, ".claude/skills/claude-skill/SKILL.md"), "---\nname: claude-skill\ndescription: Claude fixture\n---\nCLAUDE_SKILL");
    await writeFile(join(workspace, ".claude/commands/claude-command.md"), "CLAUDE_COMMAND");
    await writeFile(join(workspace, ".claude/agents/claude-agent.md"), "---\ndescription: Claude fixture\n---\nCLAUDE_AGENT");
    await writeFile(join(workspace, "CLAUDE.md"), "CLAUDE_INSTRUCTIONS");
    await writeFile(join(workspace, ".mcp.json"), '{"mcpServers":{"claude-fixture":{"command":"false"}}}');
    await writeFile(join(managed, ".claude-plugin/plugin.json"), '{"name":"haarness-fixture","description":"Haarness fixture"}');
    await writeFile(join(projectPlugin, ".claude-plugin/plugin.json"), '{"name":"local-source","description":"Project fixture"}');
    await writeFile(join(root, "desktop/desktop.json"), JSON.stringify({ preferences: {}, recent: [], values: {}, credentials: {} }));
    execFileSync(binary, ["--workspace", workspace, "plugin", "install", managed], { env, stdio: "pipe" });
    const endpoint = JSON.parse(execFileSync(binary, ["--workspace", workspace, "desktop"], {
      env, input: '{"action":"connect"}', encoding: "utf8", timeout: 120_000,
    }));
    client = new Haarness({ baseUrl: endpoint.url, token: endpoint.token });
    const catalog = await client.plugins();
    expect(catalog.find((plugin) => plugin.name === "clangd-lsp")).toBeUndefined();
    expect(catalog.find((plugin) => plugin.name === "haarness-fixture")).toMatchObject({ source: "haarness", managed: true });
    expect(catalog.find((plugin) => plugin.name === "local-source")).toMatchObject({ source: "haarness-local", managed: true });
    expect((await client.commands()).find((command) => ["claude-plugin-command", "claude-command", "home-command"].includes(command.name))).toBeUndefined();
    expect((await client.skills()).find((skill) => ["claude-skill", "home-skill"].includes(skill.name))).toBeUndefined();
    expect((await client.agents()).find((agent) => agent.id === "claude-agent")).toBeUndefined();
    expect((await client.mcpServers()).find((server) => server.name === "claude-fixture")).toBeUndefined();
    await expect(client.pluginAction({ action: "update", target: "local-source" })).rejects.toThrow(/source directory directly/);

    app = await electron.launch({ args: ["."], env, timeout: 120_000 });
    const page = await app.firstWindow();
    await app.evaluate(({ dialog }, directory) => {
      dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [directory] });
    }, workspace);
    await page.getByRole("button", { name: "Open another project", exact: true }).click();
    await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
    await page.getByRole("button", { name: "Settings", exact: true }).click();
    const settings = page.getByRole("dialog", { name: "Settings", exact: true });
    await settings.getByRole("button", { name: "Plugins", exact: true }).click();
    const externalRow = settings.locator('[data-setting="plugin:clangd-lsp"]');
    await expect(externalRow).toHaveCount(0);
    const managedRow = settings.locator('[data-setting="plugin:haarness-fixture"]');
    await expect(managedRow.getByRole("switch")).toBeChecked();
    await expect(managedRow.getByRole("button", { name: "Update" })).toBeVisible();
    await expect(managedRow.getByRole("button", { name: "Remove" })).toBeVisible();
    const projectRow = settings.locator('[data-setting="plugin:local-source"]');
    await expect(projectRow.getByRole("switch")).toBeChecked();
    await expect(projectRow.getByRole("button", { name: "Update" })).toHaveCount(0);
    await expect(projectRow.getByRole("button", { name: "Remove" })).toBeVisible();
    await page.screenshot({ path: screenshot });
    await managedRow.getByRole("button", { name: "Update" }).click();
    await expect.poll(async () => (await client!.plugins()).find((plugin) => plugin.name === "haarness-fixture")?.enabled).toBe(true);
    await expect(page.getByRole("alert")).toHaveCount(0);
  } finally {
    client?.close();
    if (app) await app.close().catch(() => {});
    try { execFileSync(binary, ["--workspace", workspace, "serve", "--stop"], { env, stdio: "ignore", timeout: 30_000 }); } catch {}
    stopChatHome(binary, env);
    await rm(root, { recursive: true, force: true });
  }
});
