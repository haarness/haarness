import { test, expect, _electron as electron, type ElectronApplication, type Page, type Locator } from "@playwright/test";
import { Haarness } from "@haarness/sdk";
import { mkdtemp, mkdir, writeFile, readFile, realpath, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";
import { stopChatHome } from "./fixtures/chat-home";

// MCP servers in Desktop: a server that failed says why under its name, the model's resource tools read in
// plain words, a server's interactive apps turn off from its page's options or its row in Settings (and back
// on with Undo) unless the server comes from a plugin, and a secret-looking value is kept in the keychain with
// a click, never replacing another entry and taken out again when the server cannot be added. The keychain is
// a test file (HAARNESS_TEST_KEYCHAIN_FILE, which only debug builds read), so the system's is never touched.
test.describe.configure({ mode: "serial" });

let app: ElectronApplication, page: Page, client: Haarness;
let temporary: string, workspace: string, binary: string, env: Record<string, string>, keychain: string;
let thread = "";
const errors: string[] = [];
const fixtures = resolve("../../crates/haarness-mcp/tests/fixtures");
const shots = process.env.HAARNESS_SCREENSHOTS;
const settingsFile = () => join(workspace, ".haarness/haarness.json");

test.beforeAll(async () => {
  test.setTimeout(240000);
  temporary = await realpath(await mkdtemp(join(process.env.HAARNESS_E2E_DIR ?? tmpdir(), "haarness-mcp-servers-")));
  workspace = join(temporary, "workspace");
  keychain = join(temporary, "keychain.json");
  await mkdir(join(workspace, ".haarness"), { recursive: true });
  const server = (fixture: string, extra: Record<string, unknown> = {}) => ({
    command: "python3",
    args: [join(fixtures, fixture)],
    ...extra,
  });
  // A project plugin's server: Haarness does not edit its settings.
  await mkdir(join(workspace, ".haarness/plugins/kit/.claude-plugin"), { recursive: true });
  await writeFile(
    join(workspace, ".haarness/plugins/kit/.claude-plugin/plugin.json"),
    JSON.stringify({ name: "kit", description: "Plugin fixture", mcpServers: { weather: server("app.py") } }),
  );
  await writeFile(
    settingsFile(),
    JSON.stringify(
      {
        model: "echo/echo-1",
        mcp: {
          app: server("app.py"),
          docs: server("resources.py"),
          // Floods its handshake: stopped at once, and says why.
          flood: server("flood.py", { env: { FLOOD_INIT: "1" } }),
        },
      },
      null,
      2,
    ),
  );
  binary = resolve(process.env.HAARNESS_BIN ?? "../../target/debug/haarness");
  env = {
    ...(process.env as Record<string, string>),
    HAARNESS_BIN: binary,
    HAARNESS_HOME: join(temporary, "home"),
    HAARNESS_DATA_DIR: join(temporary, "data"),
    HAARNESS_DESKTOP_DATA: join(temporary, "desktop"),
    HAARNESS_TEST_PROVIDER: "1",
    HAARNESS_DESKTOP_TEST: "1",
    HAARNESS_TEST_KEYCHAIN_FILE: keychain,
  };
  delete env.ELECTRON_RUN_AS_NODE;
  delete env.ELECTRON_RENDERER_URL;
  app = await electron.launch({ args: ["."], env, timeout: 60000 });
  page = await app.firstWindow();
  page.on("pageerror", (error) => errors.push(error.message));
  await page.setViewportSize({ width: 1400, height: 950 });
  await app.evaluate(({ dialog }, directory) => {
    dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [directory] });
  }, workspace);
  await page.getByRole("button", { name: "Open another project", exact: true }).click();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 90000 });
  const connection = JSON.parse(
    execFileSync(binary, ["--workspace", workspace, "--trust-project", "desktop"], {
      env,
      input: '{"action":"connect"}',
      encoding: "utf8",
      timeout: 120000,
    }),
  );
  client = new Haarness({ baseUrl: connection.url, token: connection.token });
  await expect
    .poll(async () => Object.fromEntries((await client.mcpServers()).map((server) => [server.name, server.status])), { timeout: 30000 })
    .toEqual({ app: "connected", docs: "connected", flood: "failed", weather: "connected" });
  thread = (await client.createThread({ title: "MCP servers", provider: "echo", model: "echo-1" })).id;
  await page.locator(".sessions").getByRole("button", { name: "MCP servers", exact: true }).click();
});

test.afterAll(async () => {
  client?.close();
  await app?.close();
  if (workspace)
    try {
      execFileSync(binary, ["--workspace", workspace, "--trust-project", "serve", "--stop"], {
        env,
        stdio: "ignore",
        timeout: 30_000,
      });
    } catch {}
  stopChatHome(binary, env);
  if (temporary) await rm(temporary, { recursive: true, force: true }).catch(() => {});
});

const settings = () => page.getByRole("dialog", { name: "Settings", exact: true });
async function settled() {
  await expect.poll(async () => (await client.activeTurns()).length, { timeout: 30000 }).toBe(0);
}
async function tool(text: string) {
  await settled();
  const turn = await client.startTurn(thread, { requestId: crypto.randomUUID(), text: `!tool ${text}` });
  await client.waitTurn(turn.id);
}
/** Light and dark pictures of `target` (or of the window around several), kept for the report when HAARNESS_SCREENSHOTS names a folder. */
async function pictures(target: Locator | Locator[], name: string) {
  for (const theme of ["light", "dark"] as const) {
    await page.evaluate((theme) => window.haarness.preferences({ theme }), theme);
    await expect(page.locator("html")).toHaveAttribute("data-theme", theme);
    await page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done))));
    if (!shots) continue;
    const path = join(shots, `${name}-${theme}.png`);
    if (!Array.isArray(target)) {
      await target.screenshot({ path });
      continue;
    }
    const boxes = (await Promise.all(target.map((item) => item.boundingBox()))).filter((box) => !!box);
    const x = Math.min(...boxes.map((box) => box.x)) - 8;
    const y = Math.min(...boxes.map((box) => box.y)) - 8;
    const right = Math.max(...boxes.map((box) => box.x + box.width)) + 8;
    const bottom = Math.max(...boxes.map((box) => box.y + box.height)) + 8;
    await page.screenshot({ path, clip: { x, y, width: right - x, height: bottom - y } });
  }
  await page.evaluate(() => window.haarness.preferences({ theme: "light" }));
}
async function openMcpSettings() {
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  await settings().getByRole("button", { name: "MCP servers", exact: true }).click();
}
async function closeSettings() {
  await page.keyboard.press("Escape");
  await expect(settings()).toHaveCount(0);
}
const block = (name: string) => settings().locator(".setting-block").filter({ has: page.locator(`[data-setting="mcp:${name}"]`) });

test("a server stopped for an oversized reply says why under its name", async () => {
  await openMcpSettings();
  const flood = block("flood");
  await expect(flood.locator(".mcp-server-detail")).toHaveText(
    "Its reply was larger than 64 MB, so Haarness stopped it. Reconnect it to use it again.",
  );
  // A server that connected shows no line of its own.
  await expect(block("docs").locator(".mcp-server-detail")).toHaveCount(0);
  await pictures(flood, "mcp-failed-detail");
  await closeSettings();
});

test("the model's resource tools read as what they did", async () => {
  await tool('mcp_resource_list {"server":"docs"}');
  await tool('mcp_resource_read {"server":"docs","uri":"docs://setup"}');
  const rows = page.locator(".message.step > .tool-call > summary");
  await expect(rows.nth(-2)).toHaveText(/^Listed MCP resources\s*docs$/);
  await expect(rows.last()).toHaveText(/^Read an MCP resource\s*docs:\/\/setup$/);
  await rows.last().click();
  await expect(page.locator(".tool-call[open] .tool-output").last()).toContainText("# Setup");
  await pictures(page.locator(".message.step").last(), "mcp-resource-rows");
  await rows.last().click();
});

test("a server's interactive apps turn off from its page, with Undo, and from its row in Settings", async () => {
  await tool('mcp__app__show_weather {"city":"Porto"}');
  const view = page.locator(".mcp-app").last();
  await expect(view).toHaveAttribute("data-state", "ready", { timeout: 30000 });
  // The options button stays out of the page's way until it is pointed at.
  const options = view.getByRole("button", { name: "Show the weather app options", exact: true });
  const corner = view.locator(".mcp-app-menu");
  await page.mouse.move(5, 300);
  await expect(corner).toHaveCSS("opacity", "0");
  // Pointing at the page itself shows them, though the window cannot see past its frame.
  await view.locator("iframe").hover();
  await expect(corner).toHaveCSS("opacity", "1");
  await options.click();
  await pictures([view, page.getByRole("menu")], "mcp-app-options");
  await page.getByRole("menuitem", { name: "Turn off interactive apps for app", exact: true }).click();
  // The page turns to text at once, and says so where it was.
  const notice = page.locator(".mcp-app-off");
  await expect(notice).toContainText("Interactive apps are off for app. Its tools show as text.");
  await expect(page.locator(".mcp-app[data-mcp-app]")).toHaveCount(0);
  await expect.poll(async () => JSON.parse(await readFile(settingsFile(), "utf8")).mcp.app.apps).toBe(false);
  await pictures(page.locator(".message.step").last(), "mcp-app-turned-off");
  await expect.poll(async () => (await client.mcpServers()).find((server) => server.name === "app")?.apps, { timeout: 30000 }).toBe(false);
  // Undo puts it back, and drops the setting rather than writing the default.
  await notice.getByRole("button", { name: "Undo", exact: true }).click();
  await expect(page.locator(".mcp-app[data-mcp-app]").last()).toHaveAttribute("data-state", "ready", { timeout: 30000 });
  await expect(notice).toHaveCount(0);
  await expect.poll(async () => "apps" in JSON.parse(await readFile(settingsFile(), "utf8")).mcp.app).toBe(false);
  await expect.poll(async () => (await client.mcpServers()).find((server) => server.name === "app")?.appTools, { timeout: 30000 }).toBe(1);

  // In Settings the switch belongs to the server that has pages; the others show none.
  await openMcpSettings();
  const toggle = block("app").getByRole("switch", { name: "Interactive apps for app", exact: true });
  await expect(toggle).toBeChecked();
  await expect(block("docs").getByRole("switch")).toHaveCount(0);
  await toggle.click();
  await expect(toggle).not.toBeChecked();
  await expect.poll(async () => JSON.parse(await readFile(settingsFile(), "utf8")).mcp.app.apps).toBe(false);
  await pictures(block("app"), "mcp-apps-switch-off");
  await closeSettings();
  // Every page of that server shows as the plain tool, the older one too.
  await expect(page.locator(".mcp-app[data-mcp-app]")).toHaveCount(0);
  await openMcpSettings();
  await block("app").getByRole("switch", { name: "Interactive apps for app", exact: true }).click();
  await expect.poll(async () => "apps" in JSON.parse(await readFile(settingsFile(), "utf8")).mcp.app).toBe(false);
  await pictures(block("app"), "mcp-apps-switch-on");
  await closeSettings();
  await expect(page.locator(".mcp-app[data-mcp-app]").last()).toHaveAttribute("data-state", "ready", { timeout: 30000 });
});

test("a plugin's server shows its apps state but neither its switch nor its page turns them off", async () => {
  await openMcpSettings();
  const toggle = block("weather").getByRole("switch", { name: "Interactive apps for weather", exact: true });
  await expect(toggle).toBeChecked();
  await expect(toggle).toBeDisabled();
  await expect(block("weather").locator(".mcp-apps-switch")).toHaveAttribute("title", "Set outside your settings");
  await pictures(block("weather"), "mcp-apps-switch-read-only");
  await closeSettings();
  await tool('mcp__weather__show_weather {"city":"Faro"}');
  const view = page.locator(".mcp-app[data-mcp-app]").last();
  await expect(view).toHaveAttribute("data-state", "ready", { timeout: 30000 });
  await view.locator("iframe").hover();
  await expect(view.locator(".mcp-app-menu")).toHaveCount(0);
  // The app server's own page still offers it.
  await expect(page.locator(".mcp-app[data-mcp-app]").first().locator(".mcp-app-menu")).toHaveCount(1);
});

test("a server that cannot be added takes the secrets it stored out again", async () => {
  await writeFile(keychain, JSON.stringify({ kept: "unrelated" }));
  await openMcpSettings();
  await settings().getByRole("button", { name: "Add MCP server", exact: true }).click();
  const form = page.getByRole("dialog", { name: "Add MCP server", exact: true });
  // `app` is already a server of this project.
  await form.getByRole("textbox", { name: "Name", exact: true }).fill("app");
  await form.getByRole("textbox", { name: "Executable", exact: true }).fill("python3");
  const scope = form.getByRole("combobox", { name: "Available in" });
  await scope.click();
  await form.getByRole("option", { name: "This project", exact: true }).click();
  await form.getByRole("button", { name: "Add environment variable", exact: true }).click();
  await form.getByRole("textbox", { name: "Variable name 1", exact: true }).fill("API_KEY");
  await form.getByLabel("environment variable 1 value", { exact: true }).fill("never-kept");
  await form.getByRole("button", { name: /^Store in (Keychain|Credential Manager|keyring): API_KEY$/ }).click();
  await form.getByRole("button", { name: "Add server", exact: true }).click();
  await expect(form.getByRole("alert")).toContainText("already exists");
  expect(JSON.parse(await readFile(keychain, "utf8"))).toEqual({ kept: "unrelated" });
  await form.getByRole("button", { name: "Cancel", exact: true }).click();
  await closeSettings();
});

test("a secret-looking value is kept in the keychain and the settings name it", async () => {
  // Another server's entry by the same name stays as it is.
  await writeFile(keychain, JSON.stringify({ "keyed-GITHUB_TOKEN": "another-server" }));
  const secret = 'ghp_desk"top\\e2e-secret';
  await openMcpSettings();
  await settings().getByRole("button", { name: "Add MCP server", exact: true }).click();
  const form = page.getByRole("dialog", { name: "Add MCP server", exact: true });
  await form.getByRole("textbox", { name: "Name", exact: true }).fill("keyed");
  await form.getByRole("textbox", { name: "Executable", exact: true }).fill("python3");
  await form.getByRole("button", { name: "Add argument", exact: true }).click();
  await form.getByRole("textbox", { name: "Argument 1", exact: true }).fill(join(fixtures, "env.py"));
  const scope = form.getByRole("combobox", { name: "Available in" });
  await scope.click();
  await form.getByRole("option", { name: "This project", exact: true }).click();
  await form.getByRole("button", { name: "Add environment variable", exact: true }).click();
  await form.getByRole("textbox", { name: "Variable name 1", exact: true }).fill("LOG_LEVEL");
  await form.getByLabel("environment variable 1 value", { exact: true }).fill("debug");
  // Nothing to keep yet: the toggle's place stays empty, so the fields never move.
  const keep = form.getByRole("button", { name: /^Store in (Keychain|Credential Manager|keyring): GITHUB_TOKEN$/ });
  await expect(form.locator("[data-mcp-pair] .mcp-keep")).toHaveCount(0);
  await form.getByRole("button", { name: "Add environment variable", exact: true }).click();
  await form.getByRole("textbox", { name: "Variable name 2", exact: true }).fill("GITHUB_TOKEN");
  await form.getByLabel("environment variable 2 value", { exact: true }).fill(secret);
  await expect(keep).toHaveAttribute("aria-pressed", "false");
  await keep.click();
  await expect(keep).toHaveAttribute("aria-pressed", "true");
  await pictures(form, "mcp-keychain-toggle");
  await form.getByRole("button", { name: "Add server", exact: true }).click();
  await expect(form).toHaveCount(0);
  expect(JSON.parse(await readFile(keychain, "utf8"))).toEqual({
    "keyed-GITHUB_TOKEN": "another-server",
    "keyed-GITHUB_TOKEN-2": secret,
  });
  const written = JSON.parse(await readFile(settingsFile(), "utf8")).mcp.keyed;
  expect(written.env ?? written.environment).toEqual({ LOG_LEVEL: "debug", GITHUB_TOKEN: "${keychain:keyed-GITHUB_TOKEN-2}" });
  expect(await readFile(settingsFile(), "utf8")).not.toContain("ghp_desk");
  await expect
    .poll(async () => (await client.mcpServers()).find((server) => server.name === "keyed")?.status, { timeout: 30000 })
    .toBe("connected");
  const output = await client.callMcpTool("keyed", "env", { name: "GITHUB_TOKEN" });
  expect(output.text).toBe(secret);
  await closeSettings();
  expect(errors).toEqual([]);
});
