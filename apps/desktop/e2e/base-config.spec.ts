import { test, expect, _electron as electron, type Locator } from "@playwright/test";
import { execFileSync } from "node:child_process";
import { mkdtemp, mkdir, writeFile, readFile, realpath, rm, readdir } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve, relative } from "node:path";

const binary = resolve(process.env.HAARNESS_BIN ?? "../../target/debug/haarness");

/** Every file under `folder` with its contents, to prove nothing was written. */
async function snapshot(folder: string): Promise<Record<string, string>> {
  const files: Record<string, string> = {};
  for (const entry of await readdir(folder, { recursive: true, withFileTypes: true })) {
    if (!entry.isFile()) continue;
    const path = join(entry.parentPath, entry.name);
    files[relative(folder, path)] = await readFile(path, "utf8");
  }
  return files;
}

test("items of the base config folder carry a quiet Base tag and no Remove", async () => {
  const root = await realpath(await mkdtemp(join(tmpdir(), "haarness-base-config-")));
  const home = join(root, "home");
  const workspace = join(root, "project");
  const team = join(root, "team");
  const evidence = process.env.HAARNESS_BASE_EVIDENCE ?? join(root, "evidence");
  const env: Record<string, string> = {
    ...Object.fromEntries(Object.entries(process.env).filter((entry): entry is [string, string] => entry[1] !== undefined)),
    HOME: home,
    HAARNESS_HOME: home,
    HAARNESS_DATA_DIR: join(root, "state"),
    HAARNESS_DESKTOP_DATA: join(root, "desktop"),
    HAARNESS_BIN: binary,
    HAARNESS_TEST_PROVIDER: "1",
    HAARNESS_DESKTOP_TEST: "1",
    HAARNESS_BASE_CONFIG_DIR: team,
  };
  delete env.ELECTRON_RUN_AS_NODE;
  let app: Awaited<ReturnType<typeof electron.launch>> | undefined;
  try {
    for (const folder of [
      workspace, evidence, join(root, "desktop"), join(root, "state/config"), join(team, "agents"), join(team, "commands"),
      join(team, "skills/team-check"), join(team, "plugins/local/team-plugin/.claude-plugin"),
    ]) await mkdir(folder, { recursive: true });
    execFileSync("git", ["init", "-q", workspace]);
    await writeFile(join(root, "state/config/config.toml"), '[defaults]\nprovider="echo"\nmodel="echo-1"\n');
    await writeFile(join(root, "desktop/desktop.json"), JSON.stringify({ preferences: {}, recent: [], values: {}, credentials: {} }));
    const mcp = join(root, "mcp.py");
    await writeFile(mcp, [
      "import json, sys",
      "for line in sys.stdin:",
      "    request = json.loads(line)",
      "    if 'id' not in request: continue",
      "    method = request['method']",
      "    result = {'protocolVersion':'2024-11-05','capabilities':{'tools':{}},'serverInfo':{'name':'team','version':'1'}} if method == 'initialize' else {'tools':[]} if method == 'tools/list' else {}",
      "    print(json.dumps({'jsonrpc':'2.0','id':request['id'],'result':result}), flush=True)",
    ].join("\n"));
    await writeFile(join(team, "haarness.json"), JSON.stringify({
      mcp: { teamfs: { type: "local", command: ["python3", mcp] } },
      hooks: { Stop: [{ hooks: [{ type: "command", command: "true" }] }] },
    }));
    await writeFile(join(team, "agents/team-reviewer.md"), "---\ndescription: Reviews like the team\n---\nReview with the team checklist.");
    await writeFile(join(team, "commands/team-ship.md"), "---\ndescription: Ship with the team checklist\n---\nShip $ARGUMENTS");
    await writeFile(join(team, "skills/team-check/SKILL.md"), "---\nname: team-check\ndescription: The team release checklist\n---\n1. Changelog.");
    await writeFile(join(team, "plugins/local/team-plugin/.claude-plugin/plugin.json"), '{"name":"team-plugin","description":"The team plugin"}');
    const before = await snapshot(team);

    app = await electron.launch({ args: ["."], env, timeout: 120_000 });
    const page = await app.firstWindow();
    await app.evaluate(({ dialog }, directory) => {
      dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [directory] });
    }, workspace);
    await page.getByRole("region", { name: "Project home" })
      .getByRole("button", { name: "Open project", exact: true }).click();
    await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
    await page.getByRole("button", { name: "Settings", exact: true }).click();
    const settings = page.getByRole("dialog", { name: "Settings", exact: true });
    const section = (name: string) => settings.getByRole("button", { name, exact: true }).click();
    const tag = (row: Locator) => row.locator(".setting-tag");
    async function shoot(name: string) {
      for (const scheme of ["light", "dark"] as const) {
        await page.emulateMedia({ colorScheme: scheme });
        await expect(page.locator("html")).toHaveAttribute("data-theme", scheme);
        // Let the colour transition finish, so the picture shows the scheme itself.
        await page.waitForTimeout(400);
        await page.evaluate(() => Promise.all(document.getAnimations().map((animation) => animation.finished)));
        await page.screenshot({ path: join(evidence, `base-${name}-${scheme}.png`) });
      }
    }

    // Agents, commands and skills: the tag beside the name, Edit to save your
    // own version, no Remove, no scope text the tag already says.
    for (const [name, kind, item, description] of [
      ["Agents", "agents", "team-reviewer", "Reviews like the team"],
      ["Commands", "commands", "team-ship", "Ship with the team checklist"],
      ["Skills", "skills", "team-check", "The team release checklist"],
    ] as const) {
      await section(name);
      const row = settings.locator(`[data-setting="${kind}:${item}"]`);
      await expect(tag(row)).toHaveText("Base");
      await expect(tag(row)).toHaveAttribute("title", "From the base folder");
      await expect(row.locator("p")).toHaveText(description);
      await expect(row.getByRole("button", { name: "Edit", exact: true })).toBeVisible();
      await expect(row.getByRole("button", { name: "Remove", exact: true })).toHaveCount(0);
      if (kind === "agents") await shoot("agents");
    }
    // A built-in agent keeps its own wording and no tag.
    await section("Agents");
    const build = settings.locator('[data-setting="agents:build"]');
    await expect(build.locator(".setting-tag")).toHaveCount(0);
    await expect(build.locator("p")).toContainText("Built-in / plugin");

    // MCP: the tag beside the status, and Manage offers no Remove.
    await section("MCP servers");
    const server = settings.locator(".setting-block").filter({ has: page.locator('[data-setting="mcp:teamfs"]') });
    await expect(server.locator(".setting-tag")).toHaveText("Base");
    await expect(server.locator(".badge").last()).toHaveText(/connected|starting|connecting/);
    await shoot("mcp");
    await server.getByRole("button", { name: "Manage teamfs" }).click();
    const menu = page.getByRole("menu");
    await expect(menu.getByRole("menuitem", { name: "Disable" })).toBeVisible();
    await expect(menu.getByRole("menuitem", { name: "Remove" })).toHaveCount(0);
    await page.screenshot({ path: join(evidence, "base-mcp-manage-menu.png") });
    await page.keyboard.press("Escape");

    // Hooks: the tag where the scope is named; viewing only.
    await section("Hooks");
    const source = page.locator(".management-source").filter({ hasText: join(team, "haarness.json") });
    const card = settings.locator("article.management-card").filter({ has: source });
    await expect(card.locator(".management-source .setting-tag")).toHaveText("Base");
    await expect(card.getByRole("switch")).toBeDisabled();
    await expect(card.getByRole("button", { name: "View hook" })).toBeVisible();
    await expect(card.getByRole("button", { name: "Remove hook" })).toHaveCount(0);
    await shoot("hooks");

    // Plugins: the tag, and no switch or Remove.
    await section("Plugins");
    const plugin = settings.locator('[data-setting="plugin:team-plugin"]');
    await expect(tag(plugin)).toHaveText("Base");
    await expect(plugin.getByRole("switch")).toHaveCount(0);
    await expect(plugin.getByRole("button", { name: "Remove" })).toHaveCount(0);
    await shoot("plugins");

    expect(await snapshot(team)).toEqual(before);
  } finally {
    if (app) await app.close().catch(() => {});
    try { execFileSync(binary, ["--workspace", workspace, "serve", "--stop"], { env, stdio: "ignore", timeout: 30_000 }); } catch {}
    await rm(root, { recursive: true, force: true });
  }
});
