import { Haarness, type ServerInfo } from "@haarness/sdk";
import { themes, themeColors } from "../src/themes";
import { sessionKey } from "../src/projects";
import {
  test,
  expect,
  _electron as electron,
  type ElectronApplication,
  type Page,
  type Locator,
} from "@playwright/test";
import { attachFiles } from "./fixtures/composer";
import {
  mkdtemp,
  mkdir,
  writeFile,
  readFile,
  rm,
  realpath,
  copyFile,
  chmod,
  rename,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { execFileSync, spawn } from "node:child_process";
import { createServer } from "node:http";
import { stopChatHome } from "./fixtures/chat-home";
async function selectValue(control: Locator, value: string) {
  control = control.and(control.page().getByRole("combobox"));
  await control.click();
  const id = await control.getAttribute("aria-controls");
  await control
    .page()
    .locator(
      `[id=${JSON.stringify(id)}] [role="option"][data-value=${JSON.stringify(value)}]`,
    )
    .click();
  const done = control.page().locator(".select-footer").getByRole("button", { name: "Done", exact: true });
  if (await done.isVisible()) await done.click();
}
/** Where each row's label starts in an open menu, to the pixel. */
const labelStarts = (menu: Locator) =>
  menu.locator(":scope > button, :scope > .menu-branch > button").evaluateAll((rows) =>
    rows.map((row) => {
      const label = row.querySelector(".menu-item-label");
      if (label) return Math.round(label.getBoundingClientRect().left);
      const text = [...row.childNodes].find((node) => node.nodeType === Node.TEXT_NODE && node.textContent?.trim());
      const range = document.createRange();
      range.selectNodeContents(text!);
      return Math.round(range.getBoundingClientRect().left);
    }),
  );
/** Where the live choice's tick sits, and how far in: in a column of its own
 * before the label ("start"), or at the row's end as a picker's ✓ ("end"). */
const tickPlace = (menu: Locator) =>
  menu.locator('[aria-checked="true"]').evaluate((row) => {
    const box = row.getBoundingClientRect();
    const end = row.querySelector(".select-check");
    const start = row.querySelector(".menu-check svg");
    if (end && !start)
      return { place: "end", glyph: end.textContent?.trim(), inset: Math.round(box.right - end.getBoundingClientRect().right) };
    if (start && !end) return { place: "start", inset: Math.round(start.getBoundingClientRect().left - box.left) };
    return undefined;
  });
async function selectEffort(model: Locator, value: string) {
  if ((await model.getAttribute("aria-expanded")) !== "true") await model.click();
  const slider = model.page().getByRole("slider", { name: "Reasoning effort", exact: true });
  await slider.focus();
  await slider.press("Home");
  const target = value ? value[0].toUpperCase() + value.slice(1) : "Model default";
  const count = Number(await slider.getAttribute("max"));
  for (let index = 0; index < count && (await slider.getAttribute("aria-valuetext")) !== target; index++)
    await slider.press("ArrowRight");
  await expect(slider).toHaveAttribute("aria-valuetext", target);
  await model.page().locator(".select-footer").getByRole("button", { name: "Done", exact: true }).click();
}
const root = resolve("../..");
// Packaged tests must attach with the app's actual runtime. Daemon identity
// includes the executable path, so an external copy would replace its server.
const packagedExecutable = process.env.HAARNESS_DESKTOP_EXECUTABLE;
const binary = packagedExecutable
  ? resolve(
      dirname(packagedExecutable),
      process.platform === "darwin" ? "../Resources" : "resources",
      "bin",
      process.platform === "win32" ? "haarness.exe" : "haarness",
    )
  : process.env.HAARNESS_BIN
    ? resolve(process.env.HAARNESS_BIN)
    : join(
        root,
        "target/debug",
        process.platform === "win32" ? "haarness.exe" : "haarness",
      );
let app: ElectronApplication, page: Page, temporary: string, workspace: string;
// A stand-in for auth.openai.com so the ChatGPT device sign-in runs end to
// end without the network: one device code, one refused poll, then tokens.
let issuer: ReturnType<typeof createServer> | undefined;
let issuerPolls = 0;
let env: Record<string, string>;
let client: Haarness;
const longCommand = "ui-layout-" + "long-command-name-".repeat(9) + "finish";
async function openProjectPanel(panel: "Files" | "Review") {
  const showPanel = page.getByRole("button", { name: "Show side panel", exact: true });
  if (await showPanel.isVisible()) await showPanel.click();
  const tab = page.getByRole("button", {
    name: `Show ${panel === "Files" ? "files" : "diff"} panel`,
    exact: true,
  });
  if (await tab.isVisible()) {
    await tab.click();
    return;
  }
  await page.getByRole("button", { name: "Open panel", exact: true }).click();
  await page.getByRole("region", { name: "Open a project panel" })
    .getByRole("button", { name: `Open ${panel}`, exact: true }).click();
}
async function attachClient() {
  const result = JSON.parse(
    execFileSync(
      binary,
      ["--workspace", workspace, "--trust-project", "desktop"],
      {
        env,
        input: JSON.stringify({ action: "connect" }),
        encoding: "utf8",
        timeout: 240000,
      },
    ),
  );
  client?.close();
  client = new Haarness({ baseUrl: result.url, token: result.token });
}
async function expandPrimaryProject() {
  const header = page.locator(".project-list .project-heading").filter({ hasText: /^workspace$/ });
  if ((await header.getAttribute("aria-expanded")) !== "true") await header.click();
  await expect(header).toHaveAttribute("aria-expanded", "true");
}
async function showChat() {
  const restore = page.getByRole("button", { name: "Restore side panel", exact: true });
  if (await restore.isVisible()) await restore.click();
}
async function newSession() {
  await page.getByRole("button", { name: "New chat", exact: true }).click();
  await expect(
    page.getByRole("textbox", { name: "Message", exact: true }),
  ).toBeVisible();
}
async function send(text: string) {
  await page.getByRole("textbox", { name: "Message", exact: true }).fill(text);
  await page
    .getByRole("button", { name: /Send message|Send guidance/ })
    .click();
}
/** Waits until no turn runs, so the next message starts a turn of its own
 * instead of joining the running one as guidance (which a loaded machine
 * makes likely when one tool message follows another). */
async function settled() {
  await expect
    .poll(() => client.activeTurns().then((turns) => turns.length))
    .toBe(0);
}
const errors: string[] = [];
const extraWorkspaces: string[] = [];

test.beforeAll(async () => {
  // macOS may inspect a newly signed bundle before its first instruction runs.
  // Keep this separate from the bounded interaction and SSE assertions below.
  test.setTimeout(600000);
  temporary = await realpath(
    await mkdtemp(join(tmpdir(), "haarness-desktop-e2e-")),
  );
  workspace = join(temporary, "workspace");
  await mkdir(join(workspace, ".haarness", "workflows"), { recursive: true });
  await mkdir(join(workspace, "src"));
  await mkdir(join(workspace, ".haarness", "commands"));
  for (const name of [longCommand, "ui-layout-short"]) {
    await writeFile(
      join(workspace, ".haarness", "commands", name + ".md"),
      "---\ndescription: " +
        JSON.stringify(
          "A command description " + "unbroken-description-".repeat(100),
        ) +
        "\n---\nReturn UI command evidence.\n",
    );
  }
  await writeFile(
    join(workspace, ".haarness", "config.toml"),
    '[defaults]\nprovider="echo"\nmodel="echo-1"\n[permissions]\nbash="ask"\n[providers.desktop-missing-key]\nprotocol="anthropic-messages"\nname="Desktop credential fixture"\nbase_url="http://127.0.0.1:1/v1"\nmodels=["fixture"]\napi_key_env="HAARNESS_TEST_MISSING_KEY"\n',
  );
  await writeFile(
    join(workspace, ".haarness", "workflows", "fixture.js"),
    'export const meta = { name: "fixture", description: "Desktop verification", phases: ["run"] };\nphase("run"); return {ok: true};\n',
  );
  await writeFile(
    join(workspace, "src", "greeting.ts"),
    'export const greeting = "hello";\n',
  );
  await writeFile(
    join(workspace, "README.md"),
    "# Desktop fixture\n\nA private workspace.\n",
  );
  for (const args of [
    ["init", "-b", "main"],
    ["add", "."],
    [
      "-c",
      "user.name=Fixture",
      "-c",
      "user.email=fixture@example.test",
      "commit",
      "-m",
      "Initial fixture",
    ],
  ])
    execFileSync("git", args, { cwd: workspace, stdio: "ignore" });
  await mkdir(join(temporary, "plugin", ".claude-plugin"), { recursive: true });
  await writeFile(
    join(temporary, "plugin", ".claude-plugin", "plugin.json"),
    JSON.stringify({
      name: "desktop-fixture",
      version: "1.0.0",
      description: "Desktop plugin verification",
    }),
  );
  // Keep extension sources separate from runtime state and MCP capture files.
  await mkdir(join(temporary, "extension"));
  await writeFile(
    join(temporary, "extension", "index.ts"),
    "export default {apiVersion:1,name:'desktop_extension',tools:{ping:{description:'Desktop verification tool',parameters:{type:'object'},execute(){return 'extension-proof'}}}};\n",
  );
  await mkdir(join(temporary, "desktop"));
  await writeFile(
    join(temporary, "desktop", "desktop.json"),
    JSON.stringify({
      preferences: {},
      recent: [],
      values: {},
      credentials: {},
    }),
  );
  issuer = createServer(async (request, response) => {
    const url = new URL(request.url ?? "/", "http://127.0.0.1");
    for await (const _chunk of request) {
      // Drain the body; the fixture answers by path alone.
    }
    const json = (status: number, value: unknown) => {
      response.writeHead(status, { "Content-Type": "application/json" });
      response.end(JSON.stringify(value));
    };
    if (url.pathname === "/api/accounts/deviceauth/usercode")
      return json(200, { device_auth_id: "fixture-device", user_code: "ABCD-EFGH", interval: "1" });
    if (url.pathname === "/api/accounts/deviceauth/token") {
      if (issuerPolls++ === 0) return json(403, { error: "authorization_pending" });
      return json(200, { authorization_code: "fixture-code", code_verifier: "fixture-verifier" });
    }
    if (url.pathname === "/oauth/token")
      return json(200, { access_token: "fixture-access", refresh_token: "fixture-refresh", expires_in: 3600 });
    json(404, { error: "not found" });
  });
  await new Promise<void>((resolve) => issuer!.listen(0, "127.0.0.1", resolve));
  const issuerAddress = issuer.address();
  if (!issuerAddress || typeof issuerAddress === "string") throw Error("Missing issuer address");
  const issuerOrigin = "http://127.0.0.1:" + issuerAddress.port;
  env = Object.fromEntries(
    Object.entries(process.env).filter(
      (entry): entry is [string, string] => entry[1] !== undefined,
    ),
  );
  Object.assign(env, {
    HAARNESS_BIN: binary,
    HAARNESS_OPENAI_OAUTH_ISSUER: issuerOrigin,
    // Keep model discovery for the fixture-connected providers off the network.
    OPENAI_BASE_URL: issuerOrigin + "/v1",
    ANTHROPIC_BASE_URL: issuerOrigin + "/v1",
    HAARNESS_DATA_DIR: join(temporary, "state"),
    HAARNESS_DESKTOP_DATA: join(temporary, "desktop"),
    HAARNESS_TEST_PROVIDER: "1",
    HAARNESS_DESKTOP_TEST: "1",
    HAARNESS_VOICE_TEST_HELPER: join(temporary, "voice-fixture"),
  });
  if (process.env.HAARNESS_RIPGREP_E2E === "1") {
    env.HAARNESS_HOME = join(temporary, "isolated haarness home");
    env.HOME = join(temporary, "isolated home");
    await mkdir(env.HOME, { recursive: true });
    await writeFile(
      join(env.HOME, ".profile"),
      "PATH=/usr/bin:/bin; export PATH\n",
    );
    env.PATH = "/usr/bin:/bin";
    env.SHELL = "/bin/sh";
    delete env.RIPGREP_CONFIG_PATH;
  }
  await writeFile(join(temporary, "voice-events.json"), "[]");
  await writeFile(
    join(temporary, "voice-fixture"),
    `#!/usr/bin/env python3
import sys, json, pathlib, threading, time, os
root = pathlib.Path(__file__).parent
(root / 'voice-pid').write_text(str(os.getpid()))
def emit(data):
    print(json.dumps(data), flush=True)
def controls():
    for line in sys.stdin:
        if line.strip() == 'stop':
            emit({'type':'transcript','text':'dictated words final','isFinal':True})
        emit({'type':'ended'})
        os._exit(0)
    os._exit(0)
threading.Thread(target=controls, daemon=True).start()
if (root / 'voice-preparing').exists():
    emit({'type':'preparing','message':'Downloading speech model…'})
else:
    emit({'type':'started'})
index = 0
while True:
    try:
        values = json.loads((root / 'voice-events.json').read_text())
        for item in values[index:]: emit(item)
        index = len(values)
    except (ValueError, FileNotFoundError): pass
    time.sleep(.02)
`,
  );
  await chmod(join(temporary, "voice-fixture"), 0o755);
  delete env.ELECTRON_RUN_AS_NODE;
  delete env.HAARNESS_TEST_MISSING_KEY;
  // Begin with a real daemon using the former disabled project setting.
  const previous = JSON.parse(
    execFileSync(binary, ["--workspace", workspace, "--disable-project-config", "desktop"], {
      env,
      input: JSON.stringify({ action: "connect" }),
      encoding: "utf8",
      timeout: 240000,
    }),
  );
  const restricted = new Haarness({
    baseUrl: previous.url,
    token: previous.token,
  });
  const initial = await restricted.info();
  expect(initial.projectTrusted).toBe(false);
  const retained = await restricted.createThread({
    title: "Existing conversation",
    provider: "echo",
    model: "echo-1",
  });
  restricted.close();
  const executablePath = process.env.HAARNESS_DESKTOP_EXECUTABLE;
  const launchEnv = { ...env };
  if (executablePath) delete launchEnv.HAARNESS_BIN;
  app = await electron.launch({
    timeout: 240000,
    ...(executablePath ? { executablePath, args: [] } : { args: ["."] }),
    env: launchEnv,
  });
  page = await app.firstWindow();
  page.on("pageerror", (error) => errors.push(error.message));
  // Stub only the operating-system picker; selection, IPC and runtime are real.
  await app.evaluate(({ dialog }, directory) => {
    const original = dialog.showOpenDialog;
    dialog.showOpenDialog = async () => {
      dialog.showOpenDialog = original;
      return { canceled: false, filePaths: [directory] };
    };
  }, workspace);
  await page.getByRole("button", { name: "Open another project", exact: true }).click();
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
    {
      timeout: 240000,
    },
  );
  await expect(
    page.getByRole("dialog", { name: "Open project", exact: true }),
  ).toHaveCount(0);
  const opened = await page.evaluate(async (directory) => {
    const id = "local:" + directory;
    return {
      info: await window.haarness.api<ServerInfo>(id, "info", []),
      recent: (await window.haarness.boot()).recent.find(
        (item) => item.id === id,
      ),
    };
  }, workspace);
  expect(opened.info.projectTrusted).toBe(true);
  expect(opened.info.serverInstanceId).not.toBe(initial.serverInstanceId);
  expect(opened.recent?.trust).toBe(true);
  await attachClient();
  expect((await client.getThread(retained.id)).title).toBe(
    "Existing conversation",
  );
});
test.afterEach(async () => {
  if (test.info().status !== test.info().expectedStatus && client) {
    await page.screenshot({ path: test.info().outputPath("electron-failure.png") }).catch(() => {});
    const body = await page.locator("body").innerText().catch(() => "Window closed");
    await test.info().attach("electron-ui.txt", { body, contentType: "text/plain" });
    const events = [];
    try {
      for await (const event of client.events({
        after: 0,
        reconnect: false,
        signal: AbortSignal.timeout(500),
      })) {
        if (event.type.startsWith("configuration.")) events.push(event);
      }
    } catch {}
    await test.info().attach("configuration-events.json", {
      body: JSON.stringify(events, null, 2),
      contentType: "application/json",
    });
    const state = await Promise.allSettled([
      client.info(),
      client.activeTurns(),
      client.mcpServers(),
    ]);
    await test.info().attach("server-state.json", {
      body: JSON.stringify(state, null, 2),
      contentType: "application/json",
    });
  }
});
test.afterAll(async () => {
  if (page)
    await page.screenshot({ path: "test-results/desktop.png" }).catch(() => {});
  client?.close();
  issuer?.closeAllConnections();
  issuer?.close();
  if (app) {
    await app.evaluate(({ BrowserWindow }) => { for (const win of BrowserWindow.getAllWindows()) win.destroy(); }).catch(() => {});
    await app.close().catch(() => {});
  }
  for (const directory of [workspace, ...extraWorkspaces].filter(Boolean)) {
    try {
      execFileSync(
        binary,
        ["--workspace", directory, "--trust-project", "serve", "--stop"],
        { env, stdio: "ignore", timeout: 30_000 },
      );
    } catch {}
    stopChatHome(binary, env);
  }
  // A stopping server can still be writing its state as the folder goes.
  if (temporary)
    await rm(temporary, { recursive: true, force: true, maxRetries: 10 });
});

test("workspace opening is direct, enables project configuration, and preserves explicit settings and active work", async () => {
  expect((await client.info()).projectTrusted).toBe(true);
  expect(
    (await client.commands()).some((command) => command.name === longCommand),
  ).toBe(true);
  const instance = (await client.info()).serverInstanceId;
  await app.evaluate(({ dialog }) => {
    const original = dialog.showOpenDialog;
    dialog.showOpenDialog = async () => {
      dialog.showOpenDialog = original;
      return { canceled: true, filePaths: [] };
    };
  });
  await page
    .getByRole("button", { name: "Open another project", exact: true })
    .click();
  await expect(
    page.getByRole("dialog", { name: "Open project", exact: true }),
  ).toHaveCount(0);
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
  );
  expect((await client.info()).serverInstanceId).toBe(instance);

  const thread = await client.createThread({
    title: "Configuration policy",
    provider: "echo",
    model: "echo-1",
  });
  const turn = await client.startTurn(thread.id, {
    requestId: crypto.randomUUID(),
    text: '!tool bash {"command":"printf configuration-proof"}',
  });
  await expect
    .poll(async () => (await client.listPermissions()).length)
    .toBe(1);
  const failure = await page.evaluate(async (directory) => {
    try {
      await window.haarness.connect({ workspace: directory, trust: false });
      return "";
    } catch (error) {
      return String(error);
    }
  }, workspace);
  expect(failure).toContain("active turn");
  expect((await client.info()).serverInstanceId).toBe(instance);
  expect((await client.getTurn(turn.id)).status).toBe("running");
  await client.replyPermission(
    (await client.listPermissions())[0].permissionId,
    { decision: "once" },
  );
  expect((await client.waitTurn(turn.id)).status).toBe("completed");

  await page.getByRole("button", { name: "Settings", exact: true }).click();
  const settings = page.getByRole("dialog", { name: "Settings", exact: true });
  await settings
    .getByRole("button", { name: "Projects", exact: true })
    .click();
  const currentFolder = settings.getByRole("region", { name: "Current working folder" });
  await expect(currentFolder).toContainText(workspace);
  await expect(settings.getByRole("button", { name: "Change project trust", exact: true })).toHaveCount(0);
  expect(await settings.locator(".settings-content > :first-child").getAttribute("data-setting")).toBe("Current working folder");
  await currentFolder.getByRole("button", { name: "Refresh project" }).click();
  await page.evaluate(directory => window.haarness.connect({ workspace: directory, trust: false }), workspace);
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
  );
  await settings.getByRole("button", { name: "Close settings", exact: true }).click();
  await page.reload();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  await settings.getByRole("button", { name: "Projects", exact: true }).click();
  await expect(currentFolder).toContainText("Project configuration disabled");
  await settings.getByRole("button", { name: "General", exact: true }).click();
  // Without project configuration only the global value applies: no project override is offered.
  const delivery = settings.locator('[data-setting="Messages sent while working"]');
  await expect(delivery.getByRole("radiogroup", { name: "Messages sent while working" })).toBeVisible();
  await expect(delivery.getByRole("button", { name: "Override for this project" })).toHaveCount(0);
  await settings.getByRole("button", { name: "Browser", exact: true }).click();
  await expect(settings.getByRole("combobox", { name: "Browser settings scope" })).toContainText("Global default");
  // Settings changes may overlap a background sidebar refresh or another
  // window opening the same project. The final explicit choice must persist.
  const concurrent = await page.evaluate(async (directory) => {
    const recent = "local:" + directory;
    await Promise.all([
      window.haarness.connect({ workspace: directory, trust: true }),
      window.haarness.connect({ recent }),
      window.haarness.connect({ workspace: directory, trust: false }),
      window.haarness.connect({ recent }),
    ]);
    return {
      info: await window.haarness.api<ServerInfo>(recent, "info", []),
      saved: (await window.haarness.boot()).recent.find(
        (item) => item.id === recent,
      ),
    };
  }, workspace);
  expect(concurrent.info.projectTrusted).toBe(false);
  expect(concurrent.saved?.trust).toBe(false);
  await settings
    .getByRole("button", { name: "Close settings", exact: true })
    .click();
  await page.reload();
  await page.getByRole("region", { name: "Project workspace", exact: true }).locator(".project-heading").click();
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
  );
  const saved = await page.evaluate(async (directory) => {
    const id = "local:" + directory;
    return {
      info: await window.haarness.api<ServerInfo>(id, "info", []),
      recent: (await window.haarness.boot()).recent.find(
        (item) => item.id === id,
      ),
    };
  }, workspace);
  expect(saved.info.projectTrusted).toBe(false);
  expect(saved.recent?.trust).toBe(false);
  await app.evaluate(({ dialog }, directory) => {
    const original = dialog.showOpenDialog;
    dialog.showOpenDialog = async () => {
      dialog.showOpenDialog = original;
      return { canceled: false, filePaths: [directory] };
    };
  }, workspace);
  await page
    .getByRole("button", { name: "Open another project", exact: true })
    .click();
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
  );
  await expect(
    page.getByRole("button", { name: "New chat", exact: true }),
  ).toBeEnabled();
  // Wait for the requested reconnect, rather than reading the previous ready
  // view while its SDK client is being detached.
  await expect.poll(() => page.evaluate(async (directory) => {
    try { return (await window.haarness.api<ServerInfo>("local:" + directory, "info", [])).projectTrusted; }
    catch (error) {
      if (String(error).includes("aborted")) return false;
      throw error;
    }
  }, workspace)).toBe(true);
  await attachClient();
  expect((await client.getThread(thread.id)).title).toBe(
    "Configuration policy",
  );
  expect(errors).toEqual([]);
});

test("first-message titles update tabs and sidebar while the task is still waiting for permission", async () => {
  await expandPrimaryProject();
  await newSession();
  const prompt = '!tool bash {"command":"printf title-proof"}';
  await send(prompt);
  await expect(
    page.getByRole("button", { name: "Allow once", exact: true }),
  ).toBeVisible();
  const permission = (await client.listPermissions()).find((item) =>
    JSON.stringify(item.detail).includes("title-proof"),
  )!;
  expect(permission).toBeTruthy();
  expect(permission.turnId).toBeTruthy();
  const title = ("Echo: " + prompt).slice(0, 50);
  const sidebar = page.locator(".sidebar-lists");
  await expect(
    sidebar.getByRole("button", { name: title, exact: true }),
  ).toBeVisible();
  await expect(page.locator(".session-bar")).toContainText(title);
  expect((await client.getTurn(permission.turnId!)).status).toBe("running");
  expect(
    (await client.listItems(permission.threadId)).some(
      (item) => item.data.type === "assistant_message",
    ),
  ).toBe(false);
  await page.screenshot({ path: "test-results/title-during-permission.png" });
  await page.reload();
  await expect(page.locator(".session-bar")).toContainText(title);
  await expect(
    sidebar.getByRole("button", { name: title, exact: true }),
  ).toBeVisible();
  await page.getByRole("button", { name: "Allow once", exact: true }).click();
  await expect
    .poll(() => client.activeTurns().then((turns) => turns.length))
    .toBe(0);
  // A session created by another client also gets its title without opening it.
  const external = await client.createThread({
    provider: "echo",
    model: "echo-1",
  });
  await client.startTurn(external.id, {
    requestId: crypto.randomUUID(),
    text: "Review the project architecture",
  });
  await expect(
    sidebar.getByRole("button", {
      name: "Echo: Review the project architecture",
      exact: true,
    }),
  ).toBeVisible();
  await expect(page.locator(".session-bar")).toContainText(title);
  expect(errors).toEqual([]);
});

test("provider failures appear live without leaving chat and survive replay exactly once", async () => {
  const thread = await client.createThread({
    title: "Missing credential",
    provider: "desktop-missing-key",
    model: "fixture",
  });
  await page
    .locator(".sessions")
    .getByRole("button", { name: "Missing credential", exact: true })
    .click();
  await client.startTurn(thread.id, {
    requestId: crypto.randomUUID(),
    text: "hi",
  });
  const alert = page.locator(".transcript").getByRole("alert");
  await expect(alert).toContainText("Desktop credential fixture is not connected", {
    timeout: 5000,
  });
  await expect(alert).toHaveCount(1);
  await expect
    .poll(() => client.activeTurns().then((turns) => turns.length))
    .toBe(0);
  expect(
    (await client.listItems(thread.id)).filter(
      (item) => item.data.type === "error",
    ),
  ).toHaveLength(1);
  await openProjectPanel("Files");
  await expect(alert).toBeVisible();
  await page
    .getByRole("button", { name: "Hide side panel", exact: true })
    .click();
  await page.reload();
  await page.getByRole("region", { name: "Project workspace", exact: true }).locator(".project-heading").click();
  await expect(alert).toContainText("Desktop credential fixture is not connected");
  await expect(alert).toHaveCount(1);
  // Return the session to the deterministic provider; recovery uses the same chat.
  await client.updateThread(thread.id, { provider: "echo", model: "echo-1" });
  await expect(
    page.getByRole("combobox", { name: "Model", exact: true }),
  ).toHaveAttribute("data-value", "echo/echo-1");
  await send("credential-recovery-proof");
  await expect(page.locator(".transcript")).toContainText(
    "Echo: credential-recovery-proof",
  );
});

test("projects and sessions keep user ordering through switching, activity and reload", async () => {
  const other = join(temporary, "another-project");
  await mkdir(join(other, ".haarness"), { recursive: true });
  await writeFile(
    join(other, ".haarness", "config.toml"),
    '[defaults]\nprovider="echo"\nmodel="echo-1"\n',
  );
  extraWorkspaces.push(other);
  const headings = page.locator(".project-list .project-heading");
  await app.evaluate(({ dialog }, directory) => {
    const original = dialog.showOpenDialog;
    dialog.showOpenDialog = async () => {
      dialog.showOpenDialog = original;
      return { canceled: false, filePaths: [directory] };
    };
  }, other);
  await page
    .getByRole("button", { name: "Open another project", exact: true })
    .click();
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
  );
  await expect(headings).toHaveText(["workspace", "another-project"]);
  await headings.filter({ hasText: /^workspace$/ }).click();
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
  );
  await expect(headings).toHaveText(["workspace", "another-project"]);
  const projectHandle = page.getByRole("button", {
    name: "workspace",
    exact: true,
  });
  await projectHandle.focus();
  await projectHandle.press("Alt+End");
  await expect(headings).toHaveText(["another-project", "workspace"]);
  await expect(projectHandle).toBeFocused();
  await headings.filter({ hasText: /^another-project$/ }).click();
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
  );
  await headings.filter({ hasText: /^workspace$/ }).click();
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
  );
  await expect(headings).toHaveText(["another-project", "workspace"]);
  await projectHandle.dragTo(page.locator(".project-row").first(), {
    targetPosition: { x: 30, y: 2 },
  });
  await expect(headings).toHaveText(["workspace", "another-project"]);

  const threads = [];
  for (const title of ["Order Alpha", "Order Beta", "Order Gamma"])
    threads.push(
      await client.createThread({ title, provider: "echo", model: "echo-1" }),
    );
  // Project-title clicks toggle disclosure; ordering does not force it open.
  const disclosure = page.getByRole("button", {
    name: /^(Expand|Collapse) project workspace$/,
  });
  if ((await disclosure.getAttribute("aria-expanded")) !== "true")
    await disclosure.click();
  await expect(disclosure).toHaveAttribute("aria-expanded", "true");
  const titles = page.locator(".sessions .session-title");
  await expect(titles.first()).toHaveText("Order Gamma");
  const alpha = page.getByRole("button", {
    name: "Order Alpha",
    exact: true,
  });
  await alpha.focus();
  await alpha.press("Alt+Home");
  await expect(titles.first()).toHaveText("Order Alpha");
  await expect(alpha).toBeFocused();
  await client.updateThread(threads[0].id, { title: "Order Alpha" });
  await expect(alpha).toBeFocused();
  await client.updateThread(threads[1].id, { title: "Order Beta renamed" });
  await expect(titles.first()).toHaveText("Order Alpha");
  await page
    .locator(".sessions")
    .getByRole("button", { name: "Order Alpha", exact: true })
    .click();
  await send("ordering-activity-proof");
  await expect(page.locator(".transcript")).toContainText(
    "Echo: ordering-activity-proof",
  );
  await page
    .getByRole("button", {
      name: "Order Beta renamed",
      exact: true,
    })
    .dragTo(page.locator(".session-row").first(), {
      targetPosition: { x: 30, y: 2 },
    });
  await expect(titles.first()).toHaveText("Order Beta renamed");
  const before = await titles.allTextContents();
  await page.reload();
  await expect(disclosure).toHaveAttribute("aria-expanded", "true");
  await expect(headings).toHaveText(["workspace", "another-project"]);
  await expect(titles).toHaveText(before);
  await expect(page.locator(".transcript")).toContainText(
    "Echo: ordering-activity-proof",
  );
  expect(errors).toEqual([]);
});

test("the live plan stays above the composer, tracks progress, and follows the selected conversation", async () => {
  const thread = await client.createThread({
    title: "Plan presentation",
    provider: "echo",
    model: "echo-1",
  });
  await page
    .locator(".sessions")
    .getByRole("button", { name: "Plan presentation", exact: true })
    .click();
  const dock = page.getByRole("region", { name: "Plan", exact: true });
  const toggle = dock.getByRole("button", { name: /^(Expand|Collapse) plan$/ });
  const todos = [
    {
      content: "Audit application source and configuration for security risks",
      status: "in_progress",
    },
    {
      content: "Audit dependencies against current published vulnerabilities",
      status: "pending",
    },
    {
      content:
        "Synthesize prioritized findings and concrete remediation advice",
      status: "pending",
    },
  ];
  // A live plan is only observable while its turn runs without asking
  // anything: a waiting prompt takes the plan's place. `stage` keeps the turn
  // running a minute (`finish` stops it); `park` holds it on a question.
  async function stage(items: typeof todos) {
    const turn = await client.startTurn(thread.id, {
      requestId: crypto.randomUUID(),
      text:
        "!linger 60000 !multi " +
        JSON.stringify([{ name: "todowrite", arguments: { todos: items } }]),
    });
    const shape = (list: { content: string; status: string }[]) =>
      JSON.stringify(list.map((item) => [item.content, item.status]));
    await expect
      .poll(() => client.listTodos(thread.id).then(shape))
      .toBe(shape(items));
    return turn;
  }
  async function park(items: typeof todos) {
    const turn = await client.startTurn(thread.id, {
      requestId: crypto.randomUUID(),
      text:
        "!multi " +
        JSON.stringify([
          { name: "todowrite", arguments: { todos: items } },
          {
            name: "question",
            arguments: {
              prompt: "Continue plan verification?",
              choices: ["Continue"],
            },
          },
        ]),
    });
    await expect
      .poll(() =>
        client
          .listQuestions()
          .then((items) => items.some((item) => item.threadId === thread.id)),
      )
      .toBe(true);
    return turn;
  }
  async function finish(turn: { id: string }) {
    const question = (await client.listQuestions()).find(
      (item) => item.threadId === thread.id,
    );
    // A parked turn is answered; a lingering one is stopped so the test does
    // not wait out the provider's clock.
    if (question)
      await client.replyQuestion(question.questionId, { answer: "Continue" });
    else await client.interruptThreadTree(thread.id);
    expect(["completed", "interrupted"]).toContain(
      (await client.waitTurn(turn.id)).status,
    );
    // The terminal status lands a moment before the runner releases the
    // thread; the next turn must not race that release.
    await expect
      .poll(() =>
        client
          .activeTurns()
          .then((items) => items.some((item) => item.threadId === thread.id)),
      )
      .toBe(false);
  }

  let turn = await stage(todos);
  await expect(dock).toBeVisible();
  await expect(page.locator(".composer-area .plan-dock")).toHaveCount(1);
  await expect(page.locator('.transcript [data-type="todo"]')).toHaveCount(0);
  await expect(toggle).toHaveAttribute("aria-expanded", "true");
  await expect(dock.locator(".plan-content")).toHaveText(
    todos.map((item) => item.content),
  );
  await expect(dock.locator(".plan-count")).toHaveText("0 of 3 completed");
  // One round glyph per step: the step at work turns while the turn runs.
  await expect(
    dock.locator('li[data-status="in_progress"] .plan-spinner'),
  ).toHaveCount(1);
  await expect(dock.locator('li[data-status="pending"] svg')).toHaveCount(2);
  await expect(dock).toHaveAttribute("data-running", "true");
  await toggle.click();
  await expect(dock.locator(".plan-list")).toBeHidden();
  await expect(dock.locator(".plan-preview")).toHaveText(todos[0].content);
  await toggle.press("Enter");
  await expect(dock.locator(".plan-list")).toBeVisible();
  await toggle.press("Space");
  await expect(dock.locator(".plan-list")).toBeHidden();
  await toggle.click();
  await page.reload();
  await expect(dock.locator(".plan-list")).toBeVisible();
  await expect(dock.locator(".plan-content")).toHaveText(
    todos.map((item) => item.content),
  );
  await finish(turn);
  await expect(toggle).toHaveAttribute("aria-expanded", "false");
  await expect(dock).not.toHaveAttribute("data-running", "true");
  await expect(dock.locator(".plan-toggle .plan-spinner")).toHaveCount(1);

  const other = await client.createThread({
    title: "No plan here",
    provider: "echo",
    model: "echo-1",
  });
  await page
    .locator(".sessions")
    .getByRole("button", { name: "No plan here", exact: true })
    .click();
  await expect(dock).toHaveCount(0);
  await page
    .getByRole("tab", { name: "Plan presentation", exact: true })
    .click();
  await expect(dock).toBeVisible();
  await expect(toggle).toHaveAttribute("aria-expanded", "false");
  expect(await client.listTodos(other.id)).toEqual([]);

  todos[0].status = "completed";
  todos[1].status = "in_progress";
  todos[2].status = "cancelled";
  turn = await stage(todos);
  await expect(toggle).toHaveAttribute("aria-expanded", "true");
  await expect(dock.locator(".plan-count")).toHaveText(
    "1 of 3 completed · 1 cancelled",
  );
  await expect(
    dock.locator('li[data-status="completed"] .plan-content'),
  ).toHaveText(todos[0].content);
  await expect(
    dock.locator('li[data-status="in_progress"] .plan-content'),
  ).toHaveText(todos[1].content);
  await expect(
    dock.locator('li[data-status="cancelled"] .plan-content'),
  ).toHaveText(todos[2].content);
  await page.screenshot({ path: "test-results/plan-dock.png" });
  const size = await app.evaluate(({ BrowserWindow }) =>
    BrowserWindow.getAllWindows()[0].getSize(),
  );
  await app.evaluate(({ BrowserWindow }) =>
    BrowserWindow.getAllWindows()[0].setSize(800, 650),
  );
  await page.emulateMedia({ reducedMotion: "reduce" });
  await expect(
    page.getByRole("textbox", { name: "Message", exact: true }),
  ).toBeInViewport();
  await expect(dock).toBeInViewport();
  expect(
    await dock.evaluate(
      (element) => element.scrollWidth <= element.clientWidth + 2,
    ),
  ).toBe(true);
  await page.screenshot({ path: "test-results/plan-dock-narrow.png" });
  await app.evaluate(
    ({ BrowserWindow }, size) =>
      BrowserWindow.getAllWindows()[0].setSize(size[0], size[1]),
    size,
  );
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await finish(turn);

  // A waiting question takes the tray's place; once answered, an unfinished
  // plan returns.
  await expect(toggle).toHaveAttribute("aria-expanded", "false");
  turn = await park(todos);
  await expect(page.locator(".question")).toBeVisible();
  await expect(dock).toHaveCount(0);
  await finish(turn);
  await expect(dock).toBeVisible();
  // Finishing the checklist removes even a manually expanded tray while the
  // turn is still waiting for input, and it stays absent after reloading.
  await toggle.click();
  todos[1].status = "completed";
  turn = await park(todos);
  await expect(dock).toHaveCount(0);
  await page.reload();
  await expect(page.locator(".question")).toBeVisible();
  await expect(dock).toHaveCount(0);
  await finish(turn);
  await client.revertThread(thread.id, {});
  await expect(dock.locator(".plan-count")).toHaveText(
    "1 of 3 completed · 1 cancelled",
  );
  await client.unrevertThread(thread.id);
  await expect(dock).toHaveCount(0);
  const lengthy = Array.from({ length: 14 }, (_, index) => ({
    content: `Review module ${index + 1}: ${"a-long-configuration-path/".repeat(6)}`,
    status: index === 13 ? "in_progress" : "completed",
  }));
  turn = await stage(lengthy);
  await expect(toggle).toHaveAttribute("aria-expanded", "true");
  const list = dock.locator(".plan-list");
  await expect
    .poll(() => list.evaluate((element) => element.scrollTop))
    .toBeGreaterThan(0);
  expect(await list.evaluate((element) => element.clientHeight <= 160)).toBe(
    true,
  );
  expect(
    await dock.evaluate(
      (element) => element.scrollWidth <= element.clientWidth + 2,
    ),
  ).toBe(true);
  const appearance = await page.evaluate(async () => {
    const preferences = (await window.haarness.boot()).preferences;
    await window.haarness.preferences({ ...preferences, theme: "light" });
    return preferences;
  });
  await page.reload();
  await expect(dock.locator('li[data-status="in_progress"]')).toBeInViewport();
  await page.screenshot({ path: "test-results/plan-dock-light.png" });
  await dock.getByRole("button", { name: "Dismiss plan", exact: true }).click();
  await expect(dock).toHaveCount(0);
  await expect(
    page.getByRole("textbox", { name: "Message", exact: true }),
  ).toBeFocused();
  await page.reload();
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
  );
  await expect(dock).toHaveCount(0);
  await page
    .locator(".sessions")
    .getByRole("button", { name: "No plan here", exact: true })
    .click();
  await expect(dock).toHaveCount(0);
  await page
    .getByRole("tab", { name: "Plan presentation", exact: true })
    .click();
  await expect(dock).toHaveCount(0);
  await finish(turn);
  // A status-only update, even in another turn, retains the dismissal.
  lengthy[0].status = "pending";
  turn = await stage(lengthy);
  await expect(dock).toHaveCount(0);
  expect((await client.listTodos(thread.id))[0].status).toBe("pending");
  await finish(turn);
  turn = await stage([...lengthy].reverse());
  await expect(dock).toHaveCount(0);
  await finish(turn);
  // A replacement checklist opens normally and has its own dismissal.
  const next = [
    { content: "Verify the next checklist", status: "in_progress" },
  ];
  turn = await stage(next);
  await expect(dock.locator(".plan-content")).toHaveText([next[0].content]);
  await dock
    .getByRole("button", { name: "Dismiss plan", exact: true })
    .press("Enter");
  await expect(dock).toHaveCount(0);
  await finish(turn);
  next[0].status = "completed";
  turn = await stage(next);
  await expect(dock).toHaveCount(0);
  await finish(turn);
  // Restarting a completed checklist creates a fresh plan, even with the same text.
  next[0].status = "in_progress";
  turn = await stage(next);
  await expect(dock.locator(".plan-content")).toHaveText([next[0].content]);
  await finish(turn);
  // All-completed lists disappear without any manual dismissal as well.
  next[0].status = "completed";
  turn = await stage(next);
  await expect(dock).toHaveCount(0);
  await finish(turn);
  next[0].status = "in_progress";
  turn = await stage([]);
  await expect(dock).toHaveCount(0);
  await finish(turn);
  turn = await stage(next);
  await expect(dock.locator(".plan-content")).toHaveText([next[0].content]);
  await finish(turn);
  turn = await stage([]);
  await expect(dock).toHaveCount(0);
  await finish(turn);
  await page.reload();
  await expect(dock).toHaveCount(0);
  await page.evaluate(
    (preferences) => window.haarness.preferences(preferences),
    appearance,
  );
  await page.reload();
  expect(errors).toEqual([]);
});

test("multiple projects expand independently with live conversations and persistent collapse state", async () => {
  const primary = page.getByRole("region", {
    name: "Project workspace",
    exact: true,
  });
  // This scenario also runs after tests that intentionally collapse the folder.
  const expand = primary.getByRole("button", { name: "Expand project workspace", exact: true });
  if (await expand.count()) await expand.click();
  const original = await client.createThread({
    title: "Primary project chat",
    provider: "echo",
    model: "echo-1",
  });
  await primary
    .getByRole("button", { name: "Primary project chat", exact: true })
    .click();
  await send("primary-project-transcript");
  await expect(page.locator(".transcript")).toContainText(
    "Echo: primary-project-transcript",
  );

  const directory = join(temporary, "parallel-project");
  extraWorkspaces.push(directory);
  await mkdir(join(directory, ".haarness"), { recursive: true });
  await writeFile(
    join(directory, ".haarness", "config.toml"),
    '[defaults]\nprovider="echo"\nmodel="echo-1"\n',
  );
  await app.evaluate(({ dialog }, path) => {
    const original = dialog.showOpenDialog;
    dialog.showOpenDialog = async () => {
      dialog.showOpenDialog = original;
      return { canceled: false, filePaths: [path] };
    };
  }, directory);
  await page
    .getByRole("button", { name: "Open another project", exact: true })
    .click();
  const secondary = page.getByRole("region", {
    name: "Project parallel-project",
    exact: true,
  });
  await expect(secondary).toHaveClass(/current/);
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
  );
  const result = JSON.parse(
    execFileSync(
      binary,
      ["--workspace", directory, "--trust-project", "desktop"],
      {
        env,
        input: JSON.stringify({ action: "connect" }),
        encoding: "utf8",
        timeout: 240000,
      },
    ),
  );
  let other = new Haarness({ baseUrl: result.url, token: result.token });
  try {
    const selected = await other.createThread({
      title: "Secondary exact chat",
      provider: "echo",
      model: "echo-1",
    });
    await other.createThread({
      title: "Secondary other chat",
      provider: "echo",
      model: "echo-1",
    });
    const initial = await other.startTurn(selected.id, {
      requestId: crypto.randomUUID(),
      text: "secondary-project-transcript",
    });
    await other.waitTurn(initial.id);
    await primary
      .getByRole("button", { name: "Primary project chat", exact: true })
      .click();
    await expect(page.locator(".transcript")).toContainText(
      "Echo: primary-project-transcript",
    );
    await expect(primary.locator(".sessions")).toBeVisible();
    await expect(secondary.locator(".sessions")).toBeVisible();
    const composer = page.getByRole("textbox", {
      name: "Message",
      exact: true,
    });
    await composer.fill("draft stays here while projects expand");
    await secondary
      .getByRole("button", {
        name: "Collapse project parallel-project",
        exact: true,
      })
      .click();
    await expect(secondary.locator(".sessions")).toHaveCount(0);
    await expect(primary).toHaveClass(/current/);
    await expect(composer).toHaveValue(
      "draft stays here while projects expand",
    );
    await secondary
      .getByRole("button", {
        name: "Expand project parallel-project",
        exact: true,
      })
      .click();
    await expect(
      secondary.getByRole("button", {
        name: "Secondary exact chat",
        exact: true,
      }),
    ).toBeVisible();
    await expect(primary).toHaveClass(/current/);
    await expect(composer).toHaveValue(
      "draft stays here while projects expand",
    );
    const external = await other.createThread({
      title: "Live inactive conversation",
      provider: "echo",
      model: "echo-1",
    });
    await expect(
      secondary.getByRole("button", {
        name: "Live inactive conversation",
        exact: true,
      }),
    ).toBeVisible({ timeout: 5000 });
    await other.updateThread(external.id, { title: "Live inactive renamed" });
    await expect(
      secondary.getByRole("button", {
        name: "Live inactive renamed",
        exact: true,
      }),
    ).toBeVisible({ timeout: 5000 });
    const activeTurn = await other.startTurn(selected.id, {
      requestId: crypto.randomUUID(),
      text: "::mdslow",
    });
    const row = secondary.locator(".session-row").filter({
      has: page.locator(".session-title", {
        hasText: "Secondary exact chat",
      }),
    });
    await expect(row.locator(".session-status")).toHaveClass(/running/, {
      timeout: 1000,
    });
    await other.waitTurn(activeTurn.id);
    await expect(row.locator(".session-status")).not.toHaveClass(/running/);
    await expect(primary).toHaveClass(/current/);
    await composer.fill("");
    await secondary
      .getByRole("button", { name: "Secondary exact chat", exact: true })
      .click();
    await expect(secondary).toHaveClass(/current/);
    await expect(page.locator(".transcript")).toContainText(
      "Echo: secondary-project-transcript",
    );
    await expect(primary.locator(".sessions")).toBeVisible();
    await primary
      .getByRole("button", { name: "Primary project chat", exact: true })
      .click();
    await expect(page.locator(".transcript")).toContainText(
      "Echo: primary-project-transcript",
    );

    // Reconnect the inactive project's overview without disturbing the active chat.
    const previousInstance = (await other.info()).serverInstanceId;
    await other.shutdownServer();
    other.close();
    await expect
      .poll(
        async () => {
          try {
            return await page.evaluate(
              async (id) =>
                (await window.haarness.api<ServerInfo>(id, "info", []))
                  .serverInstanceId,
              "local:" + directory,
            );
          } catch {
            return previousInstance;
          }
        },
        { timeout: 60000 },
      )
      .not.toBe(previousInstance);
    const next = JSON.parse(
      execFileSync(
        binary,
        ["--workspace", directory, "--trust-project", "desktop"],
        {
          env,
          input: JSON.stringify({ action: "connect" }),
          encoding: "utf8",
          timeout: 240000,
        },
      ),
    );
    other = new Haarness({ baseUrl: next.url, token: next.token });
    await other.updateThread(external.id, {
      title: "Inactive reconnect proof",
    });
    await expect(
      secondary.getByRole("button", {
        name: "Inactive reconnect proof",
        exact: true,
      }),
    ).toBeVisible({ timeout: 5000 });
    await expect(page.locator(".transcript")).toContainText(
      "Echo: primary-project-transcript",
    );
    await page.screenshot({ path: "test-results/expanded-projects.png" });

    // Explicitly collapsing the current project does not change the open chat.
    await primary
      .getByRole("button", { name: "Collapse project workspace", exact: true })
      .click();
    await expect(primary.locator(".sessions")).toHaveCount(0);
    await expect(page.locator(".transcript")).toContainText(
      "Echo: primary-project-transcript",
    );
    await page.reload();
    await expect(
      primary.getByRole("button", {
        name: "Expand project workspace",
        exact: true,
      }),
    ).toHaveAttribute("aria-expanded", "false");
    await expect(
      secondary.getByRole("button", {
        name: "Collapse project parallel-project",
        exact: true,
      }),
    ).toHaveAttribute("aria-expanded", "true");
    await expect(
      secondary.getByRole("button", {
        name: "Inactive reconnect proof",
        exact: true,
      }),
    ).toBeVisible();
    await page
      .locator(".project-heading")
      .filter({ hasText: /^workspace/ })
      .click();
    await expect(primary.locator(".sessions")).toBeVisible();
    await expect(page.locator(".transcript")).toContainText(
      "Echo: primary-project-transcript",
    );
    await expect(
      primary.getByRole("button", {
        name: "Primary project chat",
        exact: true,
      }),
    ).toHaveAttribute("aria-current", "true");
    const toArchive = await other.createThread({
      title: "Archive without switching",
      provider: "echo",
      model: "echo-1",
    });
    const archive = secondary.getByRole("button", {
      name: "Archive chat Archive without switching",
      exact: true,
    });
    await secondary
      .getByRole("button", { name: toArchive.title, exact: true })
      .hover();
    await archive.click();
    await expect
      .poll(() =>
        other.getThread(toArchive.id).then((thread) => thread.archived),
      )
      .toBe(true);
    await expect(primary).toHaveClass(/current/);
    await expect(page.locator(".transcript")).toContainText(
      "Echo: primary-project-transcript",
    );
    await secondary
      .getByRole("button", {
        name: "Collapse project parallel-project",
        exact: true,
      })
      .click();
    expect((await client.getThread(original.id)).title).toBe(
      "Primary project chat",
    );
    expect(errors).toEqual([]);
  } finally {
    other.close();
  }
});

test("forked conversations appear in the sidebar through rename, archive, project switching and reload", async () => {
  const project = page.getByRole("region", {
    name: "Project workspace",
    exact: true,
  });
  const parent = await client.createThread({
    title: "Sidebar fork source",
    provider: "echo",
    model: "echo-1",
  });
  await project
    .getByRole("button", { name: parent.title, exact: true })
    .click();
  await send("fork-history-proof");
  await expect(page.locator(".transcript")).toContainText(
    "Echo: fork-history-proof",
  );
  await page
    .getByRole("button", { name: "Chat actions", exact: true })
    .click();
  await page.getByRole("menuitem", { name: "Fork", exact: true }).click();
  const title = "Fork of " + parent.title;
  await expect
    .poll(async () =>
      (await client.listThreads()).some((thread) => thread.title === title),
    )
    .toBe(true);
  const fork = (await client.listThreads()).find(
    (thread) => thread.title === title,
  )!;
  expect(fork.parentId).toBe(parent.id);
  await page.getByRole("button", { name: "Search", exact: true }).click();
  const search = page.getByRole("combobox", {
    name: "Search Haarness",
    exact: true,
  });
  await search.fill(title);
  await expect(
    page.getByRole("option", { name: new RegExp(title) }),
  ).toBeVisible();
  await search.press("Escape");
  const forkButton = project.getByRole("button", { name: title, exact: true });
  await expect(forkButton).toBeVisible({ timeout: 5000 });
  await expect(forkButton).toHaveAttribute("aria-current", "true");
  await expect(page.locator(".transcript")).toContainText(
    "Echo: fork-history-proof",
  );

  // Visibility depends on the conversation, not its title or ancestry depth.
  const nested = await client.forkThread(fork.id, {
    title: "Alternative approach",
  });
  expect(nested.parentId).toBe(fork.id);
  await expect(
    project.getByRole("button", { name: nested.title, exact: true }),
  ).toBeVisible();
  await page
    .getByRole("button", { name: "Chat actions", exact: true })
    .click();
  await page.getByRole("menuitem", { name: "Rename", exact: true }).click();
  const rename = page.getByRole("dialog", {
    name: "Rename chat",
    exact: true,
  });
  await rename
    .getByRole("textbox", { name: "Title", exact: true })
    .fill("Renamed experiment");
  await rename.getByRole("button", { name: "Save", exact: true }).click();
  const renamed = project.getByRole("button", {
    name: "Renamed experiment",
    exact: true,
  });
  await expect(renamed).toBeVisible();
  await client.updateThread(parent.id, { archived: true });
  await expect(
    project.getByRole("button", { name: parent.title, exact: true }),
  ).toHaveCount(0);
  await expect(renamed).toBeVisible();
  await expect(
    project.getByRole("button", { name: nested.title, exact: true }),
  ).toBeVisible();
  const grip = project.getByRole("button", {
    name: "Renamed experiment",
    exact: true,
  });
  await grip.focus();
  await grip.press("Alt+Home");
  await expect(project.locator(".session-title").first()).toHaveText(
    "Renamed experiment",
  );

  const directory = join(temporary, "fork-other-project");
  extraWorkspaces.push(directory);
  await mkdir(join(directory, ".haarness"), { recursive: true });
  await writeFile(
    join(directory, ".haarness", "config.toml"),
    '[defaults]\nprovider="echo"\nmodel="echo-1"\n',
  );
  await app.evaluate(({ dialog }, path) => {
    const original = dialog.showOpenDialog;
    dialog.showOpenDialog = async () => {
      dialog.showOpenDialog = original;
      return { canceled: false, filePaths: [path] };
    };
  }, directory);
  await page
    .getByRole("button", { name: "Open another project", exact: true })
    .click();
  const other = page.getByRole("region", {
    name: "Project fork-other-project",
    exact: true,
  });
  await expect(other).toHaveClass(/current/);
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
  );
  await expect(renamed).toBeVisible();
  const background = await client.forkThread(parent.id, {
    title: "Background fork",
  });
  await expect(
    project.getByRole("button", { name: background.title, exact: true }),
  ).toBeVisible({ timeout: 5000 });
  await expect(other.locator(".session-title")).toHaveCount(0);
  await renamed.click();
  await expect(project).toHaveClass(/current/);
  await expect(renamed).toHaveAttribute("aria-current", "true");
  await expect(page.locator(".transcript")).toContainText(
    "Echo: fork-history-proof",
  );
  const order = await project.locator(".session-title").allTextContents();
  await page.reload();
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
  );
  if (
    await project
      .getByRole("button", { name: "Expand project workspace", exact: true })
      .count()
  )
    await project
      .getByRole("button", { name: "Expand project workspace", exact: true })
      .click();
  await expect(project.locator(".session-title")).toHaveText(order);
  await expect(renamed).toHaveAttribute("aria-current", "true");
  await expect(page.locator(".transcript")).toContainText(
    "Echo: fork-history-proof",
  );

  // Archiving still hides that conversation; it does not hide its descendants.
  await client.updateThread(fork.id, { archived: true });
  await expect(renamed).toHaveCount(0);
  await expect(
    project.getByRole("button", { name: nested.title, exact: true }),
  ).toBeVisible();
  await client.updateThread(fork.id, { archived: false });
  await expect(renamed).toHaveCount(1);
  await expect(project.locator(".session-title")).toHaveText(order);
  await page.screenshot({ path: "test-results/fork-sidebar.png" });
  await other
    .getByRole("button", {
      name: "Collapse project fork-other-project",
      exact: true,
    })
    .click();
  expect(errors).toEqual([]);
});

test("a fork of an untitled chat reads as one, never as the server's placeholder", async () => {
  const project = page.getByRole("region", {
    name: "Project workspace",
    exact: true,
  });
  const untitled = await client.createThread({
    provider: "echo",
    model: "echo-1",
  });
  // The server's placeholder: "New chat" ("New thread" from an older server;
  // Desktop handles both).
  expect(untitled.title).toMatch(/^New (thread|chat)$/);
  // Forked before a title is generated, the fork is named after it.
  const fork = await client.forkThread(untitled.id, {});
  expect(fork.title).toMatch(/^Fork of New (thread|chat)$/);
  const row = project.getByRole("button", {
    name: "Fork of untitled chat",
    exact: true,
  });
  await expect(row).toBeVisible();
  await expect(project.getByText(/New thread|Fork of New chat/)).toHaveCount(0);
  await project.screenshot({ path: test.info().outputPath("fork-of-untitled.png") });
  await row.click();
  await expect(page.locator(".session-bar-name, .workspace-tab.active")).toContainText(
    "Fork of untitled chat",
  );
  // A rename starts from the name shown, never from the placeholder.
  await page.getByRole("button", { name: "Chat actions", exact: true }).click();
  await page.getByRole("menuitem", { name: "Rename", exact: true }).click();
  const rename = page.getByRole("dialog", { name: "Rename chat", exact: true });
  await expect(rename.getByRole("textbox", { name: "Title", exact: true })).toHaveValue(
    "Fork of untitled chat",
  );
  await rename.getByRole("button", { name: "Cancel", exact: true }).click();
  await client.updateThread(fork.id, { archived: true });
  await client.updateThread(untitled.id, { archived: true });
  expect(errors).toEqual([]);
});

test("whole rows reorder silently, project titles expand, and sessions have one archive action", async () => {
  const directory = join(temporary, "reorder-project");
  extraWorkspaces.push(directory);
  await mkdir(join(directory, ".haarness"), { recursive: true });
  await writeFile(
    join(directory, ".haarness", "config.toml"),
    '[defaults]\nprovider="echo"\nmodel="echo-1"\n',
  );
  await app.evaluate(({ dialog }, path) => {
    const original = dialog.showOpenDialog;
    dialog.showOpenDialog = async () => {
      dialog.showOpenDialog = original;
      return { canceled: false, filePaths: [path] };
    };
  }, directory);
  await page
    .getByRole("button", { name: "Open another project", exact: true })
    .click();
  const other = page.getByRole("region", {
    name: "Project reorder-project",
    exact: true,
  });
  await expect(other).toHaveClass(/current/);
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
  );
  const headings = page.locator(".project-list .project-heading");
  const before = await headings.allTextContents();
  const first = page.locator(".project-group").first();
  // Both collapsed: the common two-project case must accept centre drops both ways.
  for (const group of [first, other]) {
    const collapse = group.getByRole("button", { name: /^Collapse project / });
    if (await collapse.count()) await collapse.click();
  }
  const source = other.locator(".project-heading");
  await source.dragTo(first.locator(".project-heading"));
  await expect(headings.first()).toHaveText("reorder-project");
  await source.dragTo(headings.last());
  await expect(headings).toHaveText(before);
  await expect(page.locator(".toast")).toHaveCount(0);
  await expect(page.locator(".reorder-handle")).toHaveCount(0);
  await expect(page.getByRole("button", { name: /^Reorder / })).toHaveCount(0);
  // Clicking the label expands it; opening another project leaves this one expanded.
  await source.click();
  await expect(
    other.getByRole("button", {
      name: "Collapse project reorder-project",
      exact: true,
    }),
  ).toBeVisible();
  const project = page.getByRole("region", {
    name: "Project workspace",
    exact: true,
  });
  await project.locator(".project-heading").click();
  await expect(
    project.getByRole("button", {
      name: "Collapse project workspace",
      exact: true,
    }),
  ).toBeVisible();
  await expect(
    other.getByRole("button", {
      name: "Collapse project reorder-project",
      exact: true,
    }),
  ).toBeVisible();
  await source.focus();
  await source.press("Alt+Home");
  await expect(headings.first()).toHaveText("reorder-project");
  await source.press("Alt+End");
  await expect(headings).toHaveText(before);
  // Double-clicked, each keeps a tab of its own (a plain click previews).
  for (const title of ["Menu One", "Menu Two"]) {
    await client.createThread({ title, provider: "echo", model: "echo-1" });
    await project.getByRole("button", { name: title, exact: true }).dblclick();
  }
  const one = project.getByRole("button", { name: "Menu One", exact: true });
  const two = project.getByRole("button", { name: "Menu Two", exact: true });
  await one.press("Alt+Home");
  await expect(project.locator(".session-title").first()).toHaveText(
    "Menu One",
  );
  await one.dragTo(two);
  await expect(project.locator(".session-title").first()).toHaveText(
    "Menu Two",
  );
  await one.dragTo(two);
  await expect(project.locator(".session-title").first()).toHaveText(
    "Menu One",
  );
  await expect(two).toHaveAttribute("aria-current", "true");
  const tab = page.getByRole("tab", { name: "Menu Two", exact: true });
  await tab.press("Alt+Home");
  await expect(page.locator(".workspace-tabs [role=tab]").first()).toHaveText(
    "Menu Two",
  );
  await tab.press("Alt+ArrowRight");
  await expect(page.locator(".workspace-tabs [role=tab]").nth(1)).toHaveText(
    "Menu Two",
  );
  const sessions = await project.locator(".session-title").allTextContents();
  await page.reload();
  await expect(headings).toHaveText(before);
  await expect(project.locator(".session-title")).toHaveText(sessions);
  const row = project.locator(".session-row").filter({
    has: page.getByRole("button", { name: "Menu One", exact: true }),
  });
  await row.hover();
  await expect(row.getByRole("button")).toHaveCount(2);
  await row
    .getByRole("button", { name: "Archive chat Menu One", exact: true })
    .click();
  await expect(one).toHaveCount(0);
  await expect(two).toHaveAttribute("aria-current", "true");
  await expect
    .poll(
      async () =>
        (await client.listThreads({ archived: true })).find(
          (item) => item.title === "Menu One",
        )?.archived,
    )
    .toBe(true);
  await expect(page.locator(".toast")).toHaveCount(0);
  await other
    .getByRole("button", {
      name: "Collapse project reorder-project",
      exact: true,
    })
    .click();
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  const settings = page.getByRole("dialog", { name: "Settings", exact: true });
  await settings.getByRole("button", { name: "Profiles", exact: true }).click();
  await settings
    .getByRole("button", { name: "Manage catalog", exact: true })
    .click();
  const catalogMenu = settings.getByRole("menu", {
    name: "Manage catalog",
    exact: true,
  });
  await expect(catalogMenu).toBeVisible();
  await catalogMenu
    .getByRole("menuitem", { name: "List profiles", exact: true })
    .click();
  const catalog = page.getByRole("dialog", {
    name: "List profiles",
    exact: true,
  });
  await expect(
    catalog.getByRole("textbox", { name: "Catalog server URL", exact: true }),
  ).toBeVisible();
  await expect(
    catalog.getByRole("combobox", { name: "Action", exact: true }),
  ).toHaveCount(0);
  await catalog.getByRole("button", { name: "Cancel", exact: true }).click();
  await settings
    .getByRole("button", { name: "Close settings", exact: true })
    .click();
  expect(errors).toEqual([]);
});

test("the project menu pins, renames, files under a section, reveals, archives chats and removes a project", async () => {
  const options = (name: string) =>
    page.getByRole("button", { name: `Project options for ${name}`, exact: true });
  const item = (name: string | RegExp) =>
    page.getByRole("menuitem", { name, exact: typeof name === "string" });
  const confirmDialog = (title: string, action: string) =>
    page
      .getByRole("dialog", { name: title, exact: true })
      .getByRole("button", { name: action, exact: true });
  const headings = page.locator(".project-list .project-heading");
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");

  // Rename: a custom display name replaces the folder name wherever the project is shown.
  await options("workspace").click();
  await item("Rename…").click();
  const edit = page.getByRole("dialog", { name: "Rename project", exact: true });
  await expect(edit.getByLabel("Folder")).toHaveAttribute("readonly", "");
  await edit.getByLabel("Name").fill("Client app");
  await edit.getByRole("button", { name: "Save", exact: true }).click();
  const renamed = page.getByRole("region", { name: "Project Client app", exact: true });
  await expect(renamed.locator(".project-heading")).toHaveText("Client app");

  // Section: a new section files the project under its heading, whose menu renames it.
  await options("Client app").click();
  await item("Section").click();
  await item("New section…").click();
  const create = page.getByRole("dialog", { name: "New section", exact: true });
  await create.getByLabel("Name").fill("Clients");
  await create.getByRole("button", { name: "Create", exact: true }).click();
  const section = page.getByRole("region", { name: "Section Clients", exact: true });
  await expect(section.locator(".project-heading")).toHaveText("Client app");
  await options("Client app").click();
  await item("Section").click();
  await expect(
    page.getByRole("menuitemradio", { name: "Clients", exact: true }),
  ).toHaveAttribute("aria-checked", "true");
  await page.keyboard.press("Escape");
  await section
    .getByRole("button", { name: "Section options for Clients", exact: true })
    .click();
  await item("Rename section…").click();
  const rename = page.getByRole("dialog", { name: "Rename section", exact: true });
  await rename.getByLabel("Name").fill("Client work");
  await rename.getByRole("button", { name: "Save", exact: true }).click();
  const renamedSection = page.getByRole("region", { name: "Section Client work", exact: true });
  await expect(renamedSection.locator(".project-heading")).toHaveText("Client app");

  // Pin: the project floats above every section and stays pinned across a reload.
  await options("Client app").click();
  await item("Pin").click();
  await expect(renamed.locator(".project-heading")).toHaveAttribute("aria-description", "Pinned");
  await expect(renamedSection.locator(".project-heading")).toHaveText("Client app");
  await expect(headings.first()).toHaveText("Client app");
  await page.reload();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
  await expect(renamed.locator(".project-heading")).toHaveAttribute("aria-description", "Pinned");
  await expect(renamedSection.locator(".project-heading")).toHaveText("Client app");
  await options("Client app").click();
  await item("Unpin").click();
  await expect(renamed.locator(".project-heading")).not.toHaveAttribute("aria-description", "Pinned");
  await expect(renamedSection.locator(".project-heading")).toHaveText("Client app");
  await renamedSection
    .getByRole("button", { name: "Section options for Client work", exact: true })
    .click();
  await item("Remove section").click();
  await confirmDialog("Remove section Client work?", "Remove section").click();
  await expect(renamedSection).toHaveCount(0);
  await expect(renamed.locator(".project-heading")).toHaveText("Client app");

  // Reveal: the folder goes to the system file manager through the main process.
  await app.evaluate(({ shell }) => {
    const scope = globalThis as { revealed?: string[] };
    scope.revealed = [];
    shell.showItemInFolder = (target: string) => {
      scope.revealed!.push(target);
    };
  });
  await options("Client app").click();
  await item(/^(Reveal in Finder|Show in Explorer|Show in file manager)$/).click();
  await expect
    .poll(() => app.evaluate(() => (globalThis as { revealed?: string[] }).revealed))
    .toEqual([await realpath(workspace)]);

  // Archive chats: every conversation leaves the sidebar and the API shows it archived.
  const disclosure = renamed.getByRole("button", {
    name: /^(Expand|Collapse) project Client app$/,
  });
  if ((await disclosure.getAttribute("aria-expanded")) !== "true") await disclosure.click();
  const titles = ["Archive All One", "Archive All Two"];
  const created = [];
  for (const title of titles)
    created.push(await client.createThread({ title, provider: "echo", model: "echo-1" }));
  await expect(renamed.getByRole("button", { name: titles[1], exact: true })).toBeVisible();
  await options("Client app").click();
  await item("Archive chats").click();
  await confirmDialog("Archive all chats in Client app?", "Archive chats").click();
  for (const title of titles)
    await expect(renamed.getByRole("button", { name: title, exact: true })).toHaveCount(0);
  await expect
    .poll(async () =>
      (await client.listThreads({ archived: true }))
        .filter((thread) => titles.includes(thread.title))
        .map((thread) => thread.archived),
    )
    .toEqual([true, true]);
  for (const thread of created) await client.updateThread(thread.id, { archived: false });

  // An empty name restores the folder name, so later scenarios keep addressing "workspace".
  await options("Client app").click();
  await item("Rename…").click();
  await edit.getByLabel("Name").fill("");
  await edit.getByRole("button", { name: "Save", exact: true }).click();
  const original = page.getByRole("region", { name: "Project workspace", exact: true });
  await expect(original.locator(".project-heading")).toHaveText("workspace");

  // Remove project: a freshly opened folder leaves the sidebar after confirmation.
  const directory = join(temporary, "remove-project");
  extraWorkspaces.push(directory);
  await mkdir(join(directory, ".haarness"), { recursive: true });
  await writeFile(
    join(directory, ".haarness", "config.toml"),
    '[defaults]\nprovider="echo"\nmodel="echo-1"\n',
  );
  await app.evaluate(({ dialog }, path) => {
    const original = dialog.showOpenDialog;
    dialog.showOpenDialog = async () => {
      dialog.showOpenDialog = original;
      return { canceled: false, filePaths: [path] };
    };
  }, directory);
  await page.getByRole("button", { name: "Open another project", exact: true }).click();
  const other = page.getByRole("region", { name: "Project remove-project", exact: true });
  await expect(other).toHaveClass(/current/);
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
  await options("remove-project").click();
  await item("Remove project").click();
  await confirmDialog("Remove remove-project from the sidebar?", "Remove project").click();
  await expect(other).toHaveCount(0);
  await original.locator(".project-heading").click();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
});

test("sidebar sections keep drag and keyboard ordering inside and across groups, and the flat list names projects", async () => {
  const options = (name: string) =>
    page.getByRole("button", { name: `Project options for ${name}`, exact: true });
  const item = (name: string) => page.getByRole("menuitem", { name, exact: true });
  const region = (name: string) => page.getByRole("region", { name, exact: true });
  const titles = (scope: Locator) => scope.locator(".project-heading");
  const heading = (scope: Locator, name: string) =>
    scope.locator(".project-heading", { hasText: new RegExp(`^${name}$`) });
  const openProject = async (name: string) => {
    const directory = join(temporary, name);
    extraWorkspaces.push(directory);
    await mkdir(join(directory, ".haarness"), { recursive: true });
    await writeFile(
      join(directory, ".haarness", "config.toml"),
      '[defaults]\nprovider="echo"\nmodel="echo-1"\n',
    );
    await app.evaluate(({ dialog }, path) => {
      const original = dialog.showOpenDialog;
      dialog.showOpenDialog = async () => {
        dialog.showOpenDialog = original;
        return { canceled: false, filePaths: [path] };
      };
    }, directory);
    await page.getByRole("button", { name: "Open another project", exact: true }).click();
    await expect(region(`Project ${name}`)).toHaveClass(/current/);
    await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
  };
  const fileUnder = async (name: string, section: string) => {
    await options(name).click();
    await item("Section").click();
    await page.getByRole("menuitemradio", { name: section, exact: true }).click();
  };
  await openProject("alpha-project");
  await openProject("beta-project");
  // Collapsed rows keep drop targets unambiguous.
  for (const name of ["workspace", "alpha-project", "beta-project"]) {
    const collapse = region(`Project ${name}`).getByRole("button", {
      name: `Collapse project ${name}`,
      exact: true,
    });
    if (await collapse.count()) await collapse.click();
  }
  const all = page.locator(".project-list .project-heading");
  await expect(all).toContainText(["workspace", "alpha-project", "beta-project"]);

  // A new section files alpha and lists first; the rest is named as filed under none.
  await options("alpha-project").click();
  await item("Section").click();
  await item("New section…").click();
  const create = page.getByRole("dialog", { name: "New section", exact: true });
  await create.getByLabel("Name").fill("Clients");
  await create.getByRole("button", { name: "Create", exact: true }).click();
  const clients = region("Section Clients");
  const unfiled = region("Section No section");
  await expect(titles(clients)).toHaveText(["alpha-project"]);
  await expect(titles(unfiled)).toContainText(["workspace", "beta-project"]);
  await expect(all.first()).toHaveText("alpha-project");
  await expect(
    unfiled.getByRole("button", { name: "Section options for No section", exact: true }),
  ).toHaveCount(0);

  // Filing through the radio joins beta; the shared order still decides its place.
  await fileUnder("beta-project", "Clients");
  await expect(titles(clients)).toHaveText(["alpha-project", "beta-project"]);
  await expect(heading(unfiled, "beta-project")).toHaveCount(0);

  // Alt+arrows move inside the section only and keep focus.
  const beta = heading(clients, "beta-project");
  await beta.focus();
  await beta.press("Alt+ArrowUp");
  await expect(titles(clients)).toHaveText(["beta-project", "alpha-project"]);
  await expect(beta).toBeFocused();
  await beta.press("Alt+End");
  await expect(titles(clients)).toHaveText(["alpha-project", "beta-project"]);

  // Dropping onto the top edge of a row in another section files the project before it.
  await heading(unfiled, "workspace").dragTo(heading(clients, "alpha-project"), {
    targetPosition: { x: 30, y: 2 },
  });
  await expect(titles(clients)).toHaveText(["workspace", "alpha-project", "beta-project"]);
  await expect(heading(unfiled, "workspace")).toHaveCount(0);

  // A pin keeps a project at the top of its own section, across a reload.
  await options("beta-project").click();
  await item("Pin").click();
  await expect(titles(clients)).toHaveText(["beta-project", "workspace", "alpha-project"]);
  await page.reload();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
  await expect(titles(clients)).toHaveText(["beta-project", "workspace", "alpha-project"]);
  await expect(heading(clients, "beta-project")).toHaveAttribute("aria-description", "Pinned");
  await options("beta-project").click();
  await item("Unpin").click();
  await expect(titles(clients)).toHaveText(["workspace", "alpha-project", "beta-project"]);

  // "No section" unfiles a project; dropping it onto a section row's middle files it after that row.
  await fileUnder("beta-project", "No section");
  await expect(titles(clients)).toHaveText(["workspace", "alpha-project"]);
  await expect(titles(unfiled)).toContainText(["beta-project"]);
  await heading(unfiled, "beta-project").dragTo(heading(clients, "workspace"));
  await expect(titles(clients)).toHaveText(["workspace", "beta-project", "alpha-project"]);
  await expect(heading(unfiled, "beta-project")).toHaveCount(0);

  // Removing the section returns its projects to the plain list in the shared order.
  await clients
    .getByRole("button", { name: "Section options for Clients", exact: true })
    .click();
  await item("Remove section").click();
  await page
    .getByRole("dialog", { name: "Remove section Clients?", exact: true })
    .getByRole("button", { name: "Remove section", exact: true })
    .click();
  await expect(clients).toHaveCount(0);
  await expect(unfiled).toHaveCount(0);
  await expect(all).toContainText(["workspace", "beta-project", "alpha-project"]);
  await expect(page.locator(".toast")).toHaveCount(0);

  // The flat list names each chat's project, including a collapsed one that is
  // not the current project, spaces its two-line rows and survives a reload.
  const thread = await client.createThread({ title: "Flat list row", provider: "echo", model: "echo-1" });
  const layout = async (choice: "In one list" | "By project") => {
    await page.getByRole("button", { name: "Project list options", exact: true }).click();
    await page.getByRole("menuitemradio", { name: choice, exact: true }).click();
  };
  await layout("In one list");
  const flatRow = page.locator(".project-list > .sessions > .session-row", { hasText: "Flat list row" });
  await expect(flatRow.locator(".session-location")).toHaveText("workspace");
  expect((await flatRow.locator("button").first().boundingBox())!.height).toBeGreaterThanOrEqual(40);
  await expect(page.locator(".project-heading")).toHaveCount(0);
  await page.reload();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
  await expect(flatRow.locator(".session-location")).toHaveText("workspace");
  await page.screenshot({ path: "test-results/sidebar-flat.png" });
  // One archive action per row, without leaving the current project.
  await flatRow.getByRole("button", { name: "Archive chat Flat list row", exact: true }).click();
  await expect(flatRow).toHaveCount(0);
  await expect.poll(() => client.getThread(thread.id).then((value) => value.archived)).toBe(true);
  await layout("By project");
  await expect(all).toContainText(["workspace", "beta-project", "alpha-project"]);
  await region("Project workspace").locator(".project-heading").click();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
});

test("new-chat starting area is project-aware, responsive, and keeps starter prompts editable", async () => {
  const existing = new Set(
    (await client.listThreads()).map((thread) => thread.id),
  );
  await newSession();
  expect((await client.listThreads()).map((item) => item.id)).toEqual([
    ...existing,
  ]);
  const start = page.locator(".conversation-empty");
  await expect(start.getByRole("heading")).toHaveText(
    "What should we work on in workspace?",
  );
  const cards = start.locator(".suggestions").getByRole("button");
  await expect(cards).toHaveCount(4);
  const geometry = async () =>
    cards.evaluateAll((elements) =>
      elements.map((element) => {
        const box = element.getBoundingClientRect();
        return {
          x: box.x,
          y: box.y,
          right: box.right,
          bottom: box.bottom,
          width: box.width,
        };
      }),
    );
  const wide = await geometry();
  expect(new Set(wide.map((box) => Math.round(box.y))).size).toBe(1);
  await page.screenshot({ path: "test-results/new-chat-dark.png" });
  const composer = page.getByRole("textbox", { name: "Message", exact: true });
  for (const [label, prompt] of [
    ["Explore code", "Explain the architecture of this project"],
    ["Build a feature", "Help me build a new feature: "],
    ["Review changes", "Review the current changes"],
    ["Fix a problem", "Help me investigate and fix this issue: "],
  ]) {
    await start.getByRole("button", { name: label, exact: true }).click();
    await expect(composer).toHaveValue(prompt);
    await expect(composer).toBeFocused();
    expect((await client.listThreads()).map((item) => item.id)).toEqual([
      ...existing,
    ]);
  }
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  const settings = page.getByRole("dialog", { name: "Settings", exact: true });
  await selectValue(
    settings.getByRole("combobox", { name: "Appearance", exact: true }),
    "light",
  );
  await settings
    .getByRole("button", { name: "Close settings", exact: true })
    .click();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  await expect(start).toBeVisible();
  await page.screenshot({ path: "test-results/new-chat-light.png" });
  await page.setViewportSize({ width: 900, height: 760 });
  // Electron acknowledges the resize before container-query layout settles.
  await expect.poll(async () => new Set((await geometry()).map((box) => Math.round(box.y))).size).toBe(2);
  const narrow = await geometry();
  expect(new Set(narrow.map((box) => Math.round(box.y))).size).toBe(2);
  expect(
    narrow.every((box) => box.x >= 0 && box.right <= 900 && box.width >= 120),
  ).toBe(true);
  await expect(composer).toBeVisible();
  await page.screenshot({ path: "test-results/new-chat-narrow.png" });
  // Keep chat below its 660px container breakpoint beside the default panel.
  await page.setViewportSize({ width: 1300, height: 940 });
  await openProjectPanel("Files");
  await expect(page.locator(".file-tree")).toBeVisible();
  await expect.poll(async () => new Set((await geometry()).map((box) => Math.round(box.y))).size).toBe(2);
  await expect(composer).toBeVisible();
  await page
    .getByRole("button", { name: "Hide side panel", exact: true })
    .click();
  await page.setViewportSize({ width: 1420, height: 940 });
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  await selectValue(
    settings.getByRole("combobox", { name: "Appearance", exact: true }),
    "dark",
  );
  await settings
    .getByRole("button", { name: "Close settings", exact: true })
    .click();
  await composer.fill("start-page-first-message");
  await page.getByRole("button", { name: "Send message", exact: true }).click();
  await expect(start).toHaveCount(0);
  await expect(page.locator(".transcript")).toContainText(
    "Echo: start-page-first-message",
  );
  expect(errors).toEqual([]);
});

async function attachComposerFile(name: string) {
  const path = join(temporary, name);
  await writeFile(path, Buffer.from(
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aQx8AAAAASUVORK5CYII=", "base64",
  ));
  await app.evaluate(({ dialog }, path) => {
    const original = dialog.showOpenDialog;
    dialog.showOpenDialog = async () => {
      dialog.showOpenDialog = original;
      return { canceled: false, filePaths: [path] };
    };
  }, path);
  await attachFiles(page);
  await expect(page.getByRole("button", { name: `Remove ${name}`, exact: true })).toBeVisible();
}

async function fileTransfer(name: string, type = "image/png") {
  return page.evaluateHandle(({ name, type }) => {
    const transfer = new DataTransfer();
    const bytes = Uint8Array.from(atob("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aQx8AAAAASUVORK5CYII="), c => c.charCodeAt(0));
    transfer.items.add(new File([bytes], name, { type }));
    return transfer;
  }, { name, type });
}

test("file drops attach across the chat area and preserve text drop behavior", async () => {
  await newSession();
  const chat = page.getByRole("region", { name: "Chat", exact: true });
  const input = page.getByRole("textbox", { name: "Message", exact: true });
  const emptyFile = await fileTransfer("empty-chat.png");
  await page.locator(".conversation-empty h2").dispatchEvent("dragenter", { dataTransfer: emptyFile });
  await expect(chat).toHaveClass(/file-drop-active/);
  await expect(chat.getByRole("status").filter({ hasText: "Drop files to attach" })).toBeVisible();
  await chat.screenshot({ path: test.info().outputPath("chat-file-drop.png") });
  await page.locator(".conversation-empty h2").dispatchEvent("dragover", { dataTransfer: emptyFile });
  await page.locator(".conversation-empty h2").dispatchEvent("drop", { dataTransfer: emptyFile });
  await expect(page.getByRole("button", { name: "Remove empty-chat.png", exact: true })).toBeVisible();
  await expect(chat).not.toHaveClass(/file-drop-active/);
  await send("Drop area verification");
  await expect(page.locator(".transcript")).toContainText("Echo: Drop area verification");
  for (const [selector, name] of [[".message.assistant p", "transcript.png"], [".transcript", "margin.png"], [".composer textarea", "composer.png"]]) {
    const transfer = await fileTransfer(name);
    await page.locator(selector).last().dispatchEvent("dragenter", { dataTransfer: transfer });
    await page.locator(selector).last().dispatchEvent("drop", { dataTransfer: transfer });
    await expect(page.getByRole("button", { name: `Remove ${name}`, exact: true })).toHaveCount(1);
    await transfer.dispose();
  }
  await expect(page.locator(".composer-attachments > span")).toHaveCount(3);
  const textDropPrevented = await input.evaluate(element => {
    const transfer = new DataTransfer();
    transfer.setData("text/plain", "ordinary text");
    const event = new DragEvent("drop", { dataTransfer: transfer, bubbles: true, cancelable: true });
    element.dispatchEvent(event);
    return event.defaultPrevented;
  });
  expect(textDropPrevented).toBe(false);
  // A file Desktop cannot attach or point to (made by the page, so it has
  // no path) gets a quiet notice, not an error.
  const invalid = await fileTransfer("unsupported.txt", "text/plain");
  await chat.dispatchEvent("drop", { dataTransfer: invalid });
  await expect(page.getByText("Only images, PDFs, audio and video can be attached.", { exact: false })).toBeVisible();
  await expect(page.getByRole("button", { name: "Dismiss error", exact: true })).toHaveCount(0);
  await expect(page.locator(".composer-attachments > span")).toHaveCount(3);
  await send("Dropped files are sent once");
  await expect(page.locator(".composer-attachments")).toHaveCount(0);
  await expect(page.locator('.transcript img[alt="Attached image"]')).toHaveCount(4);
  await emptyFile.dispose();
  await invalid.dispose();
});

test("a dropped text file of the project is mentioned, any other goes into the message as its path at the caret, and undo takes it out", async () => {
  await newSession();
  const chat = page.getByRole("region", { name: "Chat", exact: true });
  const input = page.getByRole("textbox", { name: "Message", exact: true });
  const chips = page.getByLabel("Attached context", { exact: true }).locator(".prompt-context-item");
  const drop = async (path: string, name: string) => {
    const transfer = await page.evaluateHandle(({ path, name }) => {
      const transfer = new DataTransfer();
      transfer.items.add(new File(["# spec"], name, { type: "text/markdown" }));
      // A file the page makes has no path of its own; a real drop's file URL
      // names it, as Electron's own path would.
      transfer.setData("text/uri-list", "file://" + encodeURI(path));
      return transfer;
    }, { path, name });
    for (const type of ["dragenter", "dragover", "drop"])
      await page.locator(".conversation-empty h2").dispatchEvent(type, { dataTransfer: transfer });
    await transfer.dispose();
  };
  // A text file of the project becomes a mention, as choosing it after @ would.
  await mkdir(join(workspace, "docs"), { recursive: true });
  await writeFile(join(workspace, "docs", "dropped notes.md"), "# Dropped notes\n");
  await drop(join(workspace, "docs", "dropped notes.md"), "dropped notes.md");
  await expect(chips.filter({ hasText: "docs/dropped notes.md" })).toHaveCount(1);
  await expect(input).toHaveValue("");
  await chips.filter({ hasText: "docs/dropped notes.md" }).getByRole("button", { name: /Remove/ }).click();
  await expect(chips).toHaveCount(0);

  const before = "Hello\n\nsee this path: ";
  const after = "\n\nAlso!";
  await input.fill(before + after);
  // The caret sits in the middle line, then the focus leaves the message box.
  await input.evaluate((element: HTMLTextAreaElement, at) => {
    element.setSelectionRange(at, at);
    element.blur();
  }, before.length);
  // One the project cannot read (here, not there at all) is named by its path.
  await drop(join(workspace, "docs", "drop spec.md"), "drop spec.md");
  await expect(input).toHaveValue(before + '"docs/drop spec.md"' + after);
  await expect(input).toBeFocused();
  await expect(page.locator(".composer-attachments")).toHaveCount(0);
  await expect(chat).not.toHaveClass(/file-drop-active/);
  await page.keyboard.press(process.platform === "darwin" ? "Meta+z" : "Control+z");
  await expect(input).toHaveValue(before + after);
  await input.fill("");
  // Later tests review the project's changes; this one leaves none.
  await rm(join(workspace, "docs"), { recursive: true, force: true });
  expect(errors).toEqual([]);
});

/** A Cmd+V as the focused element receives it: synthetic, so nothing is
 * inserted natively and only the app's own handling shows. */
async function pasteFocused(data: { file?: string; text?: string }) {
  await page.evaluate((data) => {
    const transfer = new DataTransfer();
    if (data.file) {
      const bytes = Uint8Array.from(atob("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aQx8AAAAASUVORK5CYII="), (c) => c.charCodeAt(0));
      transfer.items.add(new File([bytes], data.file, { type: "image/png" }));
    }
    if (data.text) transfer.setData("text/plain", data.text);
    (document.activeElement ?? document.body).dispatchEvent(
      new ClipboardEvent("paste", { clipboardData: transfer, bubbles: true, cancelable: true }),
    );
  }, data);
}

test("pastes reach the draft after removing an attachment and from anywhere in the chat", async () => {
  await newSession();
  const input = page.getByRole("textbox", { name: "Message", exact: true });
  const chips = page.locator(".composer-attachments > span");
  await input.click();
  // Removing an attachment leaves the focus in the message box, so the next paste works.
  for (const round of [1, 2]) {
    await pasteFocused({ file: `pasted-${round}.png` });
    const remove = page.getByRole("button", { name: `Remove pasted-${round}.png`, exact: true });
    await expect(remove).toBeVisible();
    await remove.click();
    await expect(chips).toHaveCount(0);
    await expect(input).toBeFocused();
  }
  // Removed from the keyboard, the button hands the focus back too.
  await pasteFocused({ file: "keyboard.png" });
  await page.getByRole("button", { name: "Remove keyboard.png", exact: true }).focus();
  await page.keyboard.press("Enter");
  await expect(chips).toHaveCount(0);
  await expect(input).toBeFocused();

  await send("Paste target verification");
  await expect(page.locator(".transcript")).toContainText("Echo: Paste target verification");
  // The conversation has the focus after a click in it: files attach, text goes to the draft.
  const transcript = page.locator(".transcript");
  await transcript.focus();
  await pasteFocused({ file: "from-transcript.png" });
  await expect(page.getByRole("button", { name: "Remove from-transcript.png", exact: true })).toBeVisible();
  await transcript.focus();
  await pasteFocused({ text: "pasted words" });
  await expect(input).toHaveValue("pasted words");
  await expect(input).toBeFocused();
  // With nothing focused, text lands at the caret.
  await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
  await pasteFocused({ text: " and more" });
  await expect(input).toHaveValue("pasted words and more");
  // A field of its own keeps its paste.
  await page.keyboard.press("ControlOrMeta+f");
  const find = page.locator(".conversation-find input");
  await expect(find).toBeFocused();
  await pasteFocused({ text: "find text", file: "find.png" });
  await expect(input).toHaveValue("pasted words and more");
  await expect(chips).toHaveCount(1);
  await page.keyboard.press("Escape");
  await input.fill("");
  await send("Pasted files are sent once");
  await expect(chips).toHaveCount(0);
  await expect(page.locator('.transcript img[alt="Attached image"]')).toHaveCount(1);
  expect(errors).toEqual([]);
});

test("sending from the new-chat screen keeps the message box and never shows the chat loading mark", async () => {
  await newSession();
  await page.evaluate(() => {
    const seen = ((window as any).freshSend = { loading: false, hidden: false });
    const check = () => {
      if (document.querySelector(".chat-loading")) seen.loading = true;
      const box = document.querySelector("[data-composer-input]");
      if (!box?.getClientRects().length) seen.hidden = true;
    };
    const observer = new MutationObserver(check);
    observer.observe(document.body, { subtree: true, childList: true, attributes: true, attributeFilter: ["style", "class"] });
    (window as any).stopFreshSend = () => observer.disconnect();
  });
  const input = page.getByRole("textbox", { name: "Message", exact: true });
  await input.fill("fresh-chat-first-message");
  await page.getByRole("button", { name: "Send message", exact: true }).click();
  await expect(page.locator(".transcript")).toContainText("Echo: fresh-chat-first-message");
  await page.screenshot({ path: test.info().outputPath("new-chat-first-send.png") });
  const seen = await page.evaluate(() => {
    (window as any).stopFreshSend();
    return (window as any).freshSend;
  });
  expect(seen).toEqual({ loading: false, hidden: false });
  await expect(page.getByRole("status", { name: "Loading chat…", exact: true })).toHaveCount(0);
  await expect(input).toBeVisible();
  expect(errors).toEqual([]);
});

test("sidebar file drops open the target session and keep attachments isolated", async () => {
  await newSession();
  await send("drop-source-session");
  await expect(page.locator(".transcript")).toContainText("Echo: drop-source-session");
  await attachComposerFile("source-draft.png");
  const input = page.getByRole("textbox", { name: "Message", exact: true });
  await input.fill("source draft stays here");
  const target = await client.createThread({ title: "Unopened file drop target", provider: "echo", model: "echo-1" });
  const row = page.locator(".session-row").filter({ has: page.getByRole("button", { name: target.title, exact: true }) });
  await expect(row).toBeVisible();
  await expect(page.getByRole("tab", { name: target.title, exact: true })).toHaveCount(0);
  const transfer = await fileTransfer("target.png");
  await row.dispatchEvent("dragenter", { dataTransfer: transfer });
  await expect(row).toHaveClass(/file-drop-active/);
  await row.screenshot({ path: test.info().outputPath("sidebar-file-drop.png") });
  await row.dispatchEvent("drop", { dataTransfer: transfer });
  await expect(row.locator("button").first()).toHaveAttribute("aria-current", "true");
  await expect(page.getByRole("tab", { name: target.title, exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "Remove target.png", exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "Remove source-draft.png", exact: true })).toHaveCount(0);
  await expect(input).toHaveValue("");
  // An already-open target appends instead of replacing its existing draft.
  const another = await fileTransfer("target-next.png");
  await row.locator("button").first().dispatchEvent("drop", { dataTransfer: another });
  await expect(page.locator(".composer-attachments > span")).toHaveCount(2);
  // Hold the actual file read while navigating away from the destination.
  await page.evaluate(() => {
    const original = FileReader.prototype.readAsDataURL;
    FileReader.prototype.readAsDataURL = function (file) {
      FileReader.prototype.readAsDataURL = original;
      (window as any).finishDroppedFile = () => original.call(this, file);
    };
  });
  const delayed = await fileTransfer("delayed.png");
  await row.dispatchEvent("drop", { dataTransfer: delayed });
  await expect.poll(() => page.evaluate(() => typeof (window as any).finishDroppedFile)).toBe("function");
  await page.locator(".session-row").getByRole("button", { name: "Echo: drop-source-session", exact: true }).click();
  await page.evaluate(() => { (window as any).finishDroppedFile(); delete (window as any).finishDroppedFile; });
  await expect(input).toHaveValue("source draft stays here");
  await expect(page.getByRole("button", { name: "Remove source-draft.png", exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "Remove target.png", exact: true })).toHaveCount(0);
  await row.locator("button").first().click();
  await expect(page.locator(".composer-attachments > span")).toHaveCount(3);
  await send("Sidebar drop verification");
  await expect(page.locator(".composer-attachments")).toHaveCount(0);
  await expect(page.locator('.transcript img[alt="Attached image"]')).toHaveCount(3);

  const directory = join(temporary, "file-drop-project");
  extraWorkspaces.push(directory);
  await mkdir(join(directory, ".haarness"), { recursive: true });
  await writeFile(join(directory, ".haarness/config.toml"), '[defaults]\nprovider="echo"\nmodel="echo-1"\n');
  await app.evaluate(({ dialog }, directory) => {
    const original = dialog.showOpenDialog;
    dialog.showOpenDialog = async () => {
      dialog.showOpenDialog = original;
      return { canceled: false, filePaths: [directory] };
    };
  }, directory);
  await page.getByRole("button", { name: "Open another project", exact: true }).click();
  await expect(page.getByRole("region", { name: "Project file-drop-project", exact: true })).toHaveClass(/current/);
  await input.fill("other project draft");
  const crossProject = await fileTransfer("cross-project.png");
  const destination = page.locator(`[data-sort-id=${JSON.stringify(sessionKey("local:" + workspace, target.id))}]`);
  await destination.dispatchEvent("drop", { dataTransfer: crossProject });
  await expect(page.getByRole("region", { name: "Project workspace", exact: true })).toHaveClass(/current/);
  await expect(page.getByRole("button", { name: "Remove cross-project.png", exact: true })).toBeVisible();
  await expect(input).toHaveValue("");
  await expect(destination.locator("button").first()).toHaveAttribute("aria-current", "true");
  await transfer.dispose();
  await another.dispose();
  await delayed.dispose();
  await crossProject.dispose();
});

test("first-message attachments stay with their draft and never leak into a new chat", async () => {
  const path = join(temporary, "draft-proof.png");
  await writeFile(
    path,
    Buffer.from(
      "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aQx8AAAAASUVORK5CYII=",
      "base64",
    ),
  );
  await newSession();
  await app.evaluate(({ dialog }, path) => {
    const original = dialog.showOpenDialog;
    dialog.showOpenDialog = async () => {
      dialog.showOpenDialog = original;
      return { canceled: false, filePaths: [path] };
    };
  }, path);
  await attachFiles(page);
  await expect(
    page.getByRole("button", { name: "Remove draft-proof.png", exact: true }),
  ).toBeVisible();
  await send("The first message includes this image");
  await expect(page.locator(".transcript")).toContainText(
    "Echo: The first message includes this image",
  );
  await expect(
    page.locator('.transcript img[alt="Attached image"]'),
  ).toHaveCount(1);
  await expect(page.locator(".composer-attachments")).toHaveCount(0);
  await attachComposerFile("follow-up.png");
  await send("Follow-up with a fresh image");
  await expect(page.locator(".transcript")).toContainText("Echo: Follow-up with a fresh image");
  await expect(page.locator(".composer-attachments")).toHaveCount(0);
  await expect(page.locator('.transcript img[alt="Attached image"]')).toHaveCount(2);
  await attachComposerFile("image-only.png");
  await page.getByRole("button", { name: "Send message", exact: true }).click();
  await expect(page.locator(".composer-attachments")).toHaveCount(0);
  await expect(page.locator('.transcript img[alt="Attached image"]')).toHaveCount(3);
  await page.locator(".composer").screenshot({ path: test.info().outputPath("attachments-cleared.png") });
  await newSession();
  await expect(page.locator(".composer-attachments")).toHaveCount(0);
  await expect(
    page.getByRole("textbox", { name: "Message", exact: true }),
  ).toHaveValue("");
  expect(errors).toEqual([]);
});

for (const navigate of [false, true]) test(`attachment send cleanup preserves newer files and context ${navigate ? "through navigation" : "in place"} and reload`, async () => {
  await newSession();
  const title = `attachment-race-fixture-${navigate}`;
  await send(title);
  await expect(page.locator(".transcript")).toContainText("Echo: " + title);
  const thread = (await client.listThreads()).find((item) => item.title.includes(title))!;
  expect(thread).toBeTruthy();
  const input = page.getByRole("textbox", { name: "Message", exact: true });
  const context = page.getByLabel("Attached context", { exact: true });
  const mention = async (query: string) => {
    await input.fill("@" + query);
    await page.getByRole("listbox", { name: "Files and agents", exact: true })
      .getByRole("option").filter({ hasText: query }).first().click();
  };
  await attachComposerFile("sent.png");
  await mention("README.md");
  // Hold only the API response, after the real server accepts the message.
  await app.evaluate(({ ipcMain }) => {
    const handlers = (ipcMain as unknown as { _invokeHandlers: Map<string, (...args: any[]) => unknown> })._invokeHandlers;
    const original = handlers.get("haarness:api")!;
    ipcMain.removeHandler("haarness:api");
    ipcMain.handle("haarness:api", async (event, raw) => {
      if (raw.method !== "startTurn") return original(event, raw);
      ipcMain.removeHandler("haarness:api");
      ipcMain.handle("haarness:api", original);
      const response = await original(event, raw);
      await new Promise<void>((resolve) => { (globalThis as any).releaseAttachmentSend = resolve; });
      return response;
    });
  });
  try {
    await send("sent attachment and context");
    await expect.poll(() => app.evaluate(() => typeof (globalThis as any).releaseAttachmentSend)).toBe("function");
    await attachComposerFile("next.png");
    await openProjectPanel("Files");
    await page.getByRole("textbox", { name: "Find files", exact: true }).fill("greeting.ts");
    await page.locator(".file-entry").filter({ hasText: "src/greeting.ts" }).click();
    await page.getByRole("button", { name: "Add file to context", exact: true }).click();
    await expect(context).toContainText("greeting.ts");
    // Saving the attachment draft during navigation must also remove only sent files.
    if (navigate) {
      await newSession();
      await input.fill("unrelated chat draft");
      await attachComposerFile("unrelated.png");
    }
    await app.evaluate(() => { (globalThis as any).releaseAttachmentSend(); delete (globalThis as any).releaseAttachmentSend; });
    await expect.poll(() => page.evaluate(async key => {
      const items = await window.haarness.storage(key) as { path?: string }[];
      return items?.map(item => item.path);
    }, "prompt-context:v1:" + sessionKey("local:" + workspace, thread.id))).toEqual(["src/greeting.ts"]);
    if (navigate) {
      await expect(input).toHaveValue("unrelated chat draft");
      await expect(page.getByRole("button", { name: "Remove unrelated.png", exact: true })).toBeVisible();
      await page.getByRole("button", { name: "Search", exact: true }).click();
      await page.getByRole("combobox", { name: "Search Haarness" }).fill(thread.title);
      await page.getByRole("option").filter({ hasText: thread.title }).first().click();
    }
    await expect(input).toHaveValue("");
    await expect(page.getByRole("button", { name: "Remove sent.png", exact: true })).toHaveCount(0);
    await expect(page.getByRole("button", { name: "Remove next.png", exact: true })).toBeVisible();
    await expect(context).not.toContainText("README.md");
    await expect(context).toContainText("greeting.ts");
    await input.fill("next message survives");
    const key = "draft:local:" + workspace + ":" + thread.id;
    await expect.poll(() => page.evaluate(key => window.haarness.storage(key), key)).toBe("next message survives");
    await page.reload();
    await expect(input).toHaveValue("next message survives");
    await expect(context).toContainText("greeting.ts");
    await expect(context).not.toContainText("README.md");
  } finally {
    await app.evaluate(() => { (globalThis as any).releaseAttachmentSend?.(); delete (globalThis as any).releaseAttachmentSend; });
  }
});

test("failed attachment sends retain their draft for retry and queued sends clear it", async () => {
  await newSession();
  await attachComposerFile("retry.png");
  const input = page.getByRole("textbox", { name: "Message", exact: true });
  const context = page.getByLabel("Attached context", { exact: true });
  await input.fill("@README.md");
  await page.getByRole("listbox", { name: "Files and agents", exact: true })
    .getByRole("option").filter({ hasText: "README.md" }).first().click();
  await app.evaluate(({ ipcMain }) => {
    const handlers = (ipcMain as unknown as { _invokeHandlers: Map<string, (...args: any[]) => unknown> })._invokeHandlers;
    const original = handlers.get("haarness:api")!;
    ipcMain.removeHandler("haarness:api");
    ipcMain.handle("haarness:api", (event, raw) => {
      if (raw.method !== "startTurn") return original(event, raw);
      ipcMain.removeHandler("haarness:api");
      ipcMain.handle("haarness:api", original);
      return { ok: false, error: "Attachment send fixture unavailable" };
    });
  });
  await send("retry this attachment");
  await expect(page.getByText("Attachment send fixture unavailable", { exact: false })).toBeVisible();
  await expect(input).toHaveValue("retry this attachment");
  await expect(page.getByRole("button", { name: "Remove retry.png", exact: true })).toBeVisible();
  await expect(context).toContainText("README.md");
  await page.getByRole("button", { name: "Dismiss error", exact: true }).click();
  await page.getByRole("button", { name: "Send message", exact: true }).click();
  await expect(page.locator(".transcript")).toContainText("Echo: retry this attachment");
  await expect(page.locator(".composer-attachments")).toHaveCount(0);
  await newSession();
  await expect(page.locator(".composer-attachments")).toHaveCount(0);
  await expect(input).toHaveValue("");
  await expect(context).toHaveCount(0);
  await page.reload();
  await expect(input).toHaveValue("");
  await expect(context).toHaveCount(0);
  await send('!tool bash {"command":"printf attachment-queue-proof"}');
  await expect(page.getByRole("button", { name: "Allow once", exact: true })).toBeVisible();
  await attachComposerFile("queued.png");
  await input.fill("queued attachment");
  await page.getByRole("textbox", { name: "Message", exact: true }).press("Alt+Enter");
  await expect(page.getByRole("region", { name: "Queued messages", exact: true })).toContainText("queued attachment");
  await expect(page.locator(".composer-attachments")).toHaveCount(0);
  await expect(input).toHaveValue("");
  await page.getByRole("button", { name: "Stop response", exact: true }).click();
});

test("side-panel launcher opens tools beside chat and supports tab ordering", async () => {
  const header = page.getByRole("banner");
  await expect(header.getByRole("button", { name: "Open files", exact: true })).toHaveCount(0);
  await expect(header.getByRole("button", { name: "Review changes", exact: true })).toHaveCount(0);
  if (
    await page
      .getByRole("button", { name: "Hide side panel", exact: true })
      .count()
  )
    await page
      .getByRole("button", { name: "Hide side panel", exact: true })
      .click();
  await page
    .getByRole("button", { name: "Show side panel", exact: true })
    .click();
  const panelToggle = page.getByRole("button", { name: "Hide side panel", exact: true });
  await expect(panelToggle).toHaveCSS("box-shadow", "none");
  await expect(panelToggle).toHaveCSS("outline-style", "none");
  await panelToggle.press("Tab");
  await expect(page.locator(":focus")).toHaveCSS("outline-style", "solid");
  await page.screenshot({ path: test.info().outputPath("keyboard-focus.png") });
  await page.getByRole("button", { name: "Open panel", exact: true }).click();
  await expect(page.getByRole("button", { name: "Open panel", exact: true })).toHaveCSS("box-shadow", "none");
  const launcher = page.getByRole("region", { name: "Open a project panel" });
  await expect(launcher).toBeVisible();
  await expect(
    page.getByRole("textbox", { name: "Message", exact: true }),
  ).toBeVisible();
  await launcher
    .getByRole("button", { name: "Open Files", exact: true })
    .click();
  await expect(page.locator(".file-tree")).toBeVisible();
  await page.getByRole("button", { name: "Open panel", exact: true }).click();
  await launcher
    .getByRole("button", { name: "Open Review", exact: true })
    .click();
  await expect(page.locator(".review")).toBeVisible();
  await page.screenshot({ path: test.info().outputPath("header-and-project-tabs.png"), animations: "disabled" });
  const reviewHandle = page.getByRole("button", {
    name: "Show Review panel",
    exact: true,
  });
  await reviewHandle.focus();
  await reviewHandle.press("Alt+Home");
  await expect(page.locator(".dock-tab > button:first-child")).toHaveText([
    "Review",
    "Files",
  ]);
  await page.getByRole("button", { name: "Open panel", exact: true }).click();
  await launcher
    .getByRole("button", { name: "Open Terminal", exact: true })
    .click();
  await expect(page.locator(".xterm-screen")).toBeVisible();
  const firstTerminal = (await client.listPtys())[0];
  await client.updatePty(firstTerminal.id, { title: "Terminal One" });
  await page.getByRole("button", { name: "New terminal", exact: true }).click();
  await expect
    .poll(() => client.listPtys().then((items) => items.length))
    .toBe(2);
  const secondTerminal = (await client.listPtys()).find(
    (item) => item.id !== firstTerminal.id,
  )!;
  await client.updatePty(secondTerminal.id, { title: "Terminal Two" });
  const terminalHandle = page.getByRole("button", {
    name: "Terminal Two",
    exact: true,
  });
  await terminalHandle.focus();
  await terminalHandle.press("Alt+Home");
  await expect(
    page.locator(".terminal-tabs > div > button:first-child").first(),
  ).toHaveText("Terminal Two");
  await expect(terminalHandle).toBeFocused();
  await page.screenshot({ path: "test-results/panel-launcher.png" });
  await page
    .getByRole("button", { name: "Hide terminal", exact: true })
    .click();
  await page
    .getByRole("button", { name: "Hide side panel", exact: true })
    .click();
  await page.reload();
  await page.getByRole("region", { name: "Project workspace", exact: true }).locator(".project-heading").click();
  await page
    .getByRole("button", { name: "Toggle terminal", exact: true })
    .click();
  await expect(
    page.locator(".terminal-tabs > div > button:first-child").first(),
  ).toHaveText("Terminal Two");
  await page
    .getByRole("button", { name: "Hide terminal", exact: true })
    .click();
  await client.deletePty(firstTerminal.id);
  await client.deletePty(secondTerminal.id);
});

test("terminal expansion starts a shell and closing tabs never asks for confirmation", async () => {
  const toggle = page.getByRole("button", {
    name: "Toggle terminal",
    exact: true,
  });
  const hide = page.getByRole("button", { name: "Hide terminal", exact: true });
  if (await hide.count()) await hide.click();
  for (const terminal of await client.listPtys())
    await client.deletePty(terminal.id);
  await toggle.click();
  const input = page.locator(".xterm-helper-textarea");
  await expect(input).toBeFocused();
  await expect(
    page.getByRole("button", { name: "Open a terminal", exact: true }),
  ).toHaveCount(0);
  await expect
    .poll(() => client.listPtys().then((items) => items.length))
    .toBe(1);
  const first = (await client.listPtys())[0];
  await client.updatePty(first.id, { title: "Autostart terminal" });
  await input.fill(
    "pwd > terminal-cwd.txt; printf auto-start-proof > terminal-start.txt",
  );
  await input.press("Enter");
  await expect
    .poll(() =>
      readFile(join(workspace, "terminal-start.txt"), "utf8").catch(() => ""),
    )
    .toBe("auto-start-proof");
  expect(
    (await readFile(join(workspace, "terminal-cwd.txt"), "utf8")).trim(),
  ).toBe(workspace);
  await page.screenshot({ path: "test-results/terminal-auto-start.png" });
  await hide.click();
  await page.getByRole("button", { name: "Search", exact: true }).click();
  const search = page.getByRole("combobox", {
    name: "Search Haarness",
    exact: true,
  });
  await search.fill("Toggle terminal");
  await search.press("Enter");
  await expect(input).toBeFocused();
  expect((await client.listPtys()).map((item) => item.id)).toEqual([first.id]);
  await page.getByRole("button", { name: "New terminal", exact: true }).click();
  await expect
    .poll(() => client.listPtys().then((items) => items.length))
    .toBe(2);
  const second = (await client.listPtys()).find(
    (item) => item.id !== first.id,
  )!;
  await client.updatePty(second.id, { title: "Remaining terminal" });
  await page
    .getByRole("button", {
      name: "Close terminal Autostart terminal",
      exact: true,
    })
    .click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect
    .poll(() => client.listPtys().then((items) => items.map((item) => item.id)))
    .toEqual([second.id]);
  const alive = (pid: number) => {
    try {
      process.kill(pid, 0);
      return true;
    } catch {
      return false;
    }
  };
  if (first.pid) await expect.poll(() => alive(first.pid!)).toBe(false);
  if (second.pid) expect(alive(second.pid)).toBe(true);
  await expect(page.locator(".xterm-screen")).toBeVisible();
  await page
    .getByRole("button", {
      name: "Close terminal Remaining terminal",
      exact: true,
    })
    .click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(page.locator(".terminal-panel")).toHaveCount(0);
  expect(await client.listPtys()).toEqual([]);
  if (second.pid) await expect.poll(() => alive(second.pid!)).toBe(false);
  // Reopening after the last close starts a fresh shell; repeated toggles reuse it.
  await toggle.click();
  await expect(input).toBeFocused();
  const fresh = (await client.listPtys())[0];
  expect(fresh.id).not.toBe(first.id);
  expect(fresh.id).not.toBe(second.id);
  for (let index = 0; index < 3; index++) {
    await toggle.click();
    await toggle.click();
  }
  await expect(input).toBeFocused();
  expect((await client.listPtys()).map((item) => item.id)).toEqual([fresh.id]);
  await input.fill("printf reattach-proof > terminal-reattach.txt");
  await input.press("Enter");
  await expect
    .poll(() =>
      readFile(join(workspace, "terminal-reattach.txt"), "utf8").catch(
        () => "",
      ),
    )
    .toBe("reattach-proof");
  await hide.click();
  await client.deletePty(fresh.id);
  expect(errors).toEqual([]);
});

test("a terminal text size change gives the shell its new size", async () => {
  const hide = page.getByRole("button", { name: "Hide terminal", exact: true });
  if (await hide.count()) await hide.click();
  for (const terminal of await client.listPtys())
    await client.deletePty(terminal.id);
  await page.getByRole("button", { name: "Toggle terminal", exact: true }).click();
  await expect(page.locator(".xterm-helper-textarea")).toBeFocused();
  await expect.poll(() => client.listPtys().then((items) => items.length)).toBe(1);
  const pty = async () => (await client.listPtys())[0]!;
  const shownRows = () => page.locator(".terminal-view .xterm-rows > div").count();
  await expect.poll(async () => (await pty()).rows === (await shownRows())).toBe(true);
  const before = await pty();
  // A larger font fits fewer cells in the same panel, which does not resize.
  await page.evaluate(() => window.haarness.preferences({ terminalFontSize: 22 }));
  await expect.poll(async () => (await pty()).cols).toBeLessThan(before.cols);
  await expect.poll(async () => (await pty()).rows).toBeLessThan(before.rows);
  await expect.poll(async () => (await pty()).rows === (await shownRows())).toBe(true);
  await page.evaluate(() => window.haarness.preferences({ terminalFontSize: 13 }));
  await expect.poll(async () => (await pty()).cols).toBe(before.cols);
  await expect.poll(async () => (await pty()).rows).toBe(before.rows);
  await hide.click();
  for (const terminal of await client.listPtys())
    await client.deletePty(terminal.id);
  expect(errors).toEqual([]);
});

test("a terminal shown again does not answer the queries in its replayed output", async () => {
  const hide = page.getByRole("button", { name: "Hide terminal", exact: true });
  const toggle = page.getByRole("button", { name: "Toggle terminal", exact: true });
  if (await hide.count()) await hide.click();
  for (const terminal of await client.listPtys())
    await client.deletePty(terminal.id);
  await toggle.click();
  const input = page.locator(".xterm-helper-textarea");
  await expect(input).toBeFocused();
  // Outside the project, so later tests' changed-file lists stay as they were.
  const live = join(temporary, "replay-live.txt");
  const typed = join(temporary, "replay-typed.txt");
  // Ask for the device attributes: xterm answers, and `read` takes the live
  // answer. Then `cat` records everything the shell receives from here on.
  await input.fill(
    `printf '\\033[c'; read -rs -t 3 -d c reply; printf '%s' "\${#reply}" > ${live}; cat -v > ${typed}`,
  );
  await input.press("Enter");
  await expect
    .poll(() => readFile(live, "utf8").catch(() => ""))
    .toMatch(/^[1-9]\d*$/);
  // Shown again, the terminal replays its output, the `ESC[c` included.
  await hide.click();
  await toggle.click();
  await expect(input).toBeFocused();
  await expect(page.locator(".terminal-view .xterm-rows")).toContainText("replay-typed.txt");
  await page.waitForTimeout(300);
  await input.pressSequentially("done");
  await input.press("Enter");
  await input.press("Control+d");
  // Only the typed line reached `cat`: no `^[[?1;2c` answer to the replay.
  await expect.poll(() => readFile(typed, "utf8").catch(() => "")).toBe("done\n");
  await hide.click();
  for (const terminal of await client.listPtys())
    await client.deletePty(terminal.id);
  expect(errors).toEqual([]);
});

test("a terminal still reaches its shell after a long burst of output", async () => {
  test.setTimeout(300_000);
  const hide = page.getByRole("button", { name: "Hide terminal", exact: true });
  if (await hide.count()) await hide.click();
  for (const terminal of await client.listPtys())
    await client.deletePty(terminal.id);
  await page.getByRole("button", { name: "Toggle terminal", exact: true }).click();
  const input = page.locator(".xterm-helper-textarea");
  await expect(input).toBeFocused();
  const done = join(temporary, "burst-done.txt");
  const after = join(temporary, "burst-after.txt");
  // Far more output than the server buffers for a reader that falls behind.
  await input.fill(
    `yes haarness-burst | head -n 1000000; printf done > ${done}; echo burst-$((6*7))-end`,
  );
  await input.press("Enter");
  await expect
    .poll(() => readFile(done, "utf8").catch(() => ""), { timeout: 120_000 })
    .toBe("done");
  // The view catches up with the end of the burst (the echo prints a text
  // the command line does not hold).
  await expect(page.locator(".terminal-view .xterm-rows")).toContainText(
    "burst-42-end",
    { timeout: 120_000 },
  );
  await input.fill(`printf alive > ${after}`);
  await input.press("Enter");
  await expect
    .poll(() => readFile(after, "utf8").catch(() => ""), { timeout: 30_000 })
    .toBe("alive");
  await hide.click();
  for (const terminal of await client.listPtys())
    await client.deletePty(terminal.id);
  expect(errors).toEqual([]);
});

test("a chat read again after a compaction keeps a reply that arrived during the read", async () => {
  await showChat();
  await newSession();
  await send("history-landing-proof");
  const transcript = page.locator(".transcript");
  await expect(transcript).toContainText("Echo: history-landing-proof");
  let thread = "";
  let sequence = 0;
  for (const item of await client.listThreads()) {
    const items = await client.listItems(item.id);
    if (items.some((value) => value.data.type === "user_message" && value.data.text === "history-landing-proof")) {
      thread = item.id;
      sequence = Math.max(...items.map((value) => value.sequence));
    }
  }
  expect(thread).not.toBe("");
  const info = await client.info();
  const connection = "local:" + workspace;
  // A compaction mid-turn, then the turn's next reply, which the server
  // writes after it answered the history read the compaction asks for.
  await app.evaluate(
    ({ BrowserWindow }, input) => {
      const contents = BrowserWindow.getAllWindows()[0].webContents;
      const event = (eventId: number, type: string, payload: unknown) => ({
        kind: "server",
        connection: input.connection,
        data: {
          eventId,
          type,
          payload,
          threadId: input.thread,
          serverInstanceId: input.instance,
          schemaVersion: "1",
          createdAt: new Date().toISOString(),
        },
      });
      contents.send("haarness:event", event(input.eventId, "thread.compacted", {}));
      contents.send(
        "haarness:event",
        event(input.eventId + 1, "item.created", {
          id: "history-landing-reply",
          threadId: input.thread,
          sequence: input.sequence + 1,
          createdAt: new Date().toISOString(),
          data: { type: "assistant_message", text: "Arrived during the history read" },
        }),
      );
    },
    {
      connection,
      thread,
      instance: info.serverInstanceId,
      eventId: (info.eventCursor ?? 0) + 1_000_000,
      sequence,
    },
  );
  await expect(transcript).toContainText("Arrived during the history read");
  // The read lands within moments; the reply is still there after it.
  await page.waitForTimeout(3000);
  await expect(transcript).toContainText("Arrived during the history read");
  await expect(transcript).toContainText("Echo: history-landing-proof");
  // Follow the real event stream again (the fixture's event ids ran ahead).
  await app.evaluate(({ BrowserWindow }, connection) => {
    BrowserWindow.getAllWindows()[0].webContents.send("haarness:event", {
      kind: "server",
      connection,
      data: { type: "resync_required" },
    });
  }, connection);
  await expect(transcript).not.toContainText("Arrived during the history read");
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
  expect(errors).toEqual([]);
});

test("Ctrl+Tab steps the tabs of the panel that has the keyboard, and the shell gets nothing", async () => {
  test.setTimeout(240_000);
  const hide = page.getByRole("button", { name: "Hide terminal", exact: true });
  if (await hide.count()) await hide.click();
  for (const terminal of await client.listPtys())
    await client.deletePty(terminal.id);
  await page.getByRole("button", { name: "Toggle terminal", exact: true }).click();
  const input = page.locator(".xterm-helper-textarea");
  await expect(input).toBeFocused();
  await expect.poll(() => client.listPtys().then((items) => items.length)).toBe(1);
  // A single tab takes the key and changes nothing.
  await page.keyboard.press("Control+Tab");
  await expect(input).toBeFocused();
  await page.getByRole("button", { name: "New terminal", exact: true }).click();
  await expect.poll(() => client.listPtys().then((items) => items.length)).toBe(2);
  const [one, two] = await client.listPtys().then((items) =>
    items.sort((a, b) => a.id.localeCompare(b.id)),
  );
  await client.updatePty(one.id, { title: "Cycle one" });
  await client.updatePty(two.id, { title: "Cycle two" });
  const current = page.locator('.terminal-tabs [aria-current="true"]');
  await expect(current).toHaveText(/Cycle (one|two)/);
  await expect(input).toBeFocused();
  const shown = (await current.textContent())!.trim();
  const other = shown === "Cycle one" ? "Cycle two" : "Cycle one";
  // The shown terminal records what its shell receives.
  const typed = join(temporary, "cycle-typed.txt");
  await input.fill(`cat -v > ${typed}`);
  await input.press("Enter");
  // Two terminals: forward and back both land on the other one, and the
  // keyboard goes with the step, into the terminal shown.
  await page.keyboard.press("Control+Tab");
  await expect(current).toHaveText(other);
  await expect(input).toBeFocused();
  await page.keyboard.press("Control+Shift+Tab");
  await expect(current).toHaveText(shown);
  await expect(input).toBeFocused();
  await expect(page.locator(".terminal-view .xterm-rows")).toContainText("cycle-typed.txt");
  await page.waitForTimeout(300);
  await input.pressSequentially("done");
  await input.press("Enter");
  await input.press("Control+d");
  await expect.poll(() => readFile(typed, "utf8").catch(() => "")).toBe("done\n");
  await hide.click();
  for (const terminal of await client.listPtys())
    await client.deletePty(terminal.id);

  // Open files in the Files panel.
  await writeFile(join(workspace, "cycle-a.txt"), "cycle a\n");
  await writeFile(join(workspace, "cycle-b.txt"), "cycle b\n");
  await showChat();
  await openProjectPanel("Files");
  for (const name of ["cycle-a.txt", "cycle-b.txt"])
    await page.getByRole("button", { name, exact: true }).first().click();
  const file = page.locator('.file-tabs [aria-current="true"]');
  await expect(file).toHaveText("cycle-b.txt");
  await file.focus();
  await page.keyboard.press("Control+Tab");
  await expect(file).toHaveText("cycle-a.txt");
  // The keyboard is on the tab shown, so the next press keeps stepping.
  await expect(file).toBeFocused();
  await page.keyboard.press("Control+Shift+Tab");
  await expect(file).toHaveText("cycle-b.txt");
  await expect(page.locator(".cm-content")).toContainText("cycle b");
  for (const name of ["cycle-a.txt", "cycle-b.txt"]) {
    await page.getByRole("button", { name: "Close " + name, exact: true }).click();
    await rm(join(workspace, name));
  }

  // Chat tabs from the message box.
  await newSession();
  await send("cycle chat one");
  const transcript = page.locator(".transcript");
  await expect(transcript).toContainText("Echo: cycle chat one");
  await page.getByRole("button", { name: "New chat tab", exact: true }).click();
  await send("cycle chat two");
  await expect(transcript).toContainText("Echo: cycle chat two");
  const message = page.getByRole("textbox", { name: "Message", exact: true });
  await message.focus();
  await page.keyboard.press("Control+Shift+Tab");
  await expect(transcript).toContainText("Echo: cycle chat one");
  await expect(message).toBeFocused();
  await page.keyboard.press("Control+Tab");
  await expect(transcript).toContainText("Echo: cycle chat two");
  await expect(message).toBeFocused();
  // Outside every panel (the sidebar) the key is left alone.
  await page.getByRole("button", { name: "Search", exact: true }).focus();
  await page.keyboard.press("Control+Tab");
  await expect(transcript).toContainText("Echo: cycle chat two");
  expect(errors).toEqual([]);
});

test("the global shortcut is off until turned on, registers only a free combination and keeps the saved one", async () => {
  const registered = (accelerator: string) =>
    app.evaluate(({ globalShortcut }, value) => globalShortcut.isRegistered(value), accelerator);
  // Rare combinations, so the test never holds a key another app uses.
  await page.evaluate(() =>
    window.haarness.preferences({ globalShortcut: { enabled: false, keys: "Ctrl+Alt+Shift+F9" } }),
  );
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  const settings = page.getByRole("dialog", { name: "Settings", exact: true });
  await settings.getByRole("button", { name: "Keyboard shortcuts", exact: true }).click();
  const toggle = settings.getByRole("switch", { name: "Show and hide Haarness from any app", exact: true });
  const field = settings.getByRole("textbox", { name: "Global shortcut", exact: true });
  await expect(toggle).not.toBeChecked();
  await expect(field).toBeDisabled();
  expect(await registered("Control+Alt+Shift+F9")).toBe(false);
  await toggle.click();
  await expect(toggle).toBeChecked();
  await expect.poll(() => registered("Control+Alt+Shift+F9")).toBe(true);
  // Recording another combination moves the registration to it.
  await field.focus();
  await page.keyboard.press("Control+Alt+Shift+F8");
  await expect(field).toHaveValue("Ctrl+Alt+Shift+F8");
  // Held off while the field records, in use again once it is left.
  expect(await registered("Control+Alt+Shift+F8")).toBe(false);
  await field.blur();
  await expect.poll(() => registered("Control+Alt+Shift+F8")).toBe(true);
  expect(await registered("Control+Alt+Shift+F9")).toBe(false);
  // One the system keeps (Spotlight's) is refused, and the field shows the
  // saved one again. Off macOS the recorder takes no Super/Windows chord.
  if (process.platform === "darwin") {
    await field.focus();
    await page.keyboard.press("Meta+Space");
    await expect(settings.getByRole("alert")).toContainText("belongs to the system");
    await settings.getByRole("button", { name: "Dismiss error", exact: true }).click();
    await expect(field).toHaveValue("Ctrl+Alt+Shift+F8");
  }
  // One another app holds is refused too, and the saved one stays in use.
  await app.evaluate(({ globalShortcut }) => globalShortcut.register("Control+Alt+Shift+F7", () => {}));
  await field.focus();
  await page.keyboard.press("Control+Alt+Shift+F7");
  await expect(settings.getByRole("alert")).toContainText("already uses");
  await settings.getByRole("button", { name: "Dismiss error", exact: true }).click();
  await expect(field).toHaveValue("Ctrl+Alt+Shift+F8");
  await field.blur();
  await expect.poll(() => registered("Control+Alt+Shift+F8")).toBe(true);
  await app.evaluate(({ globalShortcut }) => globalShortcut.unregister("Control+Alt+Shift+F7"));
  // The window reloads while the field records: the shortcut is in use
  // again all the same.
  await field.focus();
  await expect.poll(() => registered("Control+Alt+Shift+F8")).toBe(false);
  await page.reload();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 60_000 });
  await expect.poll(() => registered("Control+Alt+Shift+F8")).toBe(true);
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  await settings.getByRole("button", { name: "Keyboard shortcuts", exact: true }).click();
  // Turning it off releases the combination and keeps the choice.
  await toggle.click();
  await expect(toggle).not.toBeChecked();
  await expect.poll(() => registered("Control+Alt+Shift+F8")).toBe(false);
  await expect(field).toHaveValue("Ctrl+Alt+Shift+F8");
  expect(
    await page.evaluate(async () => (await window.haarness.boot()).preferences.globalShortcut),
  ).toEqual({
    enabled: false,
    // Control is Mod off macOS (one key there), and Ctrl of its own on macOS.
    keys: process.platform === "darwin" ? "Ctrl+Alt+Shift+F8" : "Mod+Alt+Shift+F8",
  });
  await page.keyboard.press("Escape");
  await expect(settings).toHaveCount(0);
  expect(errors).toEqual([]);
});

test("a slash command's message shows the line typed, and what it expanded to once it runs, while the chat is open as after reopening", async () => {
  await showChat();
  await newSession();
  await send("/ui-layout-short expand-proof");
  await expect(page.locator(".transcript")).toContainText("Echo:");
  // The server keeps the message as the command expanded it, with the line
  // typed beside it.
  let stored: { text: string; command_line?: string | null } | undefined;
  for (const thread of await client.listThreads()) {
    const found = (await client.listItems(thread.id)).find(
      (item) => item.data.type === "user_message" && item.data.text.includes("Return UI command evidence."),
    );
    if (found?.data.type === "user_message") stored = found.data;
  }
  expect(stored?.command_line).toBe("/ui-layout-short expand-proof");
  // The open chat merges the expansion as it arrives (`item.updated`): the
  // bubble keeps the line typed and gains the expanded prompt behind
  // Show prompt, as a reopened chat shows it.
  const message = page.locator("article.message.user").last();
  await expect(message.locator(".bubble-body")).toHaveText("/ui-layout-short expand-proof");
  await message.getByRole("button", { name: /Show prompt/ }).click();
  await expect(message.locator(".command-prompt-text")).toContainText("Return UI command evidence.");
  expect(errors).toEqual([]);
});

test("conversation tab order survives reopening and uses horizontal keyboard controls", async () => {
  await expandPrimaryProject();
  const first = await client.createThread({
    title: "Tab One",
    provider: "echo",
    model: "echo-1",
  });
  const second = await client.createThread({
    title: "Tab Two",
    provider: "echo",
    model: "echo-1",
  });
  // Double-clicked, each keeps a tab of its own (a plain click previews).
  for (const title of [first.title, second.title])
    await page
      .locator(".sessions")
      .getByRole("button", { name: title, exact: true })
      .dblclick();
  const handle = page.getByRole("tab", {
    name: "Tab Two",
    exact: true,
  });
  await handle.focus();
  await handle.press("Alt+Home");
  await expect(page.locator(".workspace-tabs [role=tab]").first()).toHaveText(
    "Tab Two",
  );
  await expect(handle).toBeFocused();
  await handle.press("Alt+ArrowRight");
  await expect(page.locator(".workspace-tabs [role=tab]").nth(1)).toHaveText(
    "Tab Two",
  );
  // The open tabs come back with the window, in their saved order. A chat an
  // earlier scenario archived while it was on screen stays closed (an agent's
  // own chat, which the server archives when its work ends, does not).
  const archived = new Set(
    (await client.listThreads({ archived: true }))
      .filter((thread) => thread.archived && !thread.isSubagent)
      .map((thread) => thread.id),
  );
  const tabs = page.locator(".workspace-tabs [role=tab]");
  const expected: string[] = [];
  for (const tab of await tabs.all())
    if (!archived.has((await tab.getAttribute("data-cycle-tab")) ?? ""))
      expected.push((await tab.textContent()) ?? "");
  expect(expected).toContain("Tab Two");
  await page.reload();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
  await expect(tabs).toHaveText(expected);
  const disclosure = page.getByRole("button", { name: /^(Expand|Collapse) project workspace$/ });
  if ((await disclosure.getAttribute("aria-expanded")) !== "true") await disclosure.click();
  await expect(disclosure).toHaveAttribute("aria-expanded", "true");
});

test("closing a session tab returns to the previous active conversation", async () => {
  const titles = ["History A", "History B", "History C", "History D"];
  for (const title of titles)
    await client.createThread({ title, provider: "echo", model: "echo-1" });
  // Double-clicked, each keeps a tab of its own (a plain click previews).
  const open = async (title: string) => {
    await page
      .locator(".sessions")
      .getByRole("button", { name: title, exact: true })
      .dblclick();
    await expect(
      page.locator(".session-bar .workspace-tab.active, .session-bar-title"),
    ).toContainText(title);
  };
  const close = async (title: string) => {
    await page
      .locator(".workspace-tab")
      .filter({ has: page.getByRole("tab", { name: title, exact: true }) })
      .getByRole("button", { name: "Close chat tab", exact: true })
      .click();
  };
  for (const title of titles.slice(0, 3)) await open(title);
  await open("History B");
  await open("History D");
  await close("History D");
  await expect(
    page.getByRole("tab", { name: "History B", exact: true }),
  ).toHaveAttribute("aria-selected", "true");
  await close("History C");
  await expect(
    page.getByRole("tab", { name: "History B", exact: true }),
  ).toHaveAttribute("aria-selected", "true");
  await open("History A");
  await open("History C");
  await page
    .getByRole("tab", { name: "History C", exact: true })
    .press("Alt+Home");
  await close("History C");
  await expect(
    page.getByRole("tab", { name: "History A", exact: true }),
  ).toHaveAttribute("aria-selected", "true");
  expect(errors).toEqual([]);
});

test("real workspace: conversations, files, review and terminal", async () => {
  await page.getByRole("button", { name: "New chat", exact: true }).click();
  await selectValue(page.getByRole("combobox", { name: "Model", exact: true }), "echo/echo-1");
  await expect(page.locator("textarea")).toBeVisible();
  await page.locator("textarea").fill("::mdslow");
  await page.getByRole("button", { name: "Send message" }).click();
  await expect(page.locator(".transcript")).toContainText("Live markdown");
  await expect(page.locator(".transcript")).toContainText(
    "streams while working",
  );
  await expect(page.getByRole("button", { name: "Stop response" })).toHaveCount(
    0,
  );
  await openProjectPanel("Files");
  await page.getByRole("button", { name: "src", exact: true }).click();
  await page.getByRole("button", { name: "greeting.ts", exact: true }).click();
  const editor = page.locator(".cm-content");
  await expect(editor).toContainText("hello");
  await page.screenshot({ path: "test-results/files.png" });
  await editor.fill('export const greeting = "from desktop";\n');
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect
    .poll(() => readFile(join(workspace, "src/greeting.ts"), "utf8"))
    .toContain("from desktop");
  await openProjectPanel("Review");
  await expect(page.locator(".review")).toContainText("greeting.ts");
  await expect(page.locator(".review")).toContainText("from desktop");
  await page.screenshot({ path: "test-results/diff.png" });
  await selectValue(
    page.getByRole("combobox", { name: "Diff layout", exact: true }),
    "unified",
  );
  await page
    .getByRole("button", {
      name: "Comment on src/greeting.ts line 1",
      exact: true,
    })
    .last()
    .click();
  const comment = page.getByRole("textbox", {
    name: "Review comment",
    exact: true,
  });
  const form = page.locator(".diff-comment");
  await expect(page.locator("dialog[open]")).toHaveCount(0);
  await expect(comment).toBeFocused();
  await expect(
    form.getByRole("button", { name: "Add to message", exact: true }),
  ).toBeDisabled();
  await comment.press("Escape");
  await expect(form).toHaveCount(0);
  await expect(
    page
      .getByRole("button", {
        name: "Comment on src/greeting.ts line 1",
        exact: true,
      })
      .last(),
  ).toBeFocused();
  await page.locator(".diff-line.add code").dblclick();
  await expect(comment).toBeFocused();
  expect(
    await form.evaluate((element) =>
      element.parentElement?.previousElementSibling?.classList.contains("add"),
    ),
  ).toBe(true);
  await comment.fill("Keep the greeting configurable.");
  await comment.press("Shift+Enter");
  await comment.pressSequentially("Preserve the default.");
  const note = "Keep the greeting configurable.\nPreserve the default.";
  await expect(comment).toHaveValue(note);
  await page.screenshot({ path: "test-results/inline-comment-unified.png" });
  const beforeRefresh = await comment.elementHandle();
  await page
    .getByRole("button", { name: "Refresh changes", exact: true })
    .click();
  // Wait for the refreshed comment's mount/focus before opening another control.
  await expect
    .poll(() => beforeRefresh!.evaluate((element) => element.isConnected))
    .toBe(false);
  await expect(comment).toBeFocused();
  await expect(comment).toHaveValue(note);
  await selectValue(
    page.getByRole("combobox", { name: "Diff layout", exact: true }),
    "split",
  );
  await expect(comment).toHaveValue(note);
  await expect(form).toHaveAttribute("data-side", "updated");
  expect(
    await form.evaluate((element) =>
      element.parentElement?.previousElementSibling?.classList.contains(
        "split-row",
      ),
    ),
  ).toBe(true);
  await page.screenshot({ path: "test-results/inline-comment-split.png" });
  await page
    .getByRole("button", { name: "Comment on original line 1", exact: true })
    .click();
  await expect(comment).toHaveValue("");
  await expect(form).toHaveAttribute("data-side", "original");
  await comment.fill("Explain the original greeting.");
  await form
    .getByRole("button", { name: "Add to message", exact: true })
    .click();
  await expect(form).toHaveCount(0);
  await expect(
    page.getByRole("textbox", { name: "Message", exact: true }),
  ).toHaveValue(
    /Review comment on src\/greeting.ts:1 \(original\):\nExplain the original greeting\./,
  );
  await page
    .getByRole("button", { name: "Comment on updated line 1", exact: true })
    .click();
  await expect(comment).toHaveValue(note);
  await comment.press("Enter");
  await expect(form).toHaveCount(0);
  await expect(
    page.getByRole("textbox", { name: "Message", exact: true }),
  ).toHaveValue(
    /\(original\):\nExplain the original greeting\.[\s\S]*Review comment on src\/greeting.ts:1 \(updated\):\nKeep the greeting configurable\.\nPreserve the default\./,
  );
  await page.getByRole("textbox", { name: "Message", exact: true }).fill("");
  await page
    .getByRole("button", { name: "Toggle terminal", exact: true })
    .click();
  await expect(page.locator(".terminal-panel")).toBeVisible();
  const input = page.locator(".xterm-helper-textarea");
  await expect(input).toBeFocused();
  await input.pressSequentially('printf "terminal-proof\\n"', { delay: 15 });
  await input.press("Enter");
  await expect
    .poll(async () => {
      const tabs = await client.listPtys();
      return tabs.length;
    })
    .toBe(1);
  await page.getByRole("button", { name: "Hide terminal" }).click();
  await page
    .getByRole("button", { name: "Toggle terminal", exact: true })
    .click();
  await expect(page.locator(".xterm-screen")).toBeVisible();
  const terminal = (await client.listPtys())[0];
  const replay = await client.connectPty(
    terminal.id,
    {},
    { signal: AbortSignal.timeout(10000) },
  );
  let output = "";
  for await (const message of replay) {
    if (message instanceof Uint8Array)
      output += new TextDecoder().decode(message);
    if (output.includes("terminal-proof\r\n")) break;
  }
  expect(output).toContain("terminal-proof\r\n");
  await page.screenshot({ path: "test-results/terminal.png" });
  await page.getByRole("button", { name: "Hide terminal" }).click();
  expect(errors).toEqual([]);
});

test("new chats select concrete models, honor configuration and remember available choices across projects and reload", async () => {
  await newSession();
  const model = page.getByRole("combobox", { name: "Model", exact: true });
  const info = await client.info();
  expect(info.defaultProvider).toBe("echo");
  expect(info.defaultModel).toBe("echo-1");
  await expect(model).toHaveAttribute("data-value", "echo/echo-1");
  await expect(page.getByText("Default model", { exact: true })).toHaveCount(0);
  const received: { model: string; effort?: string }[] = [];
  const provider = createServer(async (request, response) => {
    if (request.method === "GET") {
      response.writeHead(200, { "Content-Type": "application/json" });
      response.end(
        JSON.stringify({
          data: ["smart-preferred", "smart-other"].map((id) => ({
            id,
            supported_parameters: ["reasoning_effort"],
          })),
        }),
      );
      return;
    }
    const chunks: Buffer[] = [];
    for await (const chunk of request) chunks.push(Buffer.from(chunk));
    const body = JSON.parse(Buffer.concat(chunks).toString());
    received.push({ model: body.model, effort: body.reasoning_effort });
    response.writeHead(200, { "Content-Type": "text/event-stream" });
    response.end(
      "data: " +
        JSON.stringify({
          choices: [
            {
              index: 0,
              delta: { content: "model-selection-proof" },
              finish_reason: null,
            },
          ],
        }) +
        "\n\ndata: " +
        JSON.stringify({
          choices: [{ index: 0, delta: {}, finish_reason: "stop" }],
        }) +
        "\n\ndata: [DONE]\n\n",
    );
  });
  await new Promise<void>((resolve) =>
    provider.listen(0, "127.0.0.1", resolve),
  );
  const address = provider.address();
  if (!address || typeof address === "string")
    throw Error("Provider fixture did not bind");
  const catalog = `[providers.smart-model]\nname="Smart model fixture"\nprotocol="openai-chat-completions"\nbase_url="http://127.0.0.1:${address.port}/v1"\nrequires_auth=false\ntools=true\nreasoning=true\n`;
  async function open(name: string, config: string) {
    const directory = join(temporary, name);
    await mkdir(join(directory, ".haarness"), { recursive: true });
    await writeFile(join(directory, ".haarness/config.toml"), config);
    await mkdir(join(directory, ".haarness/agents"), { recursive: true });
    await writeFile(
      join(directory, ".haarness/agents/model-specialist.md"),
      "---\ndescription: Model choice fixture\nmode: primary\nmodel: smart-model/smart-other\n---\nUse the selected model.\n",
    );
    extraWorkspaces.push(directory);
    await app.evaluate(({ dialog }, path) => {
      const original = dialog.showOpenDialog;
      dialog.showOpenDialog = async () => {
        dialog.showOpenDialog = original;
        return { canceled: false, filePaths: [path] };
      };
    }, directory);
    await page
      .getByRole("button", { name: "Open another project", exact: true })
      .click();
    await expect(page.locator(".application")).toHaveAttribute(
      "data-project-ready",
      "true",
    );
    await newSession();
    return "local:" + directory;
  }
  try {
    await open(
      "configured-model",
      '[defaults]\nprovider="smart-model"\nmodel="smart-preferred"\n' + catalog,
    );
    await expect(model).toHaveAttribute(
      "data-value",
      "smart-model/smart-preferred",
    );
    await expect(page.getByRole("combobox", { name: "Reasoning effort", exact: true })).toHaveCount(0);
    await model.click();
    await expect(page.getByRole("slider", { name: "Reasoning effort", exact: true })).toBeVisible();
    await page.keyboard.press("Escape");
    await selectValue(model, "smart-model/smart-preferred");
    await selectValue(
      page.getByRole("combobox", { name: "Agent", exact: true }),
      "model-specialist",
    );
    await expect(model).toHaveAttribute(
      "data-value",
      "smart-model/smart-other",
    );
    await selectValue(
      page.getByRole("combobox", { name: "Agent", exact: true }),
      "build",
    );
    await expect(model).toHaveAttribute(
      "data-value",
      "smart-model/smart-preferred",
    );
    await selectValue(model, "smart-model/smart-other");
    // The agent shown, picked again, is not a change: the new chat keeps the
    // model chosen for it rather than going back to the configured default.
    await selectValue(
      page.getByRole("combobox", { name: "Agent", exact: true }),
      "build",
    );
    await expect(model).toHaveAttribute(
      "data-value",
      "smart-model/smart-other",
    );
    const connection = await open("remembered-model", catalog);
    await expect(model).toHaveAttribute(
      "data-value",
      "smart-model/smart-other",
    );
    await page.reload();
    await expect(page.locator(".application")).toHaveAttribute(
      "data-project-ready",
      "true",
    );
    await expect(model).toHaveAttribute(
      "data-value",
      "smart-model/smart-other",
    );
    await selectEffort(
      page.getByRole("combobox", { name: "Model", exact: true }),
      "high",
    );
    await send("Use the model shown before the first message");
    await expect(page.locator(".transcript")).toContainText(
      "model-selection-proof",
    );
    const threads = await page.evaluate(
      async (connection) =>
        window.haarness.api<{ provider: string; model: string }[]>(
          connection,
          "listThreads",
          [],
        ),
      connection,
    );
    expect(threads[0]).toMatchObject({
      provider: "smart-model",
      model: "smart-other",
    });
    // In a chat, the agent and model shown, picked again, write nothing: the
    // chat was rewritten and its sidebar age said "now".
    const updated = () =>
      page.evaluate(
        async (connection) =>
          (
            await window.haarness.api<{ updatedAt: string }[]>(
              connection,
              "listThreads",
              [],
            )
          )[0].updatedAt,
        connection,
      );
    let settled = "";
    await expect
      .poll(async () => {
        const before = settled;
        settled = await updated();
        return before === settled;
      }, { intervals: [500] })
      .toBe(true);
    await selectValue(model, "smart-model/smart-other");
    await selectValue(
      page.getByRole("combobox", { name: "Agent", exact: true }),
      "build",
    );
    await page.waitForTimeout(500);
    expect(await updated()).toBe(settled);
    expect(received.length).toBeGreaterThan(0);
    expect(received.every((request) => request.model === "smart-other")).toBe(
      true,
    );
    expect(received.some((request) => request.effort === "high")).toBe(true);
    await newSession();
    await expect(model).toHaveAttribute(
      "data-value",
      "smart-model/smart-other",
    );
    await page
      .locator(".composer")
      .screenshot({ path: "test-results/concrete-model-composer.png" });
    await page
      .getByRole("region", { name: "Project workspace", exact: true })
      .getByRole("button", { name: "workspace", exact: true })
      .click();
    await expect(page.locator(".application")).toHaveAttribute(
      "data-project-ready",
      "true",
    );
    await newSession();
    await expect(model).toHaveAttribute("data-value", "echo/echo-1");
    expect(errors).toEqual([]);
  } finally {
    provider.closeAllConnections();
    await new Promise<void>((resolve, reject) =>
      provider.close((error) => (error ? reject(error) : resolve())),
    );
  }
});

test("connected models refresh live, preserve metadata, and guide provider setup", async () => {
  test.setTimeout(120000);
  let models: {
    id: string;
    display_name?: string;
    context_length: number;
    max_output_tokens: number;
    supported_parameters: string[];
  }[] = [
    {
      id: "gpt-6-astra",
      context_length: 222222,
      max_output_tokens: 12345,
      supported_parameters: ["tools", "reasoning_effort"],
    },
  ];
  let unavailable = false;
  let failedLists = 0;
  const requests: string[] = [];
  const service = createServer(async (request, response) => {
    if (request.method === "GET") {
      if (unavailable) failedLists++;
      response.writeHead(unavailable ? 503 : 200, {
        "Content-Type": "application/json",
      });
      response.end(
        JSON.stringify(
          unavailable ? { error: "temporarily unavailable" } : { data: models },
        ),
      );
      return;
    }
    const chunks: Buffer[] = [];
    for await (const chunk of request) chunks.push(Buffer.from(chunk));
    requests.push(JSON.parse(Buffer.concat(chunks).toString()).model);
    response.writeHead(200, { "Content-Type": "text/event-stream" });
    response.end(
      'data: {"choices":[{"index":0,"delta":{"content":"live-model-proof"},"finish_reason":null}]}\n\ndata: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}\n\ndata: [DONE]\n\n',
    );
  });
  await new Promise<void>((resolve) => service.listen(0, "127.0.0.1", resolve));
  const address = service.address();
  if (!address || typeof address === "string")
    throw Error("Missing fixture address");
  const directory = join(temporary, "live-model-catalog");
  extraWorkspaces.push(directory);
  await mkdir(join(directory, ".haarness"), { recursive: true });
  const disabled = [
    ...new Set([
      ...(await client.providers()).map((p) => p.id),
      "ollama",
      "lmstudio",
      "echo",
    ]),
  ];
  await writeFile(
    join(directory, ".haarness/config.toml"),
    disabled.map((id) => `[providers.${id}]\nenabled=false\n`).join("") +
      `[defaults]\nprovider="live-catalog"\nmodel="deleted-model"\n[providers.live-catalog]\nname="Live catalog fixture"\nprotocol="openai-chat-completions"\nbase_url="http://127.0.0.1:${address.port}/v1"\napi_key_env="HAARNESS_TEST_MISSING_KEY"\ntools=true\nreasoning=true\n`,
  );
  const connection = "local:" + directory;
  const model = page.getByRole("combobox", { name: "Model", exact: true });
  const refresh = () =>
    page.evaluate(
      async (id) =>
        window.haarness.api<import("@haarness/sdk").ProviderInfo[]>(
          id,
          "refreshProviders",
          [],
        ),
      connection,
    );
  const setup = page.getByRole("dialog", { name: "Settings", exact: true });
  try {
    await app.evaluate(({ dialog }, path) => {
      const original = dialog.showOpenDialog;
      dialog.showOpenDialog = async () => {
        dialog.showOpenDialog = original;
        return { canceled: false, filePaths: [path] };
      };
    }, directory);
    await page
      .getByRole("button", { name: "Open another project", exact: true })
      .click();
    // First run: nothing can answer a prompt, so the welcome dialog is the
    // onboarding. It lists the fixture provider and can be dismissed.
    const welcome = page.getByRole("dialog", {
      name: "Welcome to Haarness",
      exact: true,
    });
    await expect(welcome).toBeVisible();
    await expect(
      welcome.getByRole("button", { name: "Live catalog fixture", exact: true }),
    ).toBeVisible();
    await expect(model).toHaveCount(0);
    await welcome
      .getByRole("button", { name: "Close dialog", exact: true })
      .click();
    await expect(welcome).toHaveCount(0);
    await expect(
      page.getByRole("button", { name: "Connect a provider", exact: true }),
    ).toBeVisible();
    await page
      .getByRole("textbox", { name: "Message", exact: true })
      .fill("Keep this draft while connecting");
    await page
      .getByRole("button", { name: "Send message", exact: true })
      .click();
    await expect(welcome).toBeVisible();
    await welcome
      .getByRole("searchbox", { name: "Search providers", exact: true })
      .fill("live catalog");
    await expect(welcome.locator(".connect-row[data-provider]")).toHaveCount(1);
    await welcome
      .getByRole("button", { name: "Live catalog fixture", exact: true })
      .click();
    const key = page.getByRole("dialog", {
      name: "Connect Live catalog fixture",
      exact: true,
    });
    await expect(key.getByLabel("API key", { exact: true })).toHaveAttribute(
      "type",
      "password",
    );
    await key
      .getByLabel("API key", { exact: true })
      .fill("local-test-credential");
    await key.getByRole("button", { name: "Connect", exact: true }).click();
    await expect(page.locator(".toast")).toContainText(
      "Live catalog fixture is connected",
    );
    await expect(key).toHaveCount(0);
    expect(await page.locator("body").innerText()).not.toContain(
      "local-test-credential",
    );
    await expect(model).toHaveAttribute(
      "data-value",
      "live-catalog/gpt-6-astra",
    );
    await expect(model).toHaveText("GPT-6 Astra");
    await expect(
      page.getByRole("textbox", { name: "Message", exact: true }),
    ).toHaveValue("Keep this draft while connecting");
    await model.click();
    await expect(
      page
        .getByRole("listbox", { name: "Model", exact: true })
        .getByRole("option"),
    ).toHaveCount(1);
    await page.keyboard.press("Escape");
    const before = (await refresh()).find((p) => p.id === "live-catalog")!;
    expect(before.models[0]).toMatchObject({
      contextWindow: 222222,
      maxOutputTokens: 12345,
    });
    models = [
      {
        id: "new-model-2026",
        display_name: "Future Reasoner",
        context_length: 333333,
        max_output_tokens: 23456,
        supported_parameters: ["tools", "reasoning_effort"],
      },
    ];
    await expect
      .poll(
        async () =>
          (await refresh()).find((p) => p.id === "live-catalog")?.models[0]?.id,
      )
      .toBe("new-model-2026");
    await model.click();
    await expect(
      page.getByRole("option", { name: "Future Reasoner", exact: true }),
    ).toBeVisible();
    await expect(
      page.getByRole("option", { name: "GPT-6 Astra", exact: true }),
    ).toHaveCount(0);
    await page.keyboard.press("Enter");
    await expect(model).toHaveAttribute(
      "data-value",
      "live-catalog/new-model-2026",
    );
    // Continue an old conversation whose saved provider no longer exists.
    const thread = await page.evaluate(
      async (id) =>
        window.haarness.api<import("@haarness/sdk").Thread>(
          id,
          "createThread",
          [
            {
              title: "Offline saved model",
              provider: "disconnected",
              model: "deleted",
            },
          ],
        ),
      connection,
    );
    await page
      .getByRole("region", { name: "Project live-model-catalog", exact: true })
      .getByRole("button", { name: "Offline saved model", exact: true })
      .click();
    await expect(model).toHaveText("Future Reasoner");
    await send("Use the connected fallback");
    await expect(page.locator(".transcript")).toContainText("live-model-proof");
    expect(requests).toContain("new-model-2026");
    const saved = await page.evaluate(
      async ({ id, thread }) =>
        window.haarness.api<import("@haarness/sdk").Thread>(id, "getThread", [
          thread,
        ]),
      { id: connection, thread: thread.id },
    );
    expect(saved).toMatchObject({
      provider: "live-catalog",
      model: "new-model-2026",
    });
    await page.screenshot({ path: "test-results/live-model-catalog.png" });
    unavailable = true;
    await expect
      .poll(async () => {
        await refresh();
        return failedLists;
      })
      .toBeGreaterThan(0);
    expect(
      (await refresh()).find((p) => p.id === "live-catalog")?.models[0]?.id,
    ).toBe("new-model-2026");
    unavailable = false;
    models = [];
    await expect
      .poll(
        async () =>
          (await refresh()).find((p) => p.id === "live-catalog")?.models.length,
      )
      .toBe(0);
    // Reload fetches the authoritative empty catalog and welcomes again.
    await page.reload();
    await expect(welcome).toBeVisible();
    await expect(model).toHaveCount(0);
    await welcome
      .getByRole("button", { name: "Close dialog", exact: true })
      .click();
    await page.screenshot({ path: "test-results/empty-model-catalog.png" });
    expect(errors).toEqual([]);
  } finally {
    service.closeAllConnections();
    await new Promise<void>((resolve) => service.close(() => resolve()));
    if (await setup.isVisible())
      await setup
        .getByRole("button", { name: "Close settings", exact: true })
        .click();
    const project = page.getByRole("region", {
      name: "Project workspace",
      exact: true,
    });
    await project
      .getByRole("button", { name: "workspace", exact: true })
      .click();
    await expect(page.locator(".application")).toHaveAttribute(
      "data-project-ready",
      "true",
    );
    await newSession();
  }
});

test("signing in with a code connects OpenAI end to end and can be disconnected", async () => {
  test.setTimeout(120000);
  issuerPolls = 0;
  // The dialog hands the sign-in URL to the system browser; never open one here.
  await app.evaluate(({ shell }) => {
    shell.openExternal = async () => {};
  });
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  const settings = page.getByRole("dialog", { name: "Settings", exact: true });
  await settings.getByRole("button", { name: "Providers", exact: true }).click();
  const block = settings.locator(".setting-block").filter({
    has: page.getByRole("heading", { name: "OpenAI", exact: true }),
  });
  // Providers lists what is connected; the catalog is in Connect a provider.
  await expect(block).toHaveCount(0);
  await settings
    .getByRole("button", { name: "Connect a provider", exact: true })
    .click();
  const picker = page.getByRole("dialog", {
    name: "Connect a provider",
    exact: true,
  });
  await expect(picker).toBeVisible();
  await expect(picker.locator(".connect-row").first()).toHaveAttribute(
    "data-provider",
    "openai",
  );
  await picker.getByRole("button", { name: "OpenAI", exact: true }).click();
  const method = page.getByRole("dialog", { name: "Connect OpenAI", exact: true });
  await expect(
    method.getByRole("button", { name: /^Sign in with ChatGPT/ }),
  ).toBeVisible();
  await expect(method.getByRole("button", { name: /^Use an API key/ })).toBeVisible();
  await method.getByRole("button", { name: /^Sign in with a code/ }).click();
  const login = page.getByRole("dialog", { name: "Sign in to OpenAI", exact: true });
  await expect(login).toContainText("ABCD-EFGH");
  await expect(
    login.getByRole("button", { name: "Open sign-in page", exact: true }),
  ).toBeVisible();
  await expect(login).toContainText("Waiting for you to finish in the browser");
  await expect(page.locator(".toast")).toContainText("OpenAI is connected", {
    timeout: 30000,
  });
  await expect(login).toHaveCount(0);
  await expect(block).toContainText("Connected");
  expect(issuerPolls).toBeGreaterThanOrEqual(2);
  expect(
    (await client.providers()).find((provider) => provider.id === "openai")
      ?.authenticated,
  ).toBe(true);
  expect(await settings.innerText()).not.toContain("fixture-access");
  await block.getByRole("button", { name: "Disconnect", exact: true }).click();
  await page
    .getByRole("dialog", { name: "Disconnect OpenAI?", exact: true })
    .getByRole("button", { name: "Disconnect", exact: true })
    .click();
  await expect(block).toHaveCount(0);
  // The keyboard moves on to another provider's row, or to Connect a provider.
  await expect
    .poll(() => page.evaluate(() => !!document.activeElement?.closest(".provider-row, .providers-hero")))
    .toBe(true);
  expect(
    (await client.providers()).find((provider) => provider.id === "openai")
      ?.authenticated,
  ).toBe(false);
  await settings
    .getByRole("button", { name: "Close settings", exact: true })
    .click();
  expect(errors).toEqual([]);
});

test("a key that could not be checked is saved and the connect dialog says so", async () => {
  test.setTimeout(90000);
  // The fixture provider's address (port 1) answers nothing, so the server
  // keeps the key without being able to ask about it.
  const provider = "desktop-missing-key";
  const connected = async () =>
    (await client.providers()).find((row) => row.id === provider)?.authenticated;
  expect(await connected()).toBe(false);
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  const settings = page.getByRole("dialog", { name: "Settings", exact: true });
  await settings.getByRole("button", { name: "Providers", exact: true }).click();
  try {
    await settings
      .getByRole("button", { name: "Connect a provider", exact: true })
      .click();
    const picker = page.getByRole("dialog", {
      name: "Connect a provider",
      exact: true,
    });
    await picker
      .getByRole("searchbox", { name: "Search providers", exact: true })
      .fill("credential fixture");
    await picker
      .getByRole("button", { name: "Desktop credential fixture", exact: true })
      .click();
    const dialog = page.getByRole("dialog", {
      name: "Connect Desktop credential fixture",
      exact: true,
    });
    const key = dialog.getByLabel("API key", { exact: true });
    await key.fill("unchecked-fixture-key");
    await dialog.getByRole("button", { name: "Connect", exact: true }).click();
    const warning = dialog.getByRole("alert");
    await expect(warning).toContainText("Saved, but the key could not be checked");
    await expect(warning).toContainText(
      "Haarness could not reach Desktop credential fixture. If chats then fail to authenticate, replace the key.",
    );
    // Saved, not announced as working.
    await expect(
      page.locator(".toast").filter({ hasText: "is connected" }),
    ).toHaveCount(0);
    await expect(dialog.getByRole("button", { name: "Done", exact: true })).toBeFocused();
    await expect.poll(connected).toBe(true);
    await dialog.screenshot({
      path: test.info().outputPath("connect-key-unchecked.png"),
    });
    // Replace key goes back to an empty field; a new key is checked again.
    await dialog.getByRole("button", { name: "Replace key", exact: true }).click();
    await expect(key).toHaveValue("");
    await expect(key).toBeFocused();
    await key.fill("unchecked-fixture-key-2");
    await key.press("Enter");
    await expect(warning).toContainText("Saved, but the key could not be checked");
    expect(await dialog.innerText()).not.toContain("unchecked-fixture-key");
    await dialog.getByRole("button", { name: "Done", exact: true }).click();
    await expect(dialog).toHaveCount(0);
    await expect(settings).toBeVisible();
  } finally {
    // Later tests rely on this provider having no key.
    await page.evaluate(
      async ({ id, name }) =>
        window.haarness.api(id, "manageResource", [
          "providers",
          { action: "logout", name },
        ]),
      { id: "local:" + workspace, name: provider },
    );
    await expect.poll(connected).toBe(false);
    if (await settings.isVisible())
      await settings
        .getByRole("button", { name: "Close settings", exact: true })
        .click();
  }
  expect(errors).toEqual([]);
});

test("Auto approvals allow once, inherit to agents, preserve Review and survive Desktop reload", async () => {
  test.setTimeout(120000);
  const parent = await client.createThread({
    title: "Auto approval verification",
    provider: "echo",
    model: "echo-1",
  });
  const open = async (title: string) => {
    await page.getByRole("button", { name: "Search", exact: true }).click();
    const search = page.getByRole("combobox", { name: "Search Haarness" });
    await search.fill(title);
    await page.getByRole("option").filter({ hasText: title }).first().click();
  };
  const start = (id: string, text: string) =>
    client.startTurn(id, { text, requestId: crypto.randomUUID() });
  const finished = () =>
    expect
      .poll(() => client.activeTurns().then((items) => items.length))
      .toBe(0);
  // The pill is named after the mode it shows ("Approvals: Review").
  const toggle = page.getByRole("button", { name: /^Approvals: (Review|Auto)$/ });
  await open(parent.title);
  await expect(toggle).toHaveAccessibleName("Approvals: Review");
  const command =
    '!tool bash {"command":"printf auto-once-proof > auto-once.txt"}';
  await start(parent.id, command);
  // Allow once + Auto waits in the Always menu, so the card's answers fit
  // one line: Deny, Always allow, Allow once.
  await page.getByRole("button", { name: "Always allow", exact: true }).click();
  await page
    .getByRole("menuitem", { name: "Allow once + Auto", exact: true })
    .click();
  await finished();
  expect(await readFile(join(workspace, "auto-once.txt"), "utf8")).toBe(
    "auto-once-proof",
  );
  await expect(toggle).toHaveAccessibleName("Approvals: Auto");
  await start(parent.id, command);
  await finished();
  expect(await client.listPermissions()).toHaveLength(0);
  await expect(page.getByText(/Auto approval failed/)).toHaveCount(0);
  await page.getByRole("textbox", { name: "Message", exact: true }).focus();
  await page
    .locator(".composer")
    .screenshot({ path: "test-results/composer-auto.png" });
  // The exact same command must prompt again after Review: Auto never learns an Always rule.
  await toggle.click();
  await start(parent.id, command);
  await expect(
    page.getByRole("button", { name: "Allow once", exact: true }),
  ).toBeVisible();
  await page.getByRole("button", { name: "Deny", exact: true }).click();
  await finished();
  await toggle.click();
  await page.reload();
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
  );
  await expect(toggle).toHaveAccessibleName("Approvals: Auto");
  await start(parent.id, command);
  await finished();
  const other = await client.createThread({
    title: "Manual approval verification",
    provider: "echo",
    model: "echo-1",
  });
  await open(other.title);
  await expect(toggle).toHaveAccessibleName("Approvals: Review");
  // A background root and its child still use the root's Auto setting while another conversation is selected.
  await start(
    parent.id,
    "!tool task " +
      JSON.stringify({
        description: "Automatic child permission",
        agent: "general",
        prompt:
          '!tool bash {"command":"printf child-auto-proof > child-auto.txt"}',
      }),
  );
  await finished();
  expect(await readFile(join(workspace, "child-auto.txt"), "utf8")).toBe(
    "child-auto-proof",
  );
  await start(other.id, command);
  await expect(
    page.getByRole("button", { name: "Allow once", exact: true }),
  ).toBeVisible();
  await page.getByRole("button", { name: "Deny", exact: true }).click();
  await finished();
  await start(
    parent.id,
    '!tool question {"prompt":"Auto still asks the user","choices":["Continue"]}',
  );
  await expect
    .poll(() => client.listQuestions().then((items) => items.length))
    .toBe(1);
  const [question] = await client.listQuestions();
  await client.replyQuestion(question.questionId, { answer: "Continue" });
  await finished();
  // Plan's write ceiling remains authoritative even with Auto enabled.
  await client.updateThread(parent.id, { agent: "plan" });
  await start(
    parent.id,
    '!tool write {"path":"must-not-exist.txt","content":"denied"}',
  );
  await finished();
  expect(
    await readFile(join(workspace, "must-not-exist.txt"), "utf8").catch(
      () => null,
    ),
  ).toBeNull();
  await client.updateThread(parent.id, { agent: "build" });
  await open(parent.title);
  await expect(toggle).toHaveAccessibleName("Approvals: Auto");
  await page.getByRole("button", { name: "Search", exact: true }).click();
  await page
    .getByRole("combobox", { name: "Search Haarness" })
    .fill("Switch approvals to Review");
  await page
    .getByRole("option")
    .filter({ hasText: "Switch approvals to Review" })
    .click();
  await expect(toggle).toHaveAccessibleName("Approvals: Review");
  expect(errors).toEqual([]);
});

test("Auto's reviewer notes what it leaves to you, blocks with a reason and says Reviewing… only when slow", async () => {
  test.setTimeout(120000);
  // The test provider's reviewer answers by the marker after the call.
  const thread = await client.createThread({
    title: "Auto review verification",
    provider: "echo",
    model: "echo-1",
  });
  await page.getByRole("button", { name: "Search", exact: true }).click();
  await page.getByRole("combobox", { name: "Search Haarness" }).fill(thread.title);
  await page.getByRole("option").filter({ hasText: thread.title }).first().click();
  await expect(page.locator(".session-bar-name, .workspace-tab.active")).toContainText(thread.title);
  await client.setApprovalMode(thread.id, { mode: "auto" });
  const toggle = page.getByRole("button", { name: "Approvals: Auto", exact: true });
  await expect(toggle).toBeVisible();
  await expect(toggle).toHaveAttribute("title", /a reviewer model approves routine permission requests/);
  const start = (command: string, verdict: string) =>
    client.startTurn(thread.id, {
      requestId: crypto.randomUUID(),
      text: `!tool bash ${JSON.stringify({ command })} [review:${verdict}]`,
    });
  const shots = async (name: string, target: Locator) => {
    for (const theme of ["light", "dark"] as const) {
      await page.evaluate((value) => window.haarness.preferences({ theme: value }), theme);
      await expect(page.locator("html")).toHaveAttribute("data-theme", theme);
      await target.screenshot({ path: test.info().outputPath(`${name}-${theme}.png`) });
    }
    await page.evaluate(() => window.haarness.preferences({ theme: "light" }));
  };

  // Left to the person: the ordinary card, with the reviewer's note in it from
  // its first paint.
  await start("printf asked > review-asked.txt", "ask");
  const card = page.getByRole("alert", { name: "Permission request" });
  await expect(card).toBeVisible();
  const note = card.locator(".approval-review");
  await expect(note).toHaveText("Auto review · Test rule: Test verdict: ask.");
  // One quiet line under the command, never a second card or a toast.
  await expect(page.locator(".toast")).toHaveCount(0);
  await shots("auto-review-card", card);
  await card.getByRole("button", { name: "Deny", exact: true }).click();
  await settled();
  expect(await readFile(join(workspace, "review-asked.txt"), "utf8").catch(() => null)).toBeNull();

  // On a command that runs a script, the card has both: the note under the
  // command and the script's toggle under the note, there from its first
  // paint, so nothing moves as it settles.
  await writeFile(join(workspace, "review-script.sh"), "printf scripted > review-script.txt\n");
  await start("sh review-script.sh", "ask");
  await expect(card).toBeVisible();
  const scriptToggle = card.locator(".approval-script-toggle");
  await expect(note).toHaveText("Auto review · Test rule: Test verdict: ask.");
  await expect(scriptToggle).toBeVisible();
  // The gaps between the note, the toggle and the answers: the card itself
  // may still be easing in.
  const layout = async () => {
    const top = async (target: Locator) => (await target.boundingBox())!.y;
    const [noteTop, toggleTop, denyTop] = [
      await top(note),
      await top(scriptToggle),
      await top(card.getByRole("button", { name: "Deny", exact: true })),
    ];
    return { toggle: Math.round(toggleTop - noteTop), deny: Math.round(denyTop - toggleTop) };
  };
  const first = await layout();
  expect(first.toggle).toBeGreaterThan(0);
  expect(first.deny).toBeGreaterThan(0);
  await page.evaluate(async () => {
    await Promise.all(
      document
        .getAnimations()
        .filter((item) => item.effect?.getComputedTiming().iterations !== Infinity)
        .map((item) => item.finished.catch(() => undefined)),
    );
    await new Promise(requestAnimationFrame);
  });
  // Within a pixel: the card's own ease-in scales what it holds.
  const settledLayout = await layout();
  expect(Math.abs(settledLayout.toggle - first.toggle)).toBeLessThanOrEqual(1);
  expect(Math.abs(settledLayout.deny - first.deny)).toBeLessThanOrEqual(1);
  await shots("auto-review-script", card);
  await card.getByRole("button", { name: "Deny", exact: true }).click();
  await settled();
  expect(await readFile(join(workspace, "review-script.txt"), "utf8").catch(() => null)).toBeNull();

  // Blocked: the row says so in the quiet style of a refused step, and its
  // detail is the reason, not the text written for the model.
  await start("printf blocked > review-blocked.txt", "deny");
  await settled();
  expect(await client.listPermissions()).toHaveLength(0);
  const row = page.locator(".tool-call").filter({ hasText: "Blocked by Auto review" }).last();
  await expect(row).toBeVisible();
  await expect(row).toHaveAttribute("data-status", "failed");
  await row.locator("summary").click();
  await expect(row.locator(".tool-output")).toHaveText("Test verdict: deny.");
  await shots("auto-review-blocked", row);
  await row.locator("summary").click();
  expect(await readFile(join(workspace, "review-blocked.txt"), "utf8").catch(() => null)).toBeNull();

  // A slow review: the activity line says so after a moment; a quick one
  // (above) never did.
  await start("printf slow > review-slow.txt", "slow");
  const activity = page.locator(".assistant-activity");
  await expect(activity).toContainText("Reviewing…", { timeout: 5000 });
  await shots("auto-review-reviewing", activity);
  await settled();
  expect(await readFile(join(workspace, "review-slow.txt"), "utf8")).toBe("slow");
  await client.setApprovalMode(thread.id, { mode: "review" });
  expect(errors).toEqual([]);
});

test("Auto's review cues: never for quick reviews, at once on a switch, and on a card Auto finds waiting", async () => {
  test.setTimeout(150000);
  const thread = await client.createThread({
    title: "Auto review cues",
    provider: "echo",
    model: "echo-1",
  });
  await page.getByRole("button", { name: "Search", exact: true }).click();
  await page.getByRole("combobox", { name: "Search Haarness" }).fill(thread.title);
  await page.getByRole("option").filter({ hasText: thread.title }).first().click();
  await expect(page.locator(".session-bar-name, .workspace-tab.active")).toContainText(thread.title);
  await client.setApprovalMode(thread.id, { mode: "auto" });
  const auto = page.getByRole("button", { name: "Approvals: Auto", exact: true });
  const review = page.getByRole("button", { name: "Approvals: Review", exact: true });
  await expect(auto).toBeVisible();
  const start = (command: string, verdict: string) =>
    client.startTurn(thread.id, {
      requestId: crypto.randomUUID(),
      text: `!tool bash ${JSON.stringify({ command })} [review:${verdict}]`,
    });
  const card = page.getByRole("alert", { name: "Permission request" });
  const activity = page.locator(".assistant-activity");
  const missing = (name: string) =>
    readFile(join(workspace, name), "utf8").catch(() => null);

  // Quick reviews never say so: an observer watches every change.
  await page.evaluate(() => {
    const seen = window as unknown as { sawReviewing?: boolean; reviewWatch?: MutationObserver };
    seen.sawReviewing = false;
    seen.reviewWatch = new MutationObserver(() => {
      if (document.querySelector(".assistant-activity")?.textContent?.includes("Reviewing…"))
        seen.sawReviewing = true;
    });
    seen.reviewWatch.observe(document.body, { subtree: true, childList: true, characterData: true });
  });
  await start("printf quick > review-quick.txt", "allow");
  await settled();
  await start("printf quick > review-quick-deny.txt", "deny");
  await settled();
  const saw = await page.evaluate(() => {
    const seen = window as unknown as { sawReviewing?: boolean; reviewWatch?: MutationObserver };
    seen.reviewWatch?.disconnect();
    return seen.sawReviewing;
  });
  expect(saw).toBe(false);
  expect(await readFile(join(workspace, "review-quick.txt"), "utf8")).toBe("quick");
  expect(await missing("review-quick-deny.txt")).toBeNull();

  // A slow review that then asks: Reviewing…, then the card with its note.
  await start("printf slow > review-slow-ask.txt", "slow-ask");
  await expect(activity).toContainText("Reviewing…", { timeout: 5000 });
  await expect(card.locator(".approval-review")).toHaveText(
    "Auto review · Test rule: Test verdict: ask.",
    { timeout: 10000 },
  );
  await card.getByRole("button", { name: "Deny", exact: true }).click();
  await settled();

  // A switch to Review mid-review shows the plain card at once.
  await start("printf switched > review-switched.txt", "stall");
  await expect(activity).toContainText("Reviewing…", { timeout: 5000 });
  await auto.click();
  await expect(review).toBeVisible();
  await expect(card).toBeVisible({ timeout: 3000 });
  await expect(card.locator(".approval-review")).toHaveCount(0);
  await card.getByRole("button", { name: "Deny", exact: true }).click();
  await settled();
  expect(await missing("review-switched.txt")).toBeNull();

  // A card shown in Review when Auto turns on says the reviewer is deciding
  // it, in place; back in Review it is plain again.
  await start("printf waiting > review-waiting.txt", "stall");
  await expect(card).toBeVisible();
  await expect(card.locator(".approval-review")).toHaveCount(0);
  // Where the card rests once it has come in: its answers stay put as the
  // note appears above them.
  // Measured from the composer, whose own controls change with the mode.
  const bottom = async () => {
    const box = await card.getByRole("button", { name: "Deny", exact: true }).boundingBox();
    const composer = await page.locator(".composer").boundingBox();
    return Math.round((composer?.y ?? 0) - (box?.y ?? 0));
  };
  // The card eases in; two close samples can round alike while it is still
  // moving, so wait for every finite animation to end instead.
  const still = () =>
    page.evaluate(async () => {
      await Promise.all(
        document
          .getAnimations()
          .filter((item) => item.effect?.getComputedTiming().iterations !== Infinity)
          .map((item) => item.finished.catch(() => undefined)),
      );
      await new Promise(requestAnimationFrame);
    });
  await still();
  const rest = await bottom();
  // Its answers keep their places as Allow once + Auto leaves the Always
  // menu, and the keyboard stays on Allow once; the card is the same
  // element, updated.
  const deny = card.getByRole("button", { name: "Deny", exact: true });
  const always = card.getByRole("button", { name: "Always allow", exact: true });
  const once = card.getByRole("button", { name: "Allow once", exact: true });
  const across = async () =>
    Promise.all([deny, always, once].map(async (button) => Math.round((await button.boundingBox())!.x)));
  const places = await across();
  await card.screenshot({ path: test.info().outputPath("auto-review-review-card-light.png") });
  await card.evaluate((node) => {
    node.dataset.probe = "kept";
  });
  await always.click();
  await expect(page.getByRole("menuitem", { name: "Allow once + Auto", exact: true })).toBeVisible();
  await page.keyboard.press("Escape");
  await once.focus();
  await client.setApprovalMode(thread.id, { mode: "auto" });
  await expect(auto).toBeVisible();
  await expect(card.locator(".approval-review")).toHaveText("Auto review is deciding…");
  await still();
  expect(Math.abs((await bottom()) - rest)).toBeLessThan(2);
  expect(await across()).toEqual(places);
  await expect(once).toBeFocused();
  await always.click();
  await expect(page.getByRole("menuitem", { name: "Allow once + Auto", exact: true })).toHaveCount(0);
  await page.keyboard.press("Escape");
  await expect(card).toHaveAttribute("data-probe", "kept");
  await card.screenshot({ path: test.info().outputPath("auto-review-deciding-light.png") });
  await page.evaluate(() => window.haarness.preferences({ theme: "dark" }));
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await card.screenshot({ path: test.info().outputPath("auto-review-deciding-dark.png") });
  await page.evaluate(() => window.haarness.preferences({ theme: "light" }));
  await auto.click();
  await expect(review).toBeVisible();
  await expect(card.locator(".approval-review")).toHaveCount(0);
  await card.getByRole("button", { name: "Deny", exact: true }).click();
  await settled();
  expect(await missing("review-waiting.txt")).toBeNull();

  await client.setApprovalMode(thread.id, { mode: "review" });
  expect(errors).toEqual([]);
});

test("Auto from a child view leaves its parent and siblings in Review", async () => {
  const parent = await client.createThread({
    title: "Selective Auto parent",
    provider: "echo",
    model: "echo-1",
  });
  await page.getByRole("button", { name: "Search", exact: true }).click();
  await page
    .getByRole("combobox", { name: "Search Haarness" })
    .fill(parent.title);
  await page
    .getByRole("option")
    .filter({ hasText: parent.title })
    .first()
    .click();
  await expect(
    page.locator(".session-bar-name, .workspace-tab.active"),
  ).toContainText(parent.title);
  await expect(
    page.getByRole("button", { name: "Approvals: Review", exact: true }),
  ).toBeVisible();
  await client.startTurn(parent.id, {
    requestId: crypto.randomUUID(),
    text:
      "!multi " +
      JSON.stringify([
        {
          name: "task",
          arguments: {
            description: "Selective Auto child",
            agent: "general",
            background: true,
            prompt:
              '!tool bash {"command":"printf selective-child-proof > selective-child.txt"}',
          },
        },
        {
          name: "task",
          arguments: {
            description: "Manual sibling",
            agent: "general",
            background: true,
            prompt:
              '!tool bash {"command":"printf sibling-proof > manual-sibling.txt"}',
          },
        },
      ]),
  });
  await expect
    .poll(() => client.listPermissions().then((items) => items.length))
    .toBe(2);
  // Requests are decided one at a time: the dock shows the oldest and counts
  // the rest, so both children's prompts never stack.
  await expect(
    page.getByRole("button", { name: "Always allow", exact: true }),
  ).toHaveCount(1);
  await expect(page.locator(".prompt-count")).toHaveText("1 of 2");
  await page
    .getByRole("button", {
      name: "Open agent task: Selective Auto child",
      exact: true,
    })
    .click();
  await page.getByRole("button", { name: "Always allow", exact: true }).click();
  await page
    .getByRole("menuitem", { name: "Allow once + Auto", exact: true })
    .click();
  await expect
    .poll(() =>
      readFile(join(workspace, "selective-child.txt"), "utf8").catch(
        () => null,
      ),
    )
    .toBe("selective-child-proof");
  await expect
    .poll(() => client.listPermissions().then((items) => items.length))
    .toBe(1);
  expect(
    await readFile(join(workspace, "manual-sibling.txt"), "utf8").catch(
      () => null,
    ),
  ).toBeNull();
  await page.getByRole("button", { name: "Search", exact: true }).click();
  await page
    .getByRole("combobox", { name: "Search Haarness" })
    .fill("Switch approvals to Review");
  await page
    .getByRole("option")
    .filter({ hasText: "Switch approvals to Review" })
    .click();
  await page.getByRole("button", { name: "Search", exact: true }).click();
  await page
    .getByRole("combobox", { name: "Search Haarness" })
    .fill("Enable Auto approvals");
  await expect(
    page.getByRole("option").filter({ hasText: "Enable Auto approvals" }),
  ).toHaveAttribute("aria-disabled", "false");
  await page.getByRole("combobox", { name: "Search Haarness" }).press("Escape");
  await page
    .getByRole("button", { name: "Back to parent chat", exact: true })
    .click();
  await expect(
    page.getByRole("button", { name: "Approvals: Review", exact: true }),
  ).toBeVisible();
  await page.getByRole("button", { name: "Deny", exact: true }).click();
  await expect
    .poll(() => client.activeTurns().then((items) => items.length))
    .toBe(0);
  await expect(page.getByText(/Auto approval failed/)).toHaveCount(0);
});

test("workspace Auto stays scoped across collapsed projects and the new composer fits narrow windows", async () => {
  test.setTimeout(120000);
  await showChat();
  await newSession();
  // The pill is named after the mode it shows ("Approvals: Review").
  const toggle = page.getByRole("button", { name: /^Approvals: (Review|Auto)$/ });
  await expect(toggle).toHaveAccessibleName("Approvals: Review");
  await toggle.click();
  await send(
    '!tool bash {"command":"printf workspace-auto-proof > workspace-auto.txt"}',
  );
  await expect
    .poll(() => client.activeTurns().then((items) => items.length))
    .toBe(0);
  await expect
    .poll(() =>
      readFile(join(workspace, "workspace-auto.txt"), "utf8").catch(() => null),
    )
    .toBe("workspace-auto-proof");
  // A real Git worktree shares the project but must not share its Auto setting.
  const checkout = (await client.createWorkspace({ name: "Auto isolation" }))
    .workspace;
  extraWorkspaces.push(checkout.path);
  const endpoint = JSON.parse(
    execFileSync(
      binary,
      ["--workspace", checkout.path, "--trust-project", "desktop"],
      {
        env,
        input: JSON.stringify({ action: "connect" }),
        encoding: "utf8",
        timeout: 120000,
      },
    ),
  );
  const child = new Haarness({ baseUrl: endpoint.url, token: endpoint.token });
  try {
    const session = await child.createThread({
      title: "Worktree approval boundary",
      provider: "echo",
      model: "echo-1",
    });
    await child.startTurn(session.id, {
      text: '!tool bash {"command":"printf worktree-review-proof"}',
      requestId: crypto.randomUUID(),
    });
    await expect
      .poll(() => child.listPermissions().then((items) => items.length))
      .toBe(1);
    await app.evaluate(({ dialog }, path) => {
      const original = dialog.showOpenDialog;
      dialog.showOpenDialog = async () => {
        dialog.showOpenDialog = original;
        return { canceled: false, filePaths: [path] };
      };
    }, checkout.path);
    await page
      .getByRole("button", { name: "Open another project", exact: true })
      .click();
    await expect(page.locator(".application")).toHaveAttribute(
      "data-project-ready",
      "true",
    );
    await expect(toggle).toHaveAccessibleName("Approvals: Review");
    await expect(
      page.getByRole("button", { name: "Allow once", exact: true }),
    ).toBeVisible();
    expect(await child.listPermissions()).toHaveLength(1);
    await page.getByRole("button", { name: "Deny", exact: true }).click();
    await expect
      .poll(() => child.activeTurns().then((items) => items.length))
      .toBe(0);
  } finally {
    child.close();
  }
  const directory = join(temporary, "manual-auto-boundary");
  await mkdir(join(directory, ".haarness"), { recursive: true });
  await writeFile(
    join(directory, ".haarness", "config.toml"),
    '[defaults]\nprovider="echo"\nmodel="echo-1"\n[permissions]\nbash="ask"\n',
  );
  extraWorkspaces.push(directory);
  await app.evaluate(({ dialog }, path) => {
    const original = dialog.showOpenDialog;
    dialog.showOpenDialog = async () => {
      dialog.showOpenDialog = original;
      return { canceled: false, filePaths: [path] };
    };
  }, directory);
  await page
    .getByRole("button", { name: "Open another project", exact: true })
    .click();
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
  );
  await newSession();
  await expect(toggle).toHaveAccessibleName("Approvals: Review");
  await send('!tool bash {"command":"printf manual-workspace-proof"}');
  await expect(
    page.getByRole("button", { name: "Allow once", exact: true }),
  ).toBeVisible();
  await page.getByRole("button", { name: "Deny", exact: true }).click();
  // Collapsing the original project must not stop the Auto consumer.
  const header = page
    .getByRole("region", { name: "Project workspace", exact: true })
    .getByRole("button", { name: "workspace", exact: true });
  await expect(header).toBeVisible();
  // Its chevron collapses it; its name would open it, as another project.
  if ((await header.getAttribute("aria-expanded")) === "true")
    await page.getByRole("button", { name: "Collapse project workspace", exact: true }).click();
  await expect(header).toHaveAttribute("aria-expanded", "false");
  // Desktop's Auto for new chats covers chats Desktop starts. A chat another
  // client starts (the TUI, a phone, the API) stays in Review.
  const background = await client.createThread({
    title: "Collapsed Auto verification",
    provider: "echo",
    model: "echo-1",
  });
  await client.startTurn(background.id, {
    text: '!tool bash {"command":"printf collapsed-auto-proof > collapsed-auto.txt"}',
    requestId: crypto.randomUUID(),
  });
  await expect
    .poll(() => client.listPermissions().then((items) => items.length))
    .toBe(1);
  expect(
    await readFile(join(workspace, "collapsed-auto.txt"), "utf8").catch(
      () => null,
    ),
  ).toBeNull();
  // The server keeps the mode, so Auto set elsewhere answers the waiting
  // request while this project is collapsed and no window watches it.
  await client.setApprovalMode(background.id, { mode: "auto" });
  await expect
    .poll(() => client.activeTurns().then((items) => items.length))
    .toBe(0);
  expect(await readFile(join(workspace, "collapsed-auto.txt"), "utf8")).toBe(
    "collapsed-auto-proof",
  );
  await expect(header).toHaveAttribute("aria-expanded", "false");
  // Earlier scenarios may leave a side panel open, which a narrow window
  // shows in place of the chat and its composer.
  const hidePanel = page.getByRole("button", {
    name: "Hide side panel",
    exact: true,
  });
  if (await hidePanel.isVisible()) await hidePanel.click();
  // Use the actual BrowserWindow dimensions to exercise wrapping with the real controls.
  const size = await app.evaluate(({ BrowserWindow }) => {
    const win = BrowserWindow.getAllWindows()[0];
    const size = win.getSize();
    win.setSize(800, 720);
    return size;
  });
  await expect(page.locator(".composer")).toBeVisible();
  const bounds = await page.locator(".composer").evaluate((element) => ({
    width: element.clientWidth,
    scroll: element.scrollWidth,
  }));
  expect(bounds.scroll).toBeLessThanOrEqual(bounds.width + 1);
  await page.screenshot({ path: "test-results/composer-narrow.png" });
  await app.evaluate(
    ({ BrowserWindow }, size) =>
      BrowserWindow.getAllWindows()[0].setSize(size[0], size[1]),
    size,
  );
  // Restore the primary workspace and turn off its default using the same user controls.
  await page.getByRole("button", { name: "Search", exact: true }).click();
  await page
    .getByRole("combobox", { name: "Search Haarness" })
    .fill("Collapsed Auto verification");
  await page
    .getByRole("option")
    .filter({ hasText: "Collapsed Auto verification" })
    .first()
    .click();
  await expect(
    page.locator(".session-bar-name, .workspace-tab.active"),
  ).toContainText("Collapsed Auto verification");
  await newSession();
  await expect(
    page.locator(".session-bar-name, .workspace-tab.active"),
  ).toHaveCount(0);
  await expect(toggle).toHaveAccessibleName("Approvals: Auto");
  await toggle.click();
  await expect(toggle).toHaveAccessibleName("Approvals: Review");
  await page.reload();
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
  );
  await expect(toggle).toHaveAccessibleName("Approvals: Review");
  if ((await header.getAttribute("aria-expanded")) === "false")
    await header.click();
  await expect(header).toHaveAttribute("aria-expanded", "true");
  expect(errors).toEqual([]);
});

test("SSE chat fabric: live streaming, guidance, permissions, questions and interruption", async () => {
  await newSession();
  await send("::mdslow");
  await expect(page.getByTestId("streaming-reply")).toContainText(
    "Live markdown",
  );
  await expect.poll(() => client.activeTurns().then((v) => v.length)).toBe(0);
  // ::mdslow ends after about 1.4 s, which a busy machine outruns; this turn
  // stays open while the running composer is inspected.
  await send("!linger 10000 running-composer-proof");
  const composer = page.getByRole("textbox", { name: "Message", exact: true });
  await expect(page.locator(".composer")).toHaveClass(/running/);
  await expect(page.locator(".working-strip, .composer-hint")).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: "Stop response", exact: true }),
  ).toHaveCount(1);
  await expect(page.locator(".composer")).toHaveCSS(
    "animation-name",
    "composer-breathe",
  );
  await page.emulateMedia({ reducedMotion: "reduce" });
  await expect(page.locator(".composer")).toHaveCSS("animation-name", "none");
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await page.screenshot({ path: "test-results/composer-working.png" });
  await composer.fill("guidance draft");
  await expect(
    page.getByRole("button", { name: "Send message", exact: true }),
  ).toBeEnabled();
  await expect(
    page.getByRole("button", { name: "Stop response", exact: true }),
  ).toHaveCount(0);
  await composer.fill("");
  await expect(
    page.getByRole("button", { name: "Stop response", exact: true }),
  ).toBeEnabled();
  await send("queued-guidance-proof");
  await expect(
    page.locator(".transcript article.assistant").last(),
  ).toContainText("queued-guidance-proof", { timeout: 30_000 });
  await expect.poll(() => client.activeTurns().then((v) => v.length)).toBe(0);
  await send('!tool bash {"command":"printf permission-proof"}');
  await expect(
    page.getByRole("button", { name: "Allow once", exact: true }),
  ).toBeVisible();
  await expect(page.locator(".composer")).toHaveClass(/waiting/);
  await expect(page.locator(".working-dots")).toHaveCount(0);
  await page.getByRole("button", { name: "Allow once", exact: true }).click();
  await expect(page.locator(".transcript")).toContainText("Tool bash =>");
  await expect
    .poll(() => client.listPermissions().then((v) => v.length))
    .toBe(0);
  await send(
    '!tool question {"prompt":"Choose the fixture color", "choices":["Green", "Blue"]}',
  );
  await expect(page.locator(".question")).toContainText(
    "Choose the fixture color",
  );
  await expect(page.locator(".composer")).toHaveClass(/waiting/);
  await page
    .locator(".question")
    .getByRole("button", { name: "Green", exact: true })
    .click();
  await page
    .locator(".question")
    .getByRole("button", { name: "Reply", exact: true })
    .click();
  await expect.poll(() => client.listQuestions().then((v) => v.length)).toBe(0);
  await expect.poll(() => client.activeTurns().then((v) => v.length)).toBe(0);
  await send('!tool bash {"command":"sleep 30"}');
  await page.getByRole("button", { name: "Allow once", exact: true }).click();
  await expect(page.locator(".assistant-activity")).toHaveText(
    "Running a command",
  );
  await page.getByRole("button", { name: "Stop response", exact: true }).click();
  await expect.poll(() => client.activeTurns().then((v) => v.length)).toBe(0);
  expect(errors).toEqual([]);
});

test("an open-ended question leaves the keyboard in the message box, its answer field a Shift+Tab away, and Enter replies", async () => {
  const thread = await client.createThread({
    title: "Open-ended question focus",
    provider: "echo",
    model: "echo-1",
  });
  // Search opens it from whichever project an earlier test left current.
  await page.getByRole("button", { name: "Search", exact: true }).click();
  await page.getByRole("combobox", { name: "Search Haarness" }).fill(thread.title!);
  await page.getByRole("option").filter({ hasText: thread.title! }).first().click();
  await expect(page.locator(".session-bar-name, .workspace-tab.active")).toContainText(thread.title!);
  const composer = page.getByRole("textbox", { name: "Message", exact: true });
  const question = page.locator(".question");
  const answer = question.getByRole("textbox", { name: "Answer", exact: true });
  const ask = (input: Record<string, unknown>) =>
    client.startTurn(thread.id, {
      text: "!tool question " + JSON.stringify(input),
      requestId: crypto.randomUUID(),
    });
  const settled = async () => {
    await expect.poll(() => client.listQuestions().then((v) => v.length)).toBe(0);
    await expect.poll(() => client.activeTurns().then((v) => v.length)).toBe(0);
  };
  await expect(composer).toBeEnabled();

  // The message box stays in view during a question and keeps the keyboard,
  // even empty: what is typed there is the next message, not an answer. The
  // answer field is the card's last stop before it.
  await composer.focus();
  await ask({ prompt: "What should the release be called?" });
  await expect(question).toContainText("What should the release be called?");
  await expect(composer).toBeFocused();
  await page.keyboard.press("Shift+Tab");
  await expect(answer).toBeFocused();
  await page.screenshot({ path: test.info().outputPath("question-open-ended-focus.png") });
  await page.keyboard.type("Aurora");
  await page.keyboard.press("Enter");
  await settled();
  await expect(question).toHaveCount(0);
  await expect(page.locator(".transcript")).toContainText("Aurora");

  // A draft being typed keeps the focus, and Enter in it still sends nothing.
  await composer.fill("half-written follow-up");
  await ask({ prompt: "Which codename instead?" });
  await expect(question).toContainText("Which codename instead?");
  await expect(composer).toBeFocused();
  await expect(answer).not.toBeFocused();
  await answer.click();
  await answer.press("Enter");
  expect(await client.listQuestions()).toHaveLength(1);
  await answer.fill("Borealis");
  await answer.press("Enter");
  await settled();
  await expect(composer).toHaveValue("half-written follow-up");
  await composer.fill("");

  // With choices to pick, the dock waits for one; typed text still replies with Enter.
  await composer.focus();
  await ask({ prompt: "Pick a fixture color", choices: ["Green", "Blue"] });
  await expect(question).toContainText("Pick a fixture color");
  await expect(composer).toBeFocused();
  await answer.fill("Teal");
  await answer.press("Enter");
  await settled();
  await expect(page.locator(".transcript")).toContainText("Teal");

  // A question nobody could answer is refused before it is shown.
  await ask({ prompt: "Unanswerable", choices: [], custom: false });
  await expect.poll(() => client.activeTurns().then((v) => v.length)).toBe(0);
  await expect(question).toHaveCount(0);
  expect(await client.listQuestions()).toHaveLength(0);
  await expect(page.locator(".transcript")).toContainText("at least one choice");
  expect(errors).toEqual([]);
});

test("a question answers from the keyboard, and only a deliberate key sends it", async () => {
  const thread = await client.createThread({
    title: "Keyboard question",
    provider: "echo",
    model: "echo-1",
  });
  await page.getByRole("button", { name: "Search", exact: true }).click();
  await page.getByRole("combobox", { name: "Search Haarness" }).fill(thread.title!);
  await page.getByRole("option").filter({ hasText: thread.title! }).first().click();
  await expect(page.locator(".session-bar-name, .workspace-tab.active")).toContainText(thread.title!);
  const question = page.locator(".question");
  const choice = (name: string) => question.getByRole("button", { name, exact: true });
  const answer = question.getByRole("textbox", { name: "Answer", exact: true });
  const ask = (input: Record<string, unknown>) =>
    client.startTurn(thread.id, {
      text: "!tool question " + JSON.stringify(input),
      requestId: crypto.randomUUID(),
    });
  const settled = async () => {
    await expect.poll(() => client.listQuestions().then((v) => v.length)).toBe(0);
    await expect.poll(() => client.activeTurns().then((v) => v.length)).toBe(0);
  };
  // What the question's tool call returned: the answer it was given.
  const replied = async () => {
    const results = (await client.listItems(thread.id)).filter((item) => item.data.type === "tool_result");
    const last = results.at(-1)?.data;
    return last?.type === "tool_result" ? last.output : "";
  };
  const modifier = process.platform === "darwin" ? "Meta" : "Control";

  // Enter on a choice of a single-answer question picks it and replies.
  await ask({ prompt: "Which color ships?", choices: ["Green", "Blue"] });
  await expect(question).toContainText("Which color ships?");
  await choice("Green").focus();
  await page.keyboard.press("Tab");
  await expect(choice("Blue")).toBeFocused();
  await page.screenshot({ path: test.info().outputPath("question-keyboard-light.png") });
  await page.keyboard.press("Enter");
  await settled();
  expect(await replied()).toContain("Blue");

  // A click marks a choice and Enter on it then replies with it; Space only marks.
  await ask({ prompt: "Which size?", choices: ["Small", "Large"] });
  await choice("Large").press(" ");
  await expect(choice("Large")).toHaveAttribute("aria-pressed", "true");
  expect(await client.listQuestions()).toHaveLength(1);
  await choice("Small").click();
  await expect(choice("Small")).toHaveAttribute("aria-pressed", "true");
  await expect(choice("Large")).toHaveAttribute("aria-pressed", "false");
  await choice("Small").press("Enter");
  await settled();
  expect(await replied()).toContain("Small");

  // On a question that takes several answers, Enter marks and unmarks and never
  // sends; Cmd/Ctrl+Enter replies with what is marked, from a choice or the field.
  await ask({ prompt: "Which platforms?", choices: ["macOS", "Linux", "Windows"], multiple: true });
  await choice("macOS").press("Enter");
  await choice("Linux").press("Enter");
  await choice("Windows").press("Enter");
  await choice("Windows").press("Enter");
  await expect(choice("macOS")).toHaveAttribute("aria-pressed", "true");
  await expect(choice("Windows")).toHaveAttribute("aria-pressed", "false");
  await answer.press("Enter");
  await page.waitForTimeout(300);
  expect(await client.listQuestions()).toHaveLength(1);
  await page.evaluate(() => window.haarness.preferences({ theme: "dark" }));
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await page.screenshot({ path: test.info().outputPath("question-keyboard-dark.png") });
  await page.evaluate(() => window.haarness.preferences({ theme: "light" }));
  await choice("Linux").press(modifier + "+Enter");
  await settled();
  expect(await replied()).toMatch(/macOS.*Linux/);

  // Marked choices and typed text go together on a question that takes several answers.
  await ask({ prompt: "Which platforms else?", choices: ["macOS", "Linux"], multiple: true });
  await choice("macOS").click();
  await answer.fill("BSD");
  await expect(choice("macOS")).toHaveAttribute("aria-pressed", "true");
  await question.getByRole("button", { name: "Reply", exact: true }).click();
  await settled();
  expect(await replied()).toMatch(/macOS, BSD/);

  // A single-answer question sends what it shows: typing unmarks a choice, choosing clears the text.
  await ask({ prompt: "Which tier?", choices: ["Free", "Pro"] });
  await choice("Pro").click();
  await answer.fill("Team");
  await expect(choice("Pro")).toHaveAttribute("aria-pressed", "false");
  await choice("Free").click();
  await expect(answer).toHaveValue("");
  await answer.fill("Team");
  await answer.press("Enter");
  await settled();
  expect(await replied()).toContain("Team");
  expect(await replied()).not.toContain("Free");

  // Text an input method is still composing does not send, even with Cmd/Ctrl.
  await ask({ prompt: "A name, please", choices: ["Keep"] });
  await answer.fill("候補");
  await answer.evaluate((field, key) => {
    for (const options of [{}, { [key === "Meta" ? "metaKey" : "ctrlKey"]: true }])
      field.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", isComposing: true, bubbles: true, cancelable: true, ...options }));
  }, modifier);
  await page.waitForTimeout(300);
  expect(await client.listQuestions()).toHaveLength(1);
  await answer.press(modifier + "+Enter");
  await settled();
  expect(await replied()).toContain("候補");
  expect(errors).toEqual([]);
});

test("Haarness's own tools read as what they did, in every language", async () => {
  const thread = await client.createThread({ title: "Own tool rows", provider: "echo", model: "echo-1" });
  await page.getByRole("button", { name: "Search", exact: true }).click();
  await page.getByRole("combobox", { name: "Search Haarness" }).fill(thread.title!);
  await page.getByRole("option").filter({ hasText: thread.title! }).first().click();
  await expect(page.locator(".session-bar-name, .workspace-tab.active")).toContainText(thread.title!);
  for (const text of [
    '!tool session_manage {"action":"list"}',
    '!tool load_tool {"queries":["browser"]}',
    '!tool memory_search {"query":"release"}',
    "!tool memory_list {}",
  ]) {
    const turn = await client.startTurn(thread.id, { requestId: crypto.randomUUID(), text });
    await client.waitTurn(turn.id);
  }
  // Every tool the server defines itself has wording of its own, so a new one without it fails here.
  const { describeTool, toolVerbKeys } = await import("../src/renderer/transcript");
  const catalog = await client.toolCatalog({ threadId: thread.id });
  const own = catalog.tools.filter((tool) => tool.source === "native" && !tool.id.startsWith("browser_"));
  expect(own.length).toBeGreaterThan(30);
  for (const { id } of own) {
    const shape = describeTool(id, {});
    expect(shape.verb, id).not.toBe(id);
    expect(toolVerbKeys, id).toContain(shape.verb);
  }
  // A management tool reads as what it manages, not the delegation or command its name once suggested.
  const rows = page.locator(".message.step > .tool-call > summary");
  await expect(rows).toHaveText([
    /^Managed chats$/,
    // No tools wait to be loaded in this chat, so the call failed.
    /^Tried to load tools\s*browser$/,
    /^Looked through memories\s*release$/,
    /^Looked through memories$/,
  ]);
  await page.screenshot({ path: test.info().outputPath("own-tools-light.png") });
  await page.evaluate(() => window.haarness.preferences({ theme: "dark" }));
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await page.screenshot({ path: test.info().outputPath("own-tools-dark.png") });
  await page.evaluate(() => window.haarness.preferences({ theme: "light", language: "ar" }));
  await expect(page.locator("html")).toHaveAttribute("dir", "rtl");
  await expect(rows.last()).toHaveText("تصفّح الذاكرة");
  await page.screenshot({ path: test.info().outputPath("own-tools-rtl.png") });
  await page.evaluate(() => window.haarness.preferences({ language: "en" }));
  await expect(page.locator("html")).toHaveAttribute("dir", "ltr");
  expect(errors).toEqual([]);
});

test("SSE cross-client updates and durable recovery after window reload and server restart", async () => {
  const external = await client.createThread({
    title: "External session",
    provider: "echo",
    model: "echo-1",
  });
  await expect(
    page
      .locator(".sessions")
      .getByRole("button", { name: "External session", exact: true }),
  ).toBeVisible();
  await page
    .locator(".sessions")
    .getByRole("button", { name: "External session", exact: true })
    .click();
  const turn = await client.startTurn(external.id, {
    requestId: crypto.randomUUID(),
    text: "cross-client-proof",
  });
  await client.waitTurn(turn.id);
  await expect(page.locator(".transcript")).toContainText(
    "Echo: cross-client-proof",
  );
  await page.reload();
  await page.getByRole("region", { name: "Project workspace", exact: true }).locator(".project-heading").click();
  await expect(page.locator(".transcript")).toContainText(
    "Echo: cross-client-proof",
  );
  await expect(page.locator(".transcript article")).toHaveCount(2);
  const original = await client.info();
  await client.shutdownServer();
  await expect
    .poll(
      async () => {
        try {
          await attachClient();
          return (
            (await client.info()).serverInstanceId !== original.serverInstanceId
          );
        } catch {
          return false;
        }
      },
      { timeout: 60000 },
    )
    .toBe(true);
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
    {
      timeout: 60000,
    },
  );
  const next = await client.startTurn(external.id, {
    requestId: crypto.randomUUID(),
    text: "after-reconnect-proof",
  });
  await client.waitTurn(next.id);
  await expect(page.locator(".transcript")).toContainText(
    "Echo: after-reconnect-proof",
  );
  await expect(page.locator(".transcript article")).toHaveCount(4);
  expect(errors).toEqual([]);
});

test("focusing earlier history during streaming preserves the reading position", async () => {
  await newSession();
  const history = "[Earlier details](https://example.test/history)\n\n" + Array.from(
    { length: 50 },
    (_, index) => `History paragraph ${index + 1}. Keep this readable while the answer streams.`,
  ).join("\n\n");
  await send(history);
  await expect.poll(async () => (await client.activeTurns()).length).toBe(0);
  const transcript = page.locator(".transcript");
  const position = () => transcript.evaluate((element) => ({
    top: element.scrollTop,
    gap: element.scrollHeight - element.clientHeight - element.scrollTop,
  }));
  const jump = page.getByRole("button", { name: "Jump to latest", exact: true });
  await expect.poll(async () => (await position()).gap).toBeLessThan(5);
  // Focusing the pane itself must keep ordinary streaming follow enabled.
  await transcript.focus();
  await expect(jump).toHaveCount(0);
  await send("::scrollstream");
  await expect(page.getByTestId("streaming-reply")).toContainText("STREAM_REFLOW_STAGE_ONE");
  const link = transcript.getByRole("link", { name: "Earlier details", exact: true }).last();
  await link.focus();
  await expect(link).toBeFocused();
  await expect(jump).toBeVisible();
  const paused = await position();
  expect(paused.gap).toBeGreaterThan(100);
  await expect.poll(async () => (await client.activeTurns()).length).toBe(0);
  await expect.poll(async () => (await position()).top).toBeLessThanOrEqual(paused.top + 2);
  await expect(link).toBeInViewport();
  await page.screenshot({ path: test.info().outputPath("focus-history.png") });
  await jump.click();
  await expect.poll(async () => (await position()).gap).toBeLessThan(5);
  await expect(jump).toHaveCount(0);
  const latest = transcript.getByRole("link", { name: "release notes", exact: true }).last();
  await expect(latest).toBeInViewport();
  await latest.focus();
  await expect(jump).toHaveCount(0);
  expect(errors).toEqual([]);
});

test("streaming respects upward scrolling and resumes only at the latest output", async () => {
  await newSession();
  const history = Array.from(
    { length: 50 },
    (_, index) =>
      `History paragraph ${index + 1}. This stays readable while the next answer streams.`,
  ).join("\n\n");
  await send(history);
  await expect.poll(async () => (await client.activeTurns()).length).toBe(0);
  const transcript = page.locator(".transcript");
  const position = () =>
    transcript.evaluate((element) => ({
      top: element.scrollTop,
      gap: element.scrollHeight - element.clientHeight - element.scrollTop,
    }));
  await expect.poll(async () => (await position()).gap).toBeLessThan(5);
  await send("::scrollstream");
  await expect(page.getByTestId("streaming-reply")).toContainText(
    "STREAM_REFLOW_STAGE_ONE",
  );
  await expect(
    page.getByTestId("streaming-reply").locator(".message-label"),
  ).toHaveCount(0);
  await expect(page.locator(".assistant-activity")).toHaveText(
    "Writing a reply",
  );
  await page.emulateMedia({ reducedMotion: "reduce" });
  await expect(page.locator(".assistant-activity span")).toHaveCSS(
    "animation-name",
    "none",
  );
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await expect(page.locator(".assistant-activity span")).toHaveCSS(
    "animation-name",
    "activity-shimmer",
  );
  await expect(
    page.getByText("Your next message guides the current task.", {
      exact: true,
    }),
  ).toHaveCount(0);
  await transcript.hover();
  await page.mouse.wheel(0, -32);
  const jump = page.getByRole("button", {
    name: "Jump to latest",
    exact: true,
  });
  await expect(jump).toBeVisible();
  await expect.poll(async () => (await position()).gap).toBeGreaterThan(15);
  const near = await position();
  // A small upward wheel movement used to be swallowed by the 120px threshold.
  await page.waitForTimeout(250);
  expect((await position()).top).toBeLessThanOrEqual(near.top + 2);
  await page.mouse.wheel(0, -320);
  await expect.poll(async () => (await position()).gap).toBeGreaterThan(250);
  const paused = await position();
  await expect.poll(async () => (await client.activeTurns()).length).toBe(0);
  await expect
    .poll(async () => (await position()).top)
    .toBeLessThanOrEqual(paused.top + 2);
  await expect(jump).toBeVisible();
  await page.screenshot({ path: "test-results/jump-latest.png" });
  await jump.click();
  await expect.poll(async () => (await position()).gap).toBeLessThan(5);
  await expect(jump).toHaveCount(0);
  await send("::scrollstream");
  await expect(page.getByTestId("streaming-reply")).toBeVisible();
  await expect.poll(async () => (await client.activeTurns()).length).toBe(0);
  await expect.poll(async () => (await position()).gap).toBeLessThan(5);
  await transcript.focus();
  await expect(transcript).toHaveCSS("outline-style", "none");
  await page.keyboard.press("PageUp");
  await expect(jump).toBeVisible();
  await expect.poll(async () => (await position()).gap).toBeGreaterThan(50);
  await page.keyboard.press("End");
  await expect.poll(async () => (await position()).gap).toBeLessThan(5);
  await expect(jump).toHaveCount(0);
  expect(errors).toEqual([]);
});

test("settings: appearance, native shortcuts, catalogs, plugins and extensions", async () => {
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  const settings = page.getByRole("dialog", { name: "Settings", exact: true });
  const theme = settings.getByRole("combobox", {
    name: "Appearance",
    exact: true,
  });
  await selectValue(theme, "light");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  await page.screenshot({ path: "test-results/settings-light.png" });
  await selectValue(theme, "dark");
  for (const name of [
    "Servers",
    "About",
    "Providers",
    "Models",
    "Agents",
    "Commands",
    "Skills",
    "Tools",
    "MCP servers",
    "Plugins",
    "Extensions",
    "Schedules",
    "Hooks",
    "Profiles",
  ]) {
    await settings.getByRole("button", { name, exact: true }).click();
    await expect(
      settings.getByRole("heading", { name, exact: true }),
    ).toBeVisible();
    await expect(
      settings.getByText("Server capabilities", { exact: true }),
    ).toHaveCount(0);
    await expect(
      settings.getByText("Architecture", { exact: true }),
    ).toHaveCount(0);
    await expect(
      settings.getByRole("button", { name: "Inspect capabilities", exact: true }),
    ).toHaveCount(0);
    await expect(
      settings.getByRole("button", { name: "View recent logs", exact: true }),
    ).toHaveCount(0);
    if (name === "Servers" || name === "About")
      await page.screenshot({
        path: `test-results/settings-${name.toLowerCase()}-clean.png`,
      });
    await expect(page.getByRole("alert")).toHaveCount(0);
  }
  await settings.getByRole("button", { name: "Plugins", exact: true }).click();
  await settings
    .getByRole("button", { name: "Install plugin", exact: true })
    .click();
  // Scoped to the dialog: page-wide, "Folder" also matches the explorer's
  // "New folder" button whenever that panel happens to be open.
  const installer = page.locator(
    "dialog:not(.settings-dialog):not(.palette-dialog)",
  );
  await installer.getByLabel("Folder", { exact: true }).fill(join(temporary, "plugin"));
  await installer.getByRole("button", { name: "Install", exact: true }).click();
  await expect(
    settings.getByRole("switch", { name: "Enable plugin desktop-fixture" }),
  ).toBeChecked();
  await settings
    .getByRole("switch", { name: "Enable plugin desktop-fixture" })
    .uncheck();
  await expect
    .poll(
      async () =>
        (await client.plugins()).find((p) => p.name === "desktop-fixture")
          ?.enabled,
    )
    .toBe(false);
  await settings
    .getByRole("button", { name: "Extensions", exact: true })
    .click();
  await settings
    .getByRole("button", { name: "Install extension", exact: true })
    .click();
  await page
    .getByLabel("Extension path")
    .fill(join(temporary, "extension", "index.ts"));
  await page
    .locator("dialog:not(.settings-dialog):not(.palette-dialog)")
    .getByRole("button", { name: "Trust & install", exact: true })
    .click();
  await expect(
    settings.getByText(join(temporary, "extension", "index.ts"), {
      exact: true,
    }),
  ).toBeVisible();
  await settings
    .getByRole("switch", { name: "Enable extensions", exact: true })
    .check();
  for (const name of [
    "Permissions",
    "Compaction",
    "Snapshots",
    "Language servers",
    "Formatters",
    "Instructions",
    "Configuration files",
  ])
    await expect(
      settings.getByRole("button", { name, exact: true }),
    ).toHaveCount(0);
  await expect(settings.locator(".configuration-editor")).toHaveCount(0);
  await settings.getByRole("button", { name: "Close settings" }).click();
  await expect
    .poll(async () =>
      (await client.toolCatalog()).tools.some(
        (tool) => tool.id === "ext__desktop_extension__ping",
      ),
    )
    .toBe(true);
  await newSession();
  await send("!tool ext__desktop_extension__ping {}");
  const allow = page.getByRole("button", { name: "Allow once", exact: true });
  await expect
    .poll(
      async () =>
        (await client.listPermissions()).length ||
        (await client.activeTurns()).length === 0,
    )
    .toBeTruthy();
  if (await allow.isVisible()) await allow.click();
  await expect(page.locator(".transcript")).toContainText("extension-proof");
  await expect(page.getByRole("alert")).toHaveCount(0);
  expect(errors).toEqual([]);
});

test("file editing protects unsaved drafts and conflicting external changes", async () => {
  await openProjectPanel("Files");
  await page.locator(".file-tree").getByRole("button", { name: "README.md", exact: true }).click();
  await page.locator(".file-preview .cm-content").fill("Unsaved desktop draft");
  await openProjectPanel("Review");
  await openProjectPanel("Files");
  await expect(page.locator(".file-preview .cm-content")).toContainText(
    "Unsaved desktop draft",
  );
  await writeFile(join(workspace, "README.md"), "External edit wins");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  // A conflicting save offers a choice where the edits are, not a dead end.
  const changed = page.locator(".file-changed");
  await expect(changed).toContainText(/changed on disk/i);
  expect(await readFile(join(workspace, "README.md"), "utf8")).toBe(
    "External edit wins",
  );
  await changed.getByRole("button", { name: "Reload", exact: true }).click();
  await expect(changed).toHaveCount(0);
  await expect(page.locator(".file-preview .cm-content")).toHaveText(
    "External edit wins",
  );
  await expect(page.getByRole("button", { name: "Save", exact: true })).toBeDisabled();
  await page.locator(".file-preview .cm-content").fill("Kept desktop draft");
  await writeFile(join(workspace, "README.md"), "External edit loses");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await changed.getByRole("button", { name: "Keep mine", exact: true }).click();
  await expect(changed).toHaveCount(0);
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect
    .poll(() => readFile(join(workspace, "README.md"), "utf8"))
    .toBe("Kept desktop draft");
  await page.locator(".file-preview .cm-content").fill("Discarded desktop draft");
  await page
    .getByRole("button", { name: "Close README.md", exact: true })
    .click();
  await expect(page.locator("dialog:not(.settings-dialog):not(.palette-dialog)")).toContainText("Discard unsaved changes");
  await page
    .locator("dialog:not(.settings-dialog):not(.palette-dialog)")
    .getByRole("button", { name: "Discard changes", exact: true })
    .click();
  const reply = await page.evaluate(async (workspace) => {
    const id = "local:" + workspace;
    const result: string[] = [];
    for (const path of ["../outside", ".git/config"]) {
      try {
        await window.haarness.files(id, "read", { path });
        result.push("unsafe");
      } catch {
        result.push("rejected");
      }
    }
    return {
      result,
      node: typeof (window as unknown as { require?: unknown }).require,
    };
  }, workspace);
  expect(reply).toEqual({
    result: ["rejected", "rejected"],
    node: "undefined",
  });
  expect(errors).toEqual([]);
});

test("custom agents, commands and skills preserve their native metadata", async () => {
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  const settings = page.getByRole("dialog", { name: "Settings", exact: true });
  for (const [section, singular, body] of [
    ["Agents", "agent", "Instructions"],
    ["Commands", "command", "Prompt template"],
    ["Skills", "skill", "Instructions"],
  ]) {
    await settings.getByRole("button", { name: section, exact: true }).click();
    await settings
      .getByRole("button", { name: "Create " + singular, exact: true })
      .click();
    const dialog = page.locator("dialog:not(.settings-dialog):not(.palette-dialog)");
    await dialog
      .getByLabel("Name", { exact: true })
      .fill("desktop-" + singular);
    await dialog
      .getByLabel("Description", { exact: true })
      .fill("Desktop management fixture");
    await dialog
      .getByLabel(body, { exact: true })
      .fill("Return desktop asset evidence.");
    await selectValue(
      dialog.getByLabel("Available in", { exact: true }),
      "local",
    );
    if (singular === "agent") {
      await selectValue(
        dialog.getByLabel("Available as", { exact: true }),
        "subagent",
      );
    }
    await dialog.getByRole("button", { name: "Create", exact: true }).click();
    await expect(
      settings
        .locator(".setting-row")
        .filter({ hasText: "desktop-" + singular }),
    ).toBeVisible();
    if (singular === "agent")
      await page.evaluate(
        (directory) =>
          window.haarness.native("local:" + directory, {
            action: "asset",
            kind: "agent",
            input: {
              action: "edit",
              name: "desktop-agent",
              scope: "local",
              tools: { bash: false },
            },
          }),
        workspace,
      );
    const row = settings
      .locator(".setting-row")
      .filter({ hasText: "desktop-" + singular });
    await row
      .getByRole("button", { name: "Edit", exact: true })
      .click();
    await expect(dialog.getByLabel(body, { exact: true })).toHaveValue(
      "Return desktop asset evidence.",
    );
    await expect(
      dialog.getByLabel("Available in", { exact: true }),
    ).toHaveAttribute("data-value", "local");
    if (singular === "agent")
      await expect(
        dialog.getByLabel("Tool ceiling (JSON)", { exact: true }),
      ).toHaveCount(0);
    await dialog
      .getByLabel(body, { exact: true })
      .fill("Updated desktop asset evidence.");
    await dialog
      .getByRole("button", { name: "Save changes", exact: true })
      .click();
    await expect(page.getByRole("alert")).toHaveCount(0);
    await expect(dialog).toHaveCount(0);
    if (singular === "agent")
      await expect
        .poll(() =>
          readFile(
            join(workspace, ".haarness/agents/desktop-agent.md"),
            "utf8",
          ),
        )
        .toContain("bash");
  }
  await settings.getByRole("button", { name: "Close settings" }).click();
  await newSession();
  await expect
    .poll(
      async () =>
        (await client.commands()).find(
          (item) => item.name === "desktop-command",
        )?.template,
    )
    .toBe("Updated desktop asset evidence.");
  await send("/desktop-command");
  await expect(page.locator(".transcript")).toContainText(
    "Updated desktop asset evidence.",
  );
  expect(errors).toEqual([]);
});

test("slash commands that answer with text show the answer above the composer", async () => {
  await newSession();
  const note = page.locator(".command-note");
  await send("/goal status");
  await expect(note).toContainText("/goal status");
  await expect(note.getByRole("status")).toContainText("No active goal");
  await send("/workflow");
  await expect(note).toContainText("/workflow");
  await expect(note.getByRole("status")).toContainText("Workflows (");
  await note.getByRole("button", { name: "Dismiss", exact: true }).click();
  await expect(note).toHaveCount(0);
  // The next message replaces the answer instead of leaving it behind.
  await send("/goal status");
  await expect(note).toHaveCount(1);
  await send("after-command-proof");
  await expect(page.locator(".transcript")).toContainText(
    "Echo: after-command-proof",
  );
  await expect(note).toHaveCount(0);
  // A workflow run's answer follows the run instead of staying "running".
  await expect.poll(() => client.activeTurns().then((v) => v.length)).toBe(0);
  await send("/workflow run fixture");
  await expect(note.getByRole("status")).toContainText(
    "Workflow fixture finished.",
  );
  await expect(note.getByRole("status")).toContainText("Run ID: ");
  await expect(page.locator(".transcript")).toContainText(
    "Background workflow completed: fixture",
  );
  await expect.poll(() => client.activeTurns().then((v) => v.length)).toBe(0);
  expect(errors).toEqual([]);
});

test("global and project resources stay separate through desktop management", async () => {
  execFileSync(binary, ["--workspace", workspace, "--trust-project", "extension", "install", join(temporary, "extension", "index.ts")], { env, stdio: "pipe", timeout: 60000 });
  execFileSync(binary, ["--workspace", workspace, "--trust-project", "extension", "enable"], { env, stdio: "pipe", timeout: 60000 });
  const other = join(temporary, "scope-other-project");
  await mkdir(other, { recursive: true });
  const native = (directory: string, input: unknown) =>
    JSON.parse(
      execFileSync(
        binary,
        ["--workspace", directory, "--trust-project", "desktop"],
        {
          env,
          input: JSON.stringify(input),
          encoding: "utf8",
          timeout: 240000,
        },
      ),
    );
  const inherited = native(other, { action: "extensions", project: true });
  expect(inherited.enabled).toBe(true);
  expect(inherited.entries).toEqual([]);
  for (const scope of ["global", "local"])
    native(workspace, {
      action: "asset",
      kind: "command",
      input: {
        action: "create",
        name: "scope-proof",
        scope,
        description: scope,
        template: scope + " command",
      },
    });
  expect(
    native(workspace, {
      action: "asset",
      kind: "command",
      input: { action: "get", name: "scope-proof" },
    }).scope,
  ).toBe("local");
  expect(
    native(other, {
      action: "asset",
      kind: "command",
      input: { action: "get", name: "scope-proof" },
    }).scope,
  ).toBe("global");
  expect(
    await readFile(
      join(temporary, "state/config/commands/scope-proof.md"),
      "utf8",
    ),
  ).toContain("global command");
  expect(
    await readFile(
      join(workspace, ".haarness/commands/scope-proof.md"),
      "utf8",
    ),
  ).toContain("local command");
  native(workspace, {
    action: "set_credential",
    provider: "scope-fixture",
    key: "fixture-key",
  });
  expect(
    JSON.parse(await readFile(join(temporary, "state/data/auth.json"), "utf8"))[
      "scope-fixture"
    ],
  ).toBeDefined();
  native(workspace, { action: "remove_credential", provider: "scope-fixture" });
  await mkdir(join(temporary, "project-extension"));
  const source = join(temporary, "project-extension", "index.ts");
  await writeFile(
    source,
    "export default {apiVersion:1,name:'project_scope_extension',tools:{ping:{description:'Project scope verification',parameters:{type:'object'},execute(){return 'project-scope-proof'}}}};\n",
  );
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  const settings = page.getByRole("dialog", { name: "Settings", exact: true });
  await settings
    .getByRole("button", { name: "Extensions", exact: true })
    .click();
  await selectValue(
    settings.getByRole("combobox", { name: "Extension scope" }),
    "Project",
  );
  await expect(
    settings.getByRole("switch", { name: "Enable extensions", exact: true }),
  ).toBeChecked();
  await settings
    .getByRole("button", { name: "Install extension", exact: true })
    .click();
  await page.getByLabel("Extension path").fill(source);
  await page
    .locator("dialog:not(.settings-dialog):not(.palette-dialog)")
    .getByRole("button", { name: "Trust & install", exact: true })
    .click();
  await expect(settings.getByText(source, { exact: true })).toBeVisible();
  expect(
    await readFile(join(workspace, ".haarness/extensions.json"), "utf8"),
  ).toContain("project_scope_extension");
  expect(
    await readFile(join(temporary, "state/config/extensions.json"), "utf8"),
  ).not.toContain("project_scope_extension");
  await selectValue(
    settings.getByRole("combobox", { name: "Extension scope" }),
    "Global",
  );
  await expect(settings.getByText(source, { exact: true })).toHaveCount(0);
  await selectValue(
    settings.getByRole("combobox", { name: "Extension scope" }),
    "Project",
  );
  await settings
    .getByRole("switch", {
      name: "Enable extension project_scope_extension",
      exact: true,
    })
    .uncheck();
  await expect
    .poll(async () =>
      (await client.toolCatalog()).tools.some(
        (tool) => tool.id === "ext__project_scope_extension__ping",
      ),
    )
    .toBe(false);
  await expect
    .poll(async () =>
      (await client.toolCatalog()).tools.some(
        (tool) => tool.id === "ext__desktop_extension__ping",
      ),
    )
    .toBe(true);
  await settings.getByRole("button", { name: "Close settings" }).click();
  expect(errors).toEqual([]);
});

test("MCP form connects local and HTTP servers with structured arguments, environment, headers and OAuth settings", async () => {
  test.setTimeout(180000);
  const settings = page.getByRole("dialog", { name: "Settings", exact: true });
  const form = page.getByRole("dialog", {
    name: "Add MCP server",
    exact: true,
  });
  const submit = () =>
    form.getByRole("button", { name: "Add server", exact: true }).click();
  const local = "desktop-stdio-form",
    remote = "desktop-http-form",
    oauth = "desktop-oauth-form";
  const native = (input: Record<string, unknown>) =>
    JSON.parse(
      execFileSync(
        binary,
        ["--workspace", workspace, "--trust-project", "desktop"],
        {
          env,
          encoding: "utf8",
          timeout: 30000,
          stdio: ["pipe", "pipe", "pipe"],
          input: JSON.stringify({ action: "asset", kind: "mcp", input }),
        },
      ),
    );
  const config = await readFile(
    join(workspace, ".haarness", "config.toml"),
    "utf8",
  );
  const capture = join(temporary, "mcp-argv.json");
  const script = join(temporary, "mcp server.py");
  await writeFile(
    script,
    `import os, sys, json, pathlib, runpy\npathlib.Path(${JSON.stringify(capture)}).write_text(json.dumps({"args":sys.argv[1:], "token":os.environ.get("MCP_FORM_TOKEN"), "empty":os.environ.get("MCP_EMPTY")}))\nrunpy.run_path(${JSON.stringify(join(root, "sdks/tests/mcp.py"))}, run_name="__main__")\n`,
  );
  const python = execFileSync(
    "python3",
    ["-c", "import sys; print(sys.executable)"],
    { encoding: "utf8" },
  ).trim();
  const received: {
    method: string;
    authorization?: string;
    tenant?: string;
  }[] = [];
  const http = createServer(async (request, response) => {
    if (request.method !== "POST") {
      response.writeHead(405).end();
      return;
    }
    const chunks: Buffer[] = [];
    for await (const chunk of request) chunks.push(Buffer.from(chunk));
    const message = JSON.parse(Buffer.concat(chunks).toString());
    received.push({
      method: message.method,
      authorization: request.headers.authorization,
      tenant: request.headers["x-tenant"] as string,
    });
    if (message.id === undefined) {
      response.writeHead(202).end();
      return;
    }
    const result =
      message.method === "initialize"
        ? {
            protocolVersion: message.params.protocolVersion,
            capabilities: { tools: {} },
            serverInfo: { name: "form-fixture", version: "1" },
          }
        : message.method === "tools/list"
          ? {
              tools: [
                {
                  name: "echo",
                  description: "Echo input",
                  inputSchema: { type: "object" },
                },
              ],
            }
          : message.method === "tools/call"
            ? {
                content: [
                  { type: "text", text: message.params.arguments.text },
                ],
                isError: false,
              }
            : {};
    response
      .writeHead(200, { "Content-Type": "application/json" })
      .end(JSON.stringify({ jsonrpc: "2.0", id: message.id, result }));
  });
  await new Promise<void>((resolve) => http.listen(0, "127.0.0.1", resolve));
  const address = http.address();
  if (!address || typeof address === "string")
    throw Error("Missing fixture port");
  const url = `http://127.0.0.1:${address.port}/mcp`;
  try {
    await page.getByRole("button", { name: "Settings", exact: true }).click();
    await settings
      .getByRole("button", { name: "MCP servers", exact: true })
      .click();
    const open = () =>
      settings
        .getByRole("button", { name: "Add MCP server", exact: true })
        .click();
    await open();
    await form.getByRole("textbox", { name: "Name", exact: true }).fill(local);
    await form
      .getByRole("textbox", { name: "Executable", exact: true })
      .fill(python);
    await selectValue(
      form.getByRole("combobox", { name: "Available in" }),
      "local",
    );
    const addArgument = async (key: string, value = "") => {
      await form
        .getByRole("button", { name: "Add argument", exact: true })
        .click();
      const count = await form.locator("[data-mcp-argument]").count();
      await form
        .getByRole("textbox", { name: `Argument ${count}`, exact: true })
        .fill(key);
      await form
        .getByRole("textbox", { name: `Argument ${count} value`, exact: true })
        .fill(value);
    };
    await addArgument(script);
    await addArgument("--path", "/folder with spaces");
    await addArgument("--mode=fixture");
    await addArgument("remove-me");
    await form.getByRole("button", { name: "Remove argument 4" }).click();
    await form.getByRole("button", { name: "Move argument 3 up" }).click();
    await form
      .getByRole("button", { name: "Add environment variable", exact: true })
      .click();
    await form
      .getByRole("textbox", { name: "Variable name 1", exact: true })
      .fill("MCP_FORM_TOKEN");
    await form
      .getByLabel("environment variable 1 value", { exact: true })
      .fill("fixture=secret with spaces");
    await expect(
      form.getByLabel("environment variable 1 value", { exact: true }),
    ).toHaveAttribute("type", "password");
    await form
      .getByRole("button", { name: "Show environment variable values" })
      .click();
    await expect(
      form.getByLabel("environment variable 1 value", { exact: true }),
    ).toHaveAttribute("type", "text");
    await form
      .getByRole("button", { name: "Add environment variable", exact: true })
      .click();
    await form
      .getByRole("textbox", { name: "Variable name 2", exact: true })
      .fill("MCP_FORM_TOKEN");
    await submit();
    await expect(form.getByRole("alert")).toContainText("repeats a name");
    expect(
      (await client.mcpServers()).some((item) => item.name === local),
    ).toBe(false);
    await form
      .getByRole("textbox", { name: "Variable name 2", exact: true })
      .fill("MCP_EMPTY");
    await form
      .getByRole("button", { name: "Remote HTTP", exact: false })
      .click();
    await expect(
      form.getByRole("textbox", { name: "Executable", exact: true }),
    ).toHaveCount(0);
    await form
      .getByRole("textbox", { name: "Server URL", exact: true })
      .fill(url);
    await form
      .getByRole("button", { name: "Local command", exact: false })
      .click();
    await expect(
      form.getByRole("textbox", { name: "Argument 2", exact: true }),
    ).toHaveValue("--mode=fixture");
    await expect(
      form.getByLabel("environment variable 1 value", { exact: true }),
    ).toHaveValue("fixture=secret with spaces");
    await page.screenshot({ path: "test-results/mcp-local-form.png" });
    await submit();
    await expect(form).toHaveCount(0);
    await expect
      .poll(
        async () =>
          (await client.mcpServers()).find((item) => item.name === local)
            ?.status,
      )
      .toBe("connected");
    await expect(
      settings.getByRole("heading", { name: local, exact: true }),
    ).toBeVisible();
    expect(JSON.parse(await readFile(capture, "utf8"))).toEqual({
      args: ["--mode=fixture", "--path", "/folder with spaces"],
      token: "fixture=secret with spaces",
      empty: "",
    });
    expect((await client.mcpTools(local)).map((tool) => tool.name)).toContain(
      `mcp__${local}__echo`,
    );
    expect(
      (await client.callMcpTool(local, "echo", { text: "STDIO_FORM_OK" })).text,
    ).toContain("STDIO_FORM_OK");
    const localConfig = native({ action: "get", name: local, scope: "local" });
    expect(JSON.stringify(localConfig)).not.toContain("fixture=secret");

    await open();
    await form.getByRole("textbox", { name: "Name", exact: true }).fill(remote);
    await form
      .getByRole("button", { name: "Remote HTTP", exact: false })
      .click();
    await form
      .getByRole("textbox", { name: "Server URL", exact: true })
      .fill(url);
    await selectValue(
      form.getByRole("combobox", { name: "OAuth", exact: true }),
      "disabled",
    );
    await expect(form.getByText("OAuth options", { exact: false })).toHaveCount(
      0,
    );
    await form.getByRole("button", { name: "Add header", exact: true }).click();
    await form
      .getByRole("textbox", { name: "Header name 1", exact: true })
      .fill("Authorization");
    await form
      .getByLabel("header 1 value", { exact: true })
      .fill("Bearer desktop-fixture");
    await form.getByRole("button", { name: "Add header", exact: true }).click();
    await form
      .getByRole("textbox", { name: "Header name 2", exact: true })
      .fill("authorization");
    await submit();
    await expect(form.getByRole("alert")).toContainText("repeats a name");
    await form
      .getByRole("textbox", { name: "Header name 2", exact: true })
      .fill("");
    await form
      .getByRole("textbox", { name: "Header name 2", exact: true })
      .pressSequentially("X-Tenant");
    await expect(
      form.getByRole("textbox", { name: "Header name 2", exact: true }),
    ).toBeFocused();
    await form
      .getByLabel("header 2 value", { exact: true })
      .fill("desktop tenant");
    await form
      .getByRole("button", { name: "Local command", exact: false })
      .click();
    await form
      .getByRole("button", { name: "Remote HTTP", exact: false })
      .click();
    await expect(
      form.getByRole("textbox", { name: "Server URL", exact: true }),
    ).toHaveValue(url);
    await page.screenshot({ path: "test-results/mcp-remote-form.png" });
    const viewport = page.viewportSize();
    await page.setViewportSize({ width: 540, height: 800 });
    try {
      await form
        .getByLabel("header 2 value", { exact: true })
        .scrollIntoViewIfNeeded();
      expect(
        await form.evaluate(
          (element) => element.scrollWidth <= element.clientWidth + 1,
        ),
      ).toBe(true);
      await page.screenshot({ path: "test-results/mcp-narrow-form.png" });
    } finally {
      await page.setViewportSize(viewport ?? { width: 1420, height: 940 });
    }
    await submit();
    await expect(form).toHaveCount(0);
    await expect
      .poll(
        async () =>
          (await client.mcpServers()).find((item) => item.name === remote)
            ?.status,
      )
      .toBe("connected");
    await expect(
      settings.getByRole("heading", { name: remote, exact: true }),
    ).toBeVisible();
    expect(
      (await client.callMcpTool(remote, "echo", { text: "HTTP_FORM_OK" })).text,
    ).toContain("HTTP_FORM_OK");
    for (const method of ["initialize", "tools/list", "tools/call"]) {
      expect(received.find((item) => item.method === method)).toMatchObject({
        authorization: "Bearer desktop-fixture",
        tenant: "desktop tenant",
      });
    }
    const remoteConfig = native({
      action: "get",
      name: remote,
      scope: "global",
    });
    expect(JSON.stringify(remoteConfig)).not.toContain(
      "Bearer desktop-fixture",
    );

    // Server-side failures keep the draft open and do not overwrite an entry.
    await open();
    await form.getByRole("textbox", { name: "Name", exact: true }).fill(remote);
    await form
      .getByRole("textbox", { name: "Executable", exact: true })
      .fill("not-saved");
    await submit();
    await expect(form.getByRole("alert")).toContainText("already exists");
    await expect(
      form.getByRole("textbox", { name: "Executable", exact: true }),
    ).toHaveValue("not-saved");
    expect(native({ action: "get", name: remote, scope: "global" })).toEqual(
      remoteConfig,
    );
    await page.keyboard.press("Escape");
    await expect(form).toHaveCount(0);
    await expect(settings).toBeVisible();

    await open();
    await form.getByRole("textbox", { name: "Name", exact: true }).fill(oauth);
    await form
      .getByRole("button", { name: "Remote HTTP", exact: false })
      .click();
    await form
      .getByRole("textbox", { name: "Server URL", exact: true })
      .fill(url);
    await selectValue(
      form.getByRole("combobox", { name: "OAuth", exact: true }),
      "enabled",
    );
    await form.getByText("OAuth options", { exact: false }).click();
    await form
      .getByRole("textbox", { name: "Client ID", exact: true })
      .fill("desktop-client");
    await form
      .getByRole("textbox", { name: "Scopes", exact: true })
      .fill("read write read");
    await form
      .getByRole("checkbox", { name: "Enable server", exact: false })
      .uncheck();
    await submit();
    await expect(form).toHaveCount(0);
    await expect
      .poll(
        async () =>
          (await client.mcpServers()).find((item) => item.name === oauth)
            ?.status,
      )
      .toBe("disabled");
    const oauthConfig = native({ action: "get", name: oauth, scope: "global" });
    expect(oauthConfig.config.oauth).toBe(true);
    expect(oauthConfig.config.oauthClientId).toBe("desktop-client");
    expect(oauthConfig.config.oauthScopes).toEqual(["read", "write"]);
    await open();
    const box = await form.boundingBox();
    if (!box) throw Error("Missing MCP dialog");
    await page.mouse.click(box.x / 2, box.y + box.height / 2);
    await expect(form).toHaveCount(0);
    await expect(settings).toBeVisible();
    await expect(
      settings.getByRole("button", { name: "Add MCP server", exact: true }),
    ).toBeFocused();
    await settings.getByRole("button", { name: "Close settings" }).click();
    await page.reload();
    await expect(
      page.getByRole("button", { name: "Settings", exact: true }),
    ).toBeVisible();
    expect(
      (await client.mcpServers()).filter((item) =>
        [local, remote, oauth].includes(item.name),
      ),
    ).toHaveLength(3);
    expect(errors).toEqual([]);
  } finally {
    for (const name of [local, remote, oauth]) {
      try {
        native({
          action: "remove",
          name,
          scope: name === local ? "local" : "global",
        });
      } catch {}
    }
    await writeFile(join(workspace, ".haarness", "config.toml"), config);
    http.closeAllConnections();
    await new Promise<void>((resolve) => http.close(() => resolve()));
  }
});

test("all modal surfaces dismiss on outside clicks without closing parents or submitting drafts", async () => {
  const settingsButton = page.getByRole("button", {
    name: "Settings",
    exact: true,
  });
  const settings = page.getByRole("dialog", { name: "Settings", exact: true });
  const outside = async (dialog: Locator) => {
    const box = await dialog.boundingBox();
    if (!box || box.x < 8) throw Error("Expected an exposed modal backdrop");
    return { x: box.x / 2, y: box.y + box.height / 2 };
  };
  const dismiss = async (dialog: Locator) => {
    const point = await outside(dialog);
    await page.mouse.click(point.x, point.y);
    await expect(dialog).toHaveCount(0);
  };
  const dragOutside = async (dialog: Locator, input: Locator) => {
    const box = await input.boundingBox();
    if (!box) throw Error("Missing modal input");
    const point = await outside(dialog);
    await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
    await page.mouse.down();
    await page.mouse.move(point.x, point.y, { steps: 5 });
    await page.mouse.up();
    await expect(dialog).toBeVisible();
    // A drag beginning on the backdrop and ending in the panel is not a
    // backdrop click either, even if the click target is their common parent.
    await page.mouse.move(point.x, point.y);
    await page.mouse.down();
    await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2, {
      steps: 5,
    });
    await page.mouse.up();
    await expect(dialog).toBeVisible();
  };
  const jobs = () =>
    JSON.parse(
      execFileSync(
        binary,
        [
          "--workspace",
          workspace,
          "--trust-project",
          "schedule",
          "list",
          "--json",
        ],
        { env, encoding: "utf8", timeout: 15000 },
      ),
    ) as { id: string; name: string }[];
  const originalJobs = jobs();
  const account = page.getByRole("dialog", { name: "Haarness account", exact: true });
  await page.getByRole("button", { name: "Sign in to Haarness", exact: true }).click();
  await account.getByRole("heading", { name: "Haarness account", exact: true }).click();
  await expect(account).toBeVisible();
  await dismiss(account);
  await settingsButton.click();
  await settings.locator("h2").first().click();
  await expect(settings).toBeVisible();
  await dragOutside(
    settings,
    settings.getByRole("textbox", { name: "Filter settings" }),
  );
  await dismiss(settings);

  await settingsButton.click();
  await settings
    .getByRole("button", { name: "Schedules", exact: true })
    .click();
  const create = settings.getByRole("button", {
    name: "Create schedule",
    exact: true,
  });
  const schedule = page.getByRole("dialog", {
    name: "Create schedule",
    exact: true,
  });
  await create.click();
  await schedule
    .getByLabel("Name", { exact: true })
    .fill("Unsubmitted schedule draft");
  await schedule
    .getByLabel("Prompt", { exact: true })
    .fill("This draft should not be saved.");
  await selectValue(
    schedule.getByRole("combobox", { name: "Frequency", exact: true }),
    "weekly",
  );
  await expect(schedule).toBeVisible();
  // Native dialog padding is part of the panel, not its backdrop.
  await schedule.click({ position: { x: 5, y: 30 } });
  await expect(schedule).toBeVisible();
  await dragOutside(schedule, schedule.getByLabel("Prompt", { exact: true }));
  await dismiss(schedule);
  await expect(settings).toBeVisible();
  expect(jobs()).toEqual(originalJobs);
  await expect(create).toBeFocused();

  await create.click();
  await expect(schedule.getByLabel("Name", { exact: true })).toHaveValue("");
  await schedule
    .getByLabel("Name", { exact: true })
    .fill("Modal dismissal fixture");
  await schedule
    .getByLabel("Prompt", { exact: true })
    .fill("Disabled fixture for modal verification.");
  await schedule.getByRole("button", { name: "Save", exact: true }).click();
  await expect(schedule).toHaveCount(0);
  const card = settings
    .locator(".schedule-card")
    .filter({ hasText: "Modal dismissal fixture" });
  await expect(card).toBeVisible();
  await card.getByRole("button", { name: "Edit", exact: true }).click();
  const edit = page.getByRole("dialog", { name: "Edit schedule", exact: true });
  await edit.getByLabel("Name", { exact: true }).fill("Unsubmitted edit");
  await dismiss(edit);
  await expect(card).toBeVisible();
  expect(jobs().some((job) => job.name === "Unsubmitted edit")).toBe(false);
  await card.getByRole("button", { name: "Remove", exact: true }).click();
  const confirmation = page.getByRole("dialog", {
    name: "Remove schedule?",
    exact: true,
  });
  await dismiss(confirmation);
  await expect(card).toBeVisible();
  await expect(settings).toBeVisible();
  await card.getByRole("button", { name: "Remove", exact: true }).click();
  await confirmation
    .getByRole("button", { name: "Remove schedule", exact: true })
    .click();
  await expect(card).toHaveCount(0);
  expect(jobs()).toEqual(originalJobs);

  await settings
    .getByRole("button", { name: "Providers", exact: true })
    .click();
  const addProvider = settings.getByRole("button", {
    name: "Add custom provider",
    exact: true,
  });
  await addProvider.click();
  const form = page.getByRole("dialog", {
    name: "Custom provider",
    exact: true,
  });
  await form
    .getByLabel("Provider ID", { exact: true })
    .fill("unsubmitted-provider");
  await dragOutside(form, form.getByLabel("Provider ID", { exact: true }));
  await dismiss(form);
  await expect(settings).toBeVisible();
  await expect(addProvider).toBeFocused();
  expect(
    (await client.providers()).some(
      (provider) => provider.id === "unsubmitted-provider",
    ),
  ).toBe(false);
  await settings.getByRole("button", { name: "Connect a provider", exact: true }).click();
  const connect = page.getByRole("dialog", { name: "Connect a provider", exact: true });
  await connect.getByRole("button", { name: "Use Haarness Cloud", exact: true }).click();
  await dismiss(account);
  await expect(connect).toBeVisible();
  await dismiss(connect);
  await expect(settings).toBeVisible();

  await settings.getByRole("button", { name: "Connect Haarness Router", exact: true }).click();
  await dismiss(page.getByRole("dialog", { name: "Connect Haarness Router", exact: true }));
  await expect(settings).toBeVisible();
  await settings.getByRole("button", { name: "MCP servers", exact: true }).click();
  await settings.getByRole("button", { name: "Add MCP server", exact: true }).click();
  await dismiss(page.getByRole("dialog", { name: "Add MCP server", exact: true }));
  await expect(settings).toBeVisible();
  await settings.getByRole("button", { name: "Servers", exact: true }).click();
  await settings.getByRole("button", { name: "Add remote server", exact: true }).click();
  const server = page.getByRole("dialog", { name: "Connect to server", exact: true });
  await server.getByLabel("Server URL", { exact: true }).fill("http://127.0.0.1:1");
  await dismiss(server);
  await expect(settings).toBeVisible();
  await dismiss(settings);

  await newSession();
  await send("/memory");
  const memory = page.getByRole("dialog", { name: "Memory", exact: true });
  await memory.getByRole("button", { name: "Add memory", exact: true }).click();
  const memoryEditor = page.getByRole("dialog", { name: "Add memory", exact: true });
  await memoryEditor.getByLabel("Memory name", { exact: true }).fill("Unsubmitted memory");
  await dismiss(memoryEditor);
  await expect(memory).toBeVisible();
  await expect(memory).not.toContainText("Unsubmitted memory");
  await dismiss(memory);

  const searchButton = page.getByRole("button", {
    name: "Search",
    exact: true,
  });
  await searchButton.click();
  const palette = page.getByRole("dialog", {
    name: "Universal search",
    exact: true,
  });
  const query = palette.getByRole("combobox", {
    name: "Search Haarness",
    exact: true,
  });
  await query.fill("session");
  await dragOutside(palette, query);
  await dismiss(palette);
  await expect(searchButton).toBeFocused();
  await searchButton.click();
  await query.press("Escape");
  await expect(palette).toHaveCount(0);
  expect(errors).toEqual([]);
});

test("schedule pickers, native execution and grouped conversations work end to end", async () => {
  test.setTimeout(180000);
  const received: { model: string; effort?: string }[] = [];
  const provider = createServer(async (request, response) => {
    if (request.method === "GET") {
      response.writeHead(200, { "Content-Type": "application/json" });
      response.end(
        JSON.stringify({
          data: ["schedule-preferred", "schedule-other"].map((id) => ({
            id,
            supported_parameters: ["reasoning_effort"],
          })),
        }),
      );
      return;
    }
    const chunks: Buffer[] = [];
    for await (const chunk of request) chunks.push(Buffer.from(chunk));
    const body = JSON.parse(Buffer.concat(chunks).toString());
    received.push({ model: body.model, effort: body.reasoning_effort });
    response.writeHead(200, { "Content-Type": "text/event-stream" });
    response.end(
      "data: " +
        JSON.stringify({
          choices: [
            {
              index: 0,
              delta: { content: "schedule-execution-proof" },
              finish_reason: null,
            },
          ],
        }) +
        "\n\ndata: " +
        JSON.stringify({
          choices: [{ index: 0, delta: {}, finish_reason: "stop" }],
        }) +
        "\n\ndata: [DONE]\n\n",
    );
  });
  await new Promise<void>((resolve) =>
    provider.listen(0, "127.0.0.1", resolve),
  );
  const address = provider.address();
  if (!address || typeof address === "string")
    throw Error("No provider fixture");
  const directory = join(temporary, "schedule-project");
  await mkdir(join(directory, ".haarness/agents"), { recursive: true });
  await writeFile(
    join(directory, ".haarness/config.toml"),
    `[defaults]\nprovider="schedule-model"\nmodel="schedule-preferred"\n[providers.schedule-model]\nname="Schedule fixture"\nprotocol="openai-chat-completions"\nbase_url="http://127.0.0.1:${address.port}/v1"\nrequires_auth=false\ntools=true\nreasoning=true\n`,
  );
  await writeFile(
    join(directory, ".haarness/agents/schedule-reviewer.md"),
    "---\ndescription: Scheduled review\nmode: primary\nmodel: schedule-model/schedule-other\n---\nReview the project.\n",
  );
  extraWorkspaces.push(directory);
  const command = (args: string[]) =>
    execFileSync(
      binary,
      ["--workspace", directory, "--trust-project", "schedule", ...args],
      { env, encoding: "utf8", timeout: 15000 },
    );
  type Job = {
    id: string;
    enabled: boolean;
    trigger: {
      type: string;
      seconds?: number;
      expression?: string;
      at?: string;
    };
    model: string;
    agent: string;
    variant?: string;
  };
  const jobs = () => JSON.parse(command(["list", "--json"])) as Job[];
  let id = "";
  try {
    await app.evaluate(({ dialog }, directory) => {
      const original = dialog.showOpenDialog;
      dialog.showOpenDialog = async () => {
        dialog.showOpenDialog = original;
        return { canceled: false, filePaths: [directory] };
      };
    }, directory);
    await page
      .getByRole("button", { name: "Open another project", exact: true })
      .click();
    await expect(page.locator(".application")).toHaveAttribute(
      "data-project-ready",
      "true",
    );
    const connection = "local:" + directory;
    await page.evaluate(async (connection) => {
      await window.haarness.api(connection, "createThread", [
        {
          title: "My ordinary conversation",
          provider: "echo",
          model: "echo-1",
        },
      ]);
    }, connection);
    await page.getByRole("button", { name: "Settings", exact: true }).click();
    const settings = page.getByRole("dialog", {
      name: "Settings",
      exact: true,
    });
    await settings
      .getByRole("button", { name: "Schedules", exact: true })
      .click();
    await settings
      .getByRole("button", { name: "Create schedule", exact: true })
      .click();
    const dialog = page.locator("dialog.schedule-dialog");
    await dialog
      .getByLabel("Name", { exact: true })
      .fill("Daily schedule fixture");
    await dialog
      .getByLabel("Prompt", { exact: true })
      .fill("Report scheduled fixture status.");
    await expect(
      dialog.getByRole("combobox", { name: "Frequency", exact: true }),
    ).toHaveAttribute("data-value", "daily");
    await expect(
      dialog.getByRole("combobox", { name: "Model", exact: true }),
    ).toHaveAttribute("data-value", "schedule-model/schedule-preferred");
    await selectValue(
      dialog.getByRole("combobox", { name: "Agent", exact: true }),
      "schedule-reviewer",
    );
    await expect(
      dialog.getByRole("combobox", { name: "Model", exact: true }),
    ).toHaveAttribute("data-value", "schedule-model/schedule-other");
    await selectEffort(
      dialog.getByRole("combobox", { name: "Model", exact: true }),
      "high",
    );
    await dialog.getByLabel("Time", { exact: true }).fill("09:30");
    await page.screenshot({
      path: test.info().outputPath("schedule-form.png"),
    });
    await dialog.getByRole("button", { name: "Save", exact: true }).click();
    const card = settings
      .locator(".schedule-card")
      .filter({ hasText: "Daily schedule fixture" });
    await expect(card).toContainText("Every day at 09:30");
    id = jobs().find(
      (job) => job.model === "schedule-model/schedule-other",
    )!.id;
    expect(jobs().find((job) => job.id === id)).toMatchObject({
      enabled: false,
      trigger: { type: "cron", expression: "30 9 * * *" },
      agent: "schedule-reviewer",
      variant: "high",
    });
    await expect(settings.locator(".command-output")).toHaveCount(0);
    await expect(
      card.getByRole("button", { name: /History|Logs/ }),
    ).toHaveCount(0);
    for (const [frequency, expression] of [
      ["weekdays", "30 9 * * 1-5"],
      ["weekly", "30 9 * * 2"],
    ]) {
      await card.getByRole("button", { name: "Edit", exact: true }).click();
      await selectValue(
        dialog.getByRole("combobox", { name: "Frequency", exact: true }),
        frequency,
      );
      if (frequency === "weekly")
        await selectValue(
          dialog.getByRole("combobox", { name: "Day", exact: true }),
          "2",
        );
      await dialog.getByRole("button", { name: "Save", exact: true }).click();
      await expect(dialog).toHaveCount(0);
      expect(jobs().find((job) => job.id === id)!.trigger.expression).toBe(
        expression,
      );
    }
    await card.getByRole("button", { name: "Edit", exact: true }).click();
    await expect(
      dialog.getByRole("combobox", { name: "Day", exact: true }),
    ).toHaveAttribute("data-value", "2");
    await selectValue(
      dialog.getByRole("combobox", { name: "Frequency", exact: true }),
      "interval",
    );
    await dialog.getByLabel("Every", { exact: true }).fill("2");
    await selectValue(
      dialog.getByRole("combobox", { name: "Interval unit", exact: true }),
      "3600",
    );
    await selectEffort(
      dialog.getByRole("combobox", { name: "Model", exact: true }),
      "",
    );
    await dialog.getByRole("button", { name: "Save", exact: true }).click();
    await expect(card).toContainText("Every 2 hours");
    expect(jobs().find((job) => job.id === id)).toMatchObject({
      trigger: { type: "interval", seconds: 7200 },
    });
    expect(jobs().find((job) => job.id === id)!.variant).toBeUndefined();
    await card.getByRole("button", { name: "Edit", exact: true }).click();
    await selectValue(
      dialog.getByRole("combobox", { name: "Frequency", exact: true }),
      "cron",
    );
    await dialog.getByLabel("Cron expression").fill("not a cron");
    await dialog.getByRole("button", { name: "Save", exact: true }).click();
    await expect(dialog.getByRole("alert")).toContainText("5 fields");
    await dialog.getByLabel("Cron expression").fill("*/15 9-17 * * 1-5");
    await dialog.getByRole("button", { name: "Save", exact: true }).click();
    await expect(dialog).toHaveCount(0);
    await card.getByRole("button", { name: "Edit", exact: true }).click();
    await expect(
      dialog.getByRole("combobox", { name: "Frequency", exact: true }),
    ).toHaveAttribute("data-value", "cron");
    await expect(dialog.getByLabel("Cron expression")).toHaveValue(
      "*/15 9-17 * * 1-5",
    );
    await selectValue(
      dialog.getByRole("combobox", { name: "Frequency", exact: true }),
      "once",
    );
    await dialog.getByLabel("Date and time").fill("2000-01-01T09:00");
    await dialog.getByRole("button", { name: "Save", exact: true }).click();
    await expect(dialog.getByRole("alert")).toContainText("one minute");
    const future = new Date(Date.now() + 86400000);
    const at = new Date(future.getTime() - future.getTimezoneOffset() * 60000)
      .toISOString()
      .slice(0, 16);
    await dialog.getByLabel("Date and time").fill(at);
    await selectEffort(
      dialog.getByRole("combobox", { name: "Model", exact: true }),
      "high",
    );
    await dialog.getByRole("switch", { name: "Enable schedule" }).check();
    await dialog.getByRole("button", { name: "Save", exact: true }).click();
    await expect(card).toContainText("Enabled");
    expect(jobs().find((job) => job.id === id)!.trigger.at).toBe(
      new Date(at).toISOString().replace(".000Z", "Z"),
    );
    // Start the installed job the way its system scheduler would.
    let source = "launchd";
    if (process.platform === "darwin") {
      const label = `gui/${process.getuid!()}/dev.haarness.schedule.${id}`;
      execFileSync("/bin/launchctl", ["print", label], { stdio: "ignore" });
      execFileSync("/bin/launchctl", ["kickstart", label]);
    } else if (process.platform === "linux") {
      source = "systemd-user";
      execFileSync("systemctl", ["--user", "is-enabled", `haarness-schedule-${id}.timer`], { stdio: "ignore" });
      execFileSync("systemctl", ["--user", "start", "--no-block", `haarness-schedule-${id}.service`]);
    } else {
      source = "windows-task";
      execFileSync("schtasks.exe", ["/Run", "/TN", `Haarness\\Schedule\\${id}`], { stdio: "ignore" });
    }
    await expect
      .poll(() => JSON.parse(command(["history", id, "--json"])), {
        timeout: 30000,
      })
      .toMatchObject([{ status: "success", source }]);
    expect(received.at(-1)).toEqual({
      model: "schedule-other",
      effort: "high",
    });
    await expect
      .poll(() => jobs().find((job) => job.id === id)!.enabled)
      .toBe(false);
    const origin = await page.evaluate(
      async ({ connection, id }) => {
        const threads = await window.haarness.api<
          { schedule?: { id: string; name: string } }[]
        >(connection, "listThreads", []);
        return threads.find((thread) => thread.schedule?.id === id)?.schedule;
      },
      { connection, id },
    );
    expect(origin).toEqual({ id, name: "Daily schedule fixture" });
    // A second real run remains in the same group even if its schedule was renamed.
    command([
      "update",
      id,
      "--name",
      "Renamed schedule fixture",
      "--every",
      "2h",
      "--disable",
    ]);
    await settings.getByRole("button", { name: "Close settings" }).click();
    await page.getByRole("button", { name: "Settings", exact: true }).click();
    await settings
      .getByRole("button", { name: "Schedules", exact: true })
      .click();
    const renamed = settings
      .locator(".schedule-card")
      .filter({ hasText: "Renamed schedule fixture" });
    await renamed.getByRole("button", { name: "Run now", exact: true }).click();
    await expect.poll(() => received.length).toBe(2);
    await expect(
      renamed.getByRole("button", { name: "Run now", exact: true }),
    ).toBeEnabled();
    const runs = JSON.parse(command(["history", id, "--json"])) as {
      threadId: string;
    }[];
    await settings.getByRole("button", { name: "Close settings" }).click();
    const project = page.getByRole("region", {
      name: "Project schedule-project",
      exact: true,
    });
    const heading = project.getByRole("button", {
      name: "schedule-project",
      exact: true,
    });
    if ((await heading.getAttribute("aria-expanded")) !== "true")
      await heading.click();
    await expect(project.locator(".sessions > .session-row")).toHaveCount(1);
    await project
      .getByRole("button", { name: "Scheduled", exact: false })
      .click();
    await expect(project.locator(".schedule-group")).toHaveCount(1);
    const group = project.getByRole("region", {
      name: "Schedule Renamed schedule fixture",
      exact: true,
    });
    await group.locator(".schedule-group-heading").click();
    await expect(group.locator(".session-row")).toHaveCount(2);
    await group.locator(".session-row > button:first-child").last().click();
    await expect(page.locator(".transcript")).toContainText(
      "schedule-execution-proof",
    );
    await page.evaluate(
      async ({ connection, thread }) => {
        await window.haarness.api(connection, "updateThread", [
          thread,
          { title: "A renamed scheduled conversation" },
        ]);
      },
      { connection, thread: runs[1].threadId },
    );
    await expect(group.locator(".session-row")).toHaveCount(2);
    await expect(group.locator(".schedule-group-heading")).toHaveAttribute(
      "aria-expanded",
      "true",
    );
    await page.reload();
    await expect(page.locator(".application")).toHaveAttribute(
      "data-project-ready",
      "true",
    );
    await expect(group.locator(".session-row")).toHaveCount(2);
    await page.screenshot({
      path: test.info().outputPath("scheduled-conversations.png"),
    });
    const fork = await page.evaluate(
      async ({ connection, thread }) =>
        window.haarness.api<{ id: string; schedule?: unknown }>(
          connection,
          "forkThread",
          [thread, { title: "Independent follow-up" }],
        ),
      { connection, thread: runs[1].threadId },
    );
    expect(fork.schedule).toBeUndefined();
    await expect(project.locator(".sessions > .session-row")).toHaveCount(2);
    await page.evaluate(
      async ({ connection, id }) => {
        await window.haarness.api(connection, "updateThread", [
          id,
          { archived: true },
        ]);
      },
      { connection, id: fork.id },
    );
    await expect(project.locator(".sessions > .session-row")).toHaveCount(1);

    await group
      .getByRole("button", {
        name: "Archive chat A renamed scheduled conversation",
        exact: true,
      })
      .click();
    await expect(group.locator(".session-row")).toHaveCount(1);
    await page.getByRole("button", { name: "Settings", exact: true }).click();
    await settings
      .getByRole("button", { name: "Schedules", exact: true })
      .click();
    await renamed.getByRole("button", { name: "Remove", exact: true }).click();
    await page
      .locator("dialog:not(.settings-dialog):not(.palette-dialog)")
      .getByRole("button", { name: "Remove schedule", exact: true })
      .click();
    await expect(renamed).toHaveCount(0);
    expect(jobs().some((job) => job.id === id)).toBe(false);
    await settings.getByRole("button", { name: "Close settings" }).click();
    // Removing the definition preserves the remaining run conversation.
    await expect(group.locator(".session-row")).toHaveCount(1);
    expect(errors).toEqual([]);
  } finally {
    if (id && jobs().some((job) => job.id === id)) command(["remove", id]);
    await new Promise<void>((resolve) => provider.close(() => resolve()));
    if (
      await page
        .getByRole("dialog", { name: "Settings", exact: true })
        .isVisible()
    )
      await page.getByRole("button", { name: "Close settings" }).click();
    const project = page.getByRole("region", {
      name: "Project workspace",
      exact: true,
    });
    await project
      .getByRole("button", { name: "workspace", exact: true })
      .click();
    await expect(page.locator(".application")).toHaveAttribute(
      "data-project-ready",
      "true",
    );
    const heading = project.getByRole("button", {
      name: "workspace",
      exact: true,
    });
    if ((await heading.getAttribute("aria-expanded")) !== "true")
      await heading.click();
    await newSession();
  }
});

test("scheduled sidebar groups paginate and retain expansion through live updates", async () => {
  const id = crypto.randomUUID();
  const created: string[] = [];
  const project = page.getByRole("region", {
    name: "Project workspace",
    exact: true,
  });
  const heading = project.getByRole("button", {
    name: "workspace",
    exact: true,
  });
  if ((await heading.getAttribute("aria-expanded")) !== "true")
    await heading.click();
  try {
    for (let index = 0; index < 12; index++) {
      const thread = await client.createThread({
        title: `Scheduled fixture ${index}`,
        provider: "echo",
        model: "echo-1",
        schedule: { id, name: "Sidebar schedule fixture" },
      });
      created.push(thread.id);
    }
    const scheduled = project.getByRole("region", {
      name: "Scheduled chats",
      exact: true,
    });
    const toggle = scheduled.locator(".scheduled-heading");
    if ((await toggle.getAttribute("aria-expanded")) !== "true")
      await toggle.click();
    const group = scheduled.getByRole("region", {
      name: "Schedule Sidebar schedule fixture",
      exact: true,
    });
    await group.locator(".schedule-group-heading").click();
    await expect(group.locator(".session-row")).toHaveCount(10);
    await group
      .getByRole("button", { name: "Show more chats", exact: true })
      .click();
    await expect(group.locator(".session-row")).toHaveCount(12);
    await client.updateThread(created[0], {
      title: "Scheduled conversation with a custom name",
    });
    await expect(group.locator(".session-row")).toHaveCount(12);
    const scheduleFile = await fileTransfer("scheduled-drop.png");
    await group.locator(".session-row").first().dispatchEvent("drop", { dataTransfer: scheduleFile });
    await expect(page.getByRole("button", { name: "Remove scheduled-drop.png", exact: true })).toBeVisible();
    await page.getByRole("button", { name: "Remove scheduled-drop.png", exact: true }).click();
    await scheduleFile.dispose();
    await expect(
      page
        .locator(".session-bar-name, .workspace-tab.active")
        .filter({ hasText: "Scheduled fixture" })
        .first(),
    ).toBeVisible();
    await group.locator(".schedule-group-heading").click();
    await client.updateThread(created[1], { title: "Another custom name" });
    await expect(group.locator(".schedule-group-heading")).toHaveAttribute(
      "aria-expanded",
      "false",
    );
    await expect(group.locator(".session-row")).toHaveCount(0);
    await group.locator(".schedule-group-heading").click();
    await expect(group.locator(".session-row")).toHaveCount(12);
    await group
      .getByRole("button", {
        name: "Archive chat Scheduled conversation with a custom name",
        exact: true,
      })
      .click();
    await expect(group.locator(".session-row")).toHaveCount(11);
    await page.reload();
    await expect(page.locator(".application")).toHaveAttribute(
      "data-project-ready",
      "true",
    );
    await expect(group.locator(".schedule-group-heading")).toHaveAttribute(
      "aria-expanded",
      "true",
    );
    await expect(group.locator(".session-row")).toHaveCount(10);
    expect(errors).toEqual([]);
  } finally {
    for (const id of created) await client.updateThread(id, { archived: true });
    await newSession();
  }
});

test("workspace panes stay beside chat, resize, preserve drafts and adapt to a small window", async () => {
  await newSession();
  await send("A workspace beside my conversation");
  await expect(page.locator(".transcript")).toContainText(
    "Echo: A workspace beside my conversation",
  );
  await openProjectPanel("Files");
  await page.locator(".file-tree").getByRole("button", { name: "README.md", exact: true }).click();
  await page
    .locator(".file-preview .cm-content")
    .fill("A draft kept while reviewing");
  await expect(
    page.getByRole("textbox", { name: "Message", exact: true }),
  ).toBeVisible();
  const chat = await page.locator(".chat-pane").boundingBox();
  const files = await page.locator(".side-panel").boundingBox();
  expect(chat!.x + chat!.width).toBeLessThanOrEqual(files!.x);
  const separator = page.getByRole("separator", { name: "Resize side panel" });
  const before = Number(await separator.getAttribute("aria-valuenow"));
  await separator.press("ArrowLeft");
  await expect(separator).toHaveAttribute("aria-valuenow", String(before + 20));
  const handle = await separator.boundingBox();
  await page.mouse.move(handle!.x, handle!.y + 80);
  await page.mouse.down();
  await page.mouse.move(handle!.x - 30, handle!.y + 80, { steps: 5 });
  await page.mouse.up();
  await expect(separator).toHaveAttribute("aria-valuenow", String(before + 50));
  await page
    .getByRole("separator", { name: "Resize sidebar" })
    .press("ArrowRight");
  await expect(page.locator(".sidebar")).toHaveCSS("width", "278px");
  await openProjectPanel("Review");
  await expect(page.locator(".review")).toBeVisible();
  await expect(page.locator(".transcript")).toContainText(
    "A workspace beside my conversation",
  );
  await page
    .getByRole("button", { name: "Show Files panel", exact: true })
    .click();
  await expect(page.locator(".file-preview .cm-content")).toContainText(
    "A draft kept while reviewing",
  );
  await page
    .getByRole("button", { name: "Expand side panel", exact: true })
    .click();
  await expect(page.locator(".chat-pane")).toBeHidden();
  await page
    .getByRole("button", { name: "Restore side panel", exact: true })
    .click();
  await expect(
    page.getByRole("textbox", { name: "Message", exact: true }),
  ).toBeVisible();
  await page.getByRole("button", { name: "Hide sidebar", exact: true }).click();
  await expect(page.locator(".sidebar")).toBeHidden();
  await page.getByRole("button", { name: "Show sidebar", exact: true }).click();
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect
    .poll(() => readFile(join(workspace, "README.md"), "utf8"))
    .toBe("A draft kept while reviewing");
  await page
    .getByRole("button", { name: "Toggle terminal", exact: true })
    .click();
  await page.getByRole("button", { name: "New terminal", exact: true }).click();
  await expect(page.locator(".xterm-screen")).toBeVisible();
  await page
    .getByRole("separator", { name: "Resize terminal" })
    .press("ArrowUp");
  await expect(page.locator(".terminal-dock")).toHaveCSS("height", "280px");
  await page.screenshot({ path: "test-results/workbench.png" });
  await page
    .getByRole("button", { name: "Hide terminal", exact: true })
    .click();
  await page.setViewportSize({ width: 900, height: 760 });
  await expect(page.locator(".side-panel")).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(
    900,
  );
  await page.screenshot({ path: "test-results/workbench-narrow.png" });
  await page
    .getByRole("button", { name: "Hide side panel", exact: true })
    .click();
  await expect(
    page.getByRole("textbox", { name: "Message", exact: true }),
  ).toBeVisible();
  await page.setViewportSize({ width: 1420, height: 940 });
  await expect
    .poll(() => page.evaluate(() => window.haarness.storage("layout:v2")))
    .toMatchObject({
      sidebarWidth: 278,
      dockWidth: before + 50,
      terminalHeight: 280,
      dock: null,
    });
  await page.reload();
  await page.getByRole("region", { name: "Project workspace", exact: true }).locator(".project-heading").click();
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
  );
  await expect(page.locator(".sidebar")).toHaveCSS("width", "278px");
  await openProjectPanel("Files");
  await expect(
    page.getByRole("separator", { name: "Resize side panel" }),
  ).toHaveAttribute("aria-valuenow", String(before + 50));
  await page
    .getByRole("separator", { name: "Resize side panel" })
    .press("Home");
  await page.getByRole("separator", { name: "Resize sidebar" }).press("Home");
  expect(errors).toEqual([]);
});

test("all themes support Light, Dark and Auto and persist across reloads", async () => {
  await openProjectPanel("Files");
  await page.getByRole("button", { name: "src", exact: true }).click();
  await page.getByRole("button", { name: "greeting.ts", exact: true }).click();
  await page
    .getByRole("button", { name: "Toggle terminal", exact: true })
    .click();
  await page.getByRole("button", { name: "New terminal", exact: true }).click();
  await expect(page.locator(".xterm-screen")).toBeVisible();
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  const settings = page.getByRole("dialog", { name: "Settings", exact: true });
  const appearance = settings.getByRole("combobox", {
    name: "Appearance",
    exact: true,
  });
  for (const mode of ["light", "dark"] as const) {
    await selectValue(appearance, mode);
    for (const theme of themes) {
      await settings
        .getByRole("button", { name: theme.name + " theme", exact: true })
        .click();
      await expect(page.locator("html")).toHaveAttribute(
        "data-palette",
        theme.id,
      );
      await expect(page.locator("html")).toHaveAttribute("data-theme", mode);
      await expect(
        settings.getByRole("button", {
          name: theme.name + " theme",
          exact: true,
        }),
      ).toHaveAttribute("aria-pressed", "true");
      const rgb = (hex: string) =>
        "rgb(" +
        [1, 3, 5]
          .map((index) => parseInt(hex.slice(index, index + 2), 16))
          .join(", ") +
        ")";
      await expect(page.locator(".file-preview .cm-gutters")).toHaveCSS(
        "background-color",
        rgb(themeColors(theme.id, mode).page),
      );
      await expect(page.locator(".xterm-scrollable-element")).toHaveCSS(
        "background-color",
        rgb(themeColors(theme.id, mode).page),
      );
      const keyword = page
        .locator(".file-preview .cm-line span")
        .filter({ hasText: /^export$/ })
        .first();
      await expect(keyword).toHaveCSS(
        "color",
        rgb(themeColors(theme.id, mode)["syntax-keyword"]),
      );
    }
  }
  await settings
    .getByRole("button", { name: "Ocean theme", exact: true })
    .click();
  await selectValue(appearance, "system");
  await page.emulateMedia({ colorScheme: "light" });
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  await page.screenshot({ path: "test-results/themes-auto-light.png" });
  await page.emulateMedia({ colorScheme: "dark" });
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await page.screenshot({ path: "test-results/themes-auto-dark.png" });
  await expect
    .poll(() =>
      page.evaluate(async () => (await window.haarness.boot()).preferences),
    )
    .toMatchObject({ theme: "system", palette: "ocean" });
  await settings
    .getByRole("button", { name: "Close settings", exact: true })
    .click();
  await page.screenshot({ path: "test-results/workbench-ocean.png" });
  await page.reload();
  await expect(page.locator("html")).toHaveAttribute("data-palette", "ocean");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await page.emulateMedia({ colorScheme: "light" });
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  await page.getByRole("region", { name: "Project workspace", exact: true }).locator(".project-heading").click();
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
  );
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  await settings
    .getByRole("button", { name: "Haarness theme", exact: true })
    .click();
  await selectValue(appearance, "dark");
  await settings
    .getByRole("button", { name: "Close settings", exact: true })
    .click();
  await page
    .getByRole("button", { name: "Hide side panel", exact: true })
    .click();
  expect(errors).toEqual([]);
});

test("minimal navigation and long slash commands stay usable without shifting chat", async () => {
  await newSession();
  // A preceding pane scenario can leave Files open. Narrow windows display
  // that pane over chat, so start this chat-layout scenario with it closed.
  const hidePanel = page.getByRole("button", { name: "Hide side panel", exact: true });
  if (await hidePanel.isVisible()) await hidePanel.click();
  await expect(page.locator(".conversation > header")).toHaveCount(0);
  for (const name of ["Compact", "Undo", "Redo", "Export conversation"]) {
    await expect(
      page.locator(".conversation").getByRole("button", { name, exact: true }),
    ).toHaveCount(0);
  }
  const navigation = page.getByRole("navigation", { name: "Main navigation" });
  await expect(navigation.getByRole("button")).toHaveCount(1);
  for (const name of [
    "Teams",
    "Workflows",
    "Goals",
    "Workspaces",
    "Usage & insights",
    "Plugins",
    "Schedules",
    "Haarness menu",
  ]) {
    await expect(
      page.locator(".sidebar").getByRole("button", { name, exact: true }),
    ).toHaveCount(0);
  }
  const message = page.getByRole("textbox", { name: "Message", exact: true });
  // Real assistant output with a long unbroken token must not widen the flex column either.
  await send("Layout proof " + "long-content-".repeat(70));
  await expect(page.locator(".transcript")).toContainText("Echo: Layout proof");
  await expect(page.getByRole("button", { name: "Stop response" })).toHaveCount(
    0,
  );
  for (const width of [1420, 1100, 900]) {
    await page.setViewportSize({ width, height: 940 });
    const before = await page.locator(".composer").boundingBox();
    await message.fill("/ui-layout-");
    const suggestions = page.getByRole("listbox", { name: "Slash commands" });
    await expect(suggestions.getByRole("option")).toHaveCount(2);
    const popup = await suggestions.boundingBox();
    const composer = await page.locator(".composer").boundingBox();
    expect(Math.abs(composer!.x - before!.x)).toBeLessThan(1);
    expect(Math.abs(composer!.width - before!.width)).toBeLessThan(1);
    expect(popup!.x).toBeGreaterThanOrEqual(composer!.x - 1);
    expect(popup!.x + popup!.width).toBeLessThanOrEqual(
      composer!.x + composer!.width + 1,
    );
    expect(popup!.y + popup!.height).toBeLessThanOrEqual(composer!.y);
    expect(
      await suggestions.evaluate(
        (element) => element.scrollWidth <= element.clientWidth,
      ),
    ).toBe(true);
    expect(
      await page.evaluate(() => document.documentElement.scrollWidth),
    ).toBe(width);
    await page.screenshot({
      path: "test-results/slash-commands-" + width + ".png",
    });
    await message.press("ArrowDown");
    await message.press("Tab");
    await expect(message).toHaveValue("/ui-layout-short ");
    await expect(suggestions).toHaveCount(0);
    await message.fill("/ui-layout-");
    await message.press("Escape");
    await expect(suggestions).toHaveCount(0);
    await expect(message).toHaveValue("/ui-layout-");
    await message.fill("");
  }
  await page.setViewportSize({ width: 1420, height: 940 });
  await openProjectPanel("Files");
  await message.fill("/ui-layout-");
  const popup = page.getByRole("listbox", { name: "Slash commands" });
  await expect(popup).toBeVisible();
  const bounds = await popup.boundingBox(),
    pane = await page.locator(".chat-pane").boundingBox();
  expect(bounds!.x + bounds!.width).toBeLessThanOrEqual(pane!.x + pane!.width);
  await message.press("Enter");
  await expect(message).toHaveValue("/" + longCommand + " ");
  await expect(popup).toHaveCount(0);
  await message.fill("/ui-layout-short");
  await popup.getByRole("option").click();
  await expect(message).toHaveValue("/ui-layout-short ");
  await message.fill("");
  await page
    .getByRole("button", { name: "Hide side panel", exact: true })
    .click();
  expect(errors).toEqual([]);
});

test("app rebuilds keep permission-waiting projects usable and upgrade them after work finishes", async () => {
  const directory = join(temporary, "upgrade-project");
  await mkdir(join(directory, ".haarness"), { recursive: true });
  await writeFile(
    join(directory, ".haarness/config.toml"),
    '[defaults]\nprovider="echo"\nmodel="echo-1"\n[permissions]\nbash="ask"\n',
  );
  extraWorkspaces.push(directory);
  // A separate executable really owns this daemon, reproducing a rebuilt app.
  const previousBinary = join(
    temporary,
    process.platform === "win32"
      ? "previous-haarness.exe"
      : "previous-haarness",
  );
  await copyFile(binary, previousBinary);
  const previous = JSON.parse(
    execFileSync(
      previousBinary,
      ["--workspace", directory, "--trust-project", "desktop"],
      {
        env,
        input: JSON.stringify({ action: "connect" }),
        encoding: "utf8",
        timeout: 240000,
      },
    ),
  );
  const prior = new Haarness({ baseUrl: previous.url, token: previous.token });
  try {
    const original = await prior.info();
    const paused = await prior.createThread({
      title: "Waiting for my permission",
      provider: "echo",
      model: "echo-1",
    });
    const turn = await prior.startTurn(paused.id, {
      requestId: crypto.randomUUID(),
      text: '!tool bash {"command":"printf preserved-permission-proof"}',
    });
    await expect
      .poll(async () => (await prior.listPermissions()).length)
      .toBe(1);
    await app.evaluate(({ dialog }, path) => {
      const original = dialog.showOpenDialog;
      dialog.showOpenDialog = async () => {
        dialog.showOpenDialog = original;
        return { canceled: false, filePaths: [path] };
      };
    }, directory);
    await page
      .getByRole("button", { name: "Open another project", exact: true })
      .click();
    const group = page.locator(
      '.project-group[aria-label="Project upgrade-project"]',
    );
    await expect(group.locator(".session-title")).toContainText([
      "Waiting for my permission",
    ]);
    await expect(
      page.getByRole("button", { name: "Allow once", exact: true }),
    ).toBeVisible();
    expect((await prior.info()).serverInstanceId).toBe(
      original.serverInstanceId,
    );
    await expect(page.locator(".error-banner")).toHaveCount(0);
    await expect(
      page.getByRole("button", { name: "New chat", exact: true }),
    ).toBeEnabled();
    // A deliberately selected new-chat draft also survives reopening.
    await newSession();
    await page.reload();
    await expect(page.locator(".application")).toHaveAttribute(
      "data-project-ready",
      "true",
    );
    await expect(page.locator(".conversation-empty")).toBeVisible();
    await expect(
      page.getByRole("button", { name: "Allow once", exact: true }),
    ).toHaveCount(0);
    // Another conversation and another project remain usable while this waits.
    await send("independent-conversation-proof");
    await expect(page.locator(".transcript")).toContainText(
      "Echo: independent-conversation-proof",
    );
    expect((await prior.getTurn(turn.id)).status).toBe("running");
    await expect(
      group.getByRole("button", {
        name: "Waiting for my permission",
        exact: true,
      }),
    ).toHaveAttribute("aria-description", "Needs approval");
    await page
      .locator(
        '.project-group[aria-label="Project workspace"] .project-heading',
      )
      .click();
    await newSession();
    await send("independent-project-proof");
    await expect(page.locator(".transcript")).toContainText(
      "Echo: independent-project-proof",
    );
    await group
      .getByRole("button", { name: "Waiting for my permission", exact: true })
      .click();
    await expect(
      page.getByRole("button", { name: "Allow once", exact: true }),
    ).toBeVisible();
    await page.reload();
    // Reopening restores the project and task directly; no connection screen.
    await expect(
      page.getByRole("button", { name: "Allow once", exact: true }),
    ).toBeVisible();
    await expect(group.locator(".session-title")).toContainText([
      "Waiting for my permission",
    ]);
    await expect(
      page.getByText("Loading conversations…", { exact: true }),
    ).toHaveCount(0);
    await expect(
      page.getByText("Reconnecting to project…", { exact: true }),
    ).toHaveCount(0);
    expect((await prior.info()).serverInstanceId).toBe(
      original.serverInstanceId,
    );
    await page.getByRole("button", { name: "Allow once", exact: true }).click();
    expect((await prior.waitTurn(turn.id)).status).toBe("completed");
    await expect(page.locator(".transcript")).toContainText(
      "preserved-permission-proof",
    );
    // Multiple windows/sidebar requests share one idle upgrade, preserving history.
    const instances = await page.evaluate(async (path) => {
      const views = await Promise.all(
        Array.from({ length: 4 }, () =>
          window.haarness.connect({ recent: "local:" + path }),
        ),
      );
      return views.map((view) => ({
        instance: view!.info.serverInstanceId,
        pending: view!.upgradePending,
      }));
    }, directory);
    expect(new Set(instances.map((view) => view.instance)).size).toBe(1);
    expect(instances[0].instance).not.toBe(original.serverInstanceId);
    expect(instances.every((view) => !view.pending)).toBe(true);
    await page.reload();
    // The upgraded server reloads the chat; a busy machine takes its time.
    await expect(page.locator(".transcript")).toContainText(
      "preserved-permission-proof",
      { timeout: 45_000 },
    );
    await expect(page.locator(".error-banner")).toHaveCount(0);
    await page
      .locator(
        '.project-group[aria-label="Project workspace"] .project-heading',
      )
      .click();
    await attachClient();
  } finally {
    prior.close();
  }
  expect(errors).toEqual([]);
});

test("slow project startup preserves the project index, drafts, and independent navigation", async () => {
  const directory = join(temporary, "slow-project");
  await mkdir(join(directory, ".haarness"), { recursive: true });
  await writeFile(
    join(directory, ".haarness/config.toml"),
    '[defaults]\nprovider="echo"\nmodel="echo-1"\n'.replaceAll("\\n", "\n"),
  );
  extraWorkspaces.push(directory);
  await app.evaluate(({ dialog }, path) => {
    const original = dialog.showOpenDialog;
    dialog.showOpenDialog = async () => {
      dialog.showOpenDialog = original;
      return { canceled: false, filePaths: [path] };
    };
  }, directory);
  await page
    .getByRole("button", { name: "Open another project", exact: true })
    .click();
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
  );
  const id = "local:" + directory;
  const kept = await page.evaluate(
    async (id) =>
      window.haarness.api<{ id: string }>(id, "createThread", [
        { title: "Available after restart", provider: "echo", model: "echo-1" },
      ]),
    id,
  );
  const slow = page.locator(
    '.project-group[aria-label="Project slow-project"]',
  );
  await expect(
    slow.getByRole("button", { name: "Available after restart", exact: true }),
  ).toBeVisible();
  await expect
    .poll(() =>
      page.evaluate(async (id) => {
        const prefix = "project-index:v2:" + id;
        const manifest = (await window.haarness.storage(prefix)) as {
          generation: string;
          pages: number;
        } | null;
        if (!manifest) return [];
        const values: { id: string }[] = [];
        for (let i = 0; i < manifest.pages; i++)
          values.push(
            ...(((await window.haarness.storage(
              `${prefix}:${manifest.generation}:${i}`,
            )) as { id: string }[]) ?? []),
          );
        return values.map((item) => item.id);
      }, id),
    )
    .toContain(kept.id);
  await newSession();
  const composer = page.getByRole("textbox", {
    name: "Message",
    exact: true,
  });
  await composer.fill("A draft for the slow project");
  const primary = page.locator(
    '.project-group[aria-label="Project workspace"] .project-heading',
  );
  await primary.click();
  // The slow project is still ready while the switch connects, and the
  // reload below reopens whichever project was stored last.
  await expect
    .poll(() => page.evaluate(() => window.haarness.storage("active-project")))
    .toBe("local:" + workspace);
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
  );
  const registry = JSON.parse(
    await readFile(join(temporary, "state/data/workspaces.json"), "utf8"),
  );
  const workspaceId = registry.workspaces.find(
    (entry: { path: string }) => entry.path === directory,
  ).id;
  const lease = join(
    temporary,
    "state/cache/workspaces",
    workspaceId,
    "runtime.connect.lock",
  );
  const locker = spawn(
    "python3",
    [
      "-u",
      "-c",
      "import fcntl,sys; f=open(sys.argv[1],'a'); fcntl.flock(f,fcntl.LOCK_EX); print('locked',flush=True); sys.stdin.read()",
      lease,
    ],
    { stdio: ["pipe", "pipe", "pipe"] },
  );
  try {
    await new Promise<void>((resolve, reject) => {
      locker.stdout.once("data", () => resolve());
      locker.once("error", reject);
    });
    execFileSync(binary, ["--workspace", directory, "serve", "--stop"], {
      env,
      stdio: "ignore",
      timeout: 30000,
    });
    await page.reload();
    // The saved index renders while the project's real launch lease is held.
    await expect(
      slow.getByRole("button", {
        name: "Available after restart",
        exact: true,
      }),
    ).toBeVisible();
    await expect(page.locator(".application")).toHaveAttribute(
      "data-project-ready",
      "true",
    );
    await slow.locator(".project-heading").click();
    await expect(slow.locator(".project-heading")).toHaveAttribute(
      "aria-busy",
      "true",
    );
    await expect(page.locator(".project-opening")).toHaveCount(0);
    await page.getByRole("button", { name: "New chat", exact: true }).click();
    // A pending project shows the loading view, never an editable home composer.
    await expect(page.getByRole("status", { name: "Opening project…" })).toBeVisible();
    await expect(composer).toHaveCount(0);
    await primary.click();
    await expect(page.locator(".application")).toHaveAttribute(
      "data-project-ready",
      "true",
    );
    const before = (await client.listThreads()).length;
    await newSession();
    expect((await client.listThreads()).length).toBe(before);
    await send("Work continues in my other project");
    await expect(page.locator(".transcript")).toContainText(
      "Echo: Work continues in my other project",
    );
    locker.stdin.end();
    await new Promise<void>((resolve) => locker.once("close", () => resolve()));
    await expect
      .poll(() =>
        page.evaluate(
          async (id) =>
            window.haarness
              .api<{ status: string }>(id, "health", [])
              .then((v) => v.status)
              .catch(() => "starting"),
          id,
        ),
      )
      .toBe("ok");
    await expect(page.locator(".transcript")).toContainText(
      "Echo: Work continues in my other project",
    );
    await slow.locator(".project-heading").click();
    await expect(page.locator(".application")).toHaveAttribute(
      "data-project-ready",
      "true",
    );
    await newSession();
    await expect(composer).toHaveValue("A draft for the slow project");
    await page.screenshot({ path: "test-results/project-draft.png" });
    await page
      .getByRole("button", { name: "Send message", exact: true })
      .click();
    await expect(page.locator(".transcript")).toContainText(
      "Echo: A draft for the slow project",
    );
    await primary.click();
    await expect(page.locator(".transcript")).toContainText(
      "Echo: Work continues in my other project",
    );
    await expect(page.locator(".error-banner")).toHaveCount(0);
  } finally {
    locker.stdin.end();
    if (locker.exitCode === null) locker.kill();
  }
  expect(errors).toEqual([]);
});

test("one project groups worktree conversations while drafts, permissions, files and new chats use the chosen folder", async () => {
  const primary = page.locator(
    '.project-group[aria-label="Project workspace"]',
  );
  await primary.locator(".project-heading").click();
  if (
    (await primary
      .locator(".project-heading")
      .getAttribute("aria-expanded")) === "false"
  )
    await primary
      .getByRole("button", { name: "Expand project workspace", exact: true })
      .click();
  await newSession();
  const composer = page.getByRole("textbox", { name: "Message", exact: true });
  await composer.fill("A draft shared between working locations");
  const count = (await client.listThreads()).length;
  // The folder the chat works in is the menu's live choice. Picking it again
  // leaves the project as it is: before, the project was opened anew, its
  // chats left the sidebar and came back, and unsaved file edits were asked
  // about.
  await page.evaluate(() => {
    const seen = ((window as any).repicked = { reopened: false });
    const root = document.querySelector(".application")!;
    new MutationObserver(() => {
      if (root.getAttribute("data-project-ready") !== "true") seen.reopened = true;
    }).observe(root, { attributes: true, attributeFilter: ["data-project-ready"] });
  });
  await page
    .getByRole("button", { name: "Choose working location", exact: true })
    .click();
  const current = page.getByRole("menuitemradio", { name: "Project folder", exact: true });
  await expect(current).toHaveAttribute("aria-checked", "true");
  // Ticked at its trailing end, as a picker's option is; the labels stay
  // beside their icons, where they were before the menu had a tick.
  const locations = page.getByRole("menu", { name: "Choose working location", exact: true });
  expect(new Set(await labelStarts(locations)).size).toBe(1);
  expect(await tickPlace(locations)).toEqual({ place: "end", glyph: "✓", inset: 8 });
  if (process.env.HAARNESS_SCREENSHOTS)
    for (const theme of ["dark", "light"] as const) {
      await page.evaluate((theme) => window.haarness.preferences({ theme }), theme);
      await expect(page.locator("html")).toHaveAttribute("data-theme", theme);
      await page
        .getByRole("menu", { name: "Choose working location", exact: true })
        .screenshot({ path: join(process.env.HAARNESS_SCREENSHOTS, `location-menu-${theme}.png`) });
    }
  await current.click();
  await expect(page.getByRole("menu", { name: "Choose working location", exact: true })).toHaveCount(0);
  await page.waitForTimeout(500);
  expect(await page.evaluate(() => (window as any).repicked)).toEqual({ reopened: false });
  await expect(composer).toHaveValue("A draft shared between working locations");
  await page
    .getByRole("button", { name: "Choose working location", exact: true })
    .click();
  await page
    .getByRole("menuitem", { name: "New worktree…", exact: true })
    .click();
  const dialog = page.getByRole("dialog", {
    name: "New worktree",
    exact: true,
  });
  await dialog
    .getByRole("textbox", { name: "Name", exact: true })
    .fill("Desktop feature");
  await dialog
    .getByRole("button", { name: "Create worktree", exact: true })
    .click();
  await expect(
    page.getByRole("button", { name: "Choose working location", exact: true }),
  ).toContainText("Desktop feature");
  await expect(composer).toHaveValue(
    "A draft shared between working locations",
  );
  await page.screenshot({ path: "test-results/new-chat-worktree.png" });
  const catalog = await client.workspacesList();
  const tree = catalog.workspaces.find(
    (item) => item.name === "Desktop feature",
  )!;
  expect(tree.kind).toBe("git_worktree");
  extraWorkspaces.push(tree.path);
  expect((await client.listThreads()).length).toBe(count);
  await expect(
    page
      .locator(".project-list .project-heading")
      .filter({ hasText: /^Desktop feature$/ }),
  ).toHaveCount(0);
  const treeId = "local:" + tree.path;
  const request = async <T>(method: string, args: unknown[] = []) =>
    page.evaluate(
      async ({ id, method, args }) => window.haarness.api(id, method, args),
      { id: treeId, method, args },
    ) as Promise<T>;
  await send(
    '!tool bash {"command":"pwd; printf checkout-proof > checkout-proof.txt"}',
  );
  await expect(
    page.getByRole("button", { name: "Allow once", exact: true }),
  ).toBeVisible();
  const waiting = await request<{ id: string; title: string }[]>("listThreads");
  const treeThread = waiting[0];
  await request("updateThread", [
    treeThread.id,
    { title: "Worktree conversation" },
  ]);
  await expect(
    primary
      .locator(".session-row")
      .filter({ hasText: "Worktree conversation" }),
  ).toContainText("Desktop feature");
  await newSession();
  await expect(
    page.getByRole("button", { name: "Choose working location", exact: true }),
  ).toContainText("Project folder");
  await send("The main folder stays usable during an approval");
  await expect(page.locator(".transcript")).toContainText(
    "Echo: The main folder stays usable during an approval",
  );
  expect((await request<unknown[]>("listPermissions")).length).toBe(1);
  expect((await client.listPermissions()).length).toBe(0);
  await primary
    .getByRole("button", { name: "Worktree conversation", exact: true })
    .click();
  await page.getByRole("button", { name: "Allow once", exact: true }).click();
  await expect(page.locator(".transcript")).toContainText("Tool bash =>");
  await expect
    .poll(() => readFile(join(tree.path, "checkout-proof.txt"), "utf8"))
    .toBe("checkout-proof");
  await expect
    .poll(() =>
      readFile(join(workspace, "checkout-proof.txt"), "utf8").catch(
        () => "missing",
      ),
    )
    .toBe("missing");
  await openProjectPanel("Files");
  // The persistent explorer may have listed this checkout while its turn was
  // waiting for approval. Refresh after the external tool creates the file.
  await page.getByRole("button", { name: "Refresh file tree", exact: true }).click();
  await page
    .getByRole("button", { name: "checkout-proof.txt", exact: true })
    .click();
  await expect(page.locator(".cm-content")).toHaveText("checkout-proof");
  await openProjectPanel("Review");
  await expect(page.locator(".review")).toContainText("checkout-proof.txt");
  await page
    .getByRole("button", { name: "Hide side panel", exact: true })
    .click();
  const handle = primary.getByRole("button", {
    name: "Worktree conversation",
    exact: true,
  });
  await handle.focus();
  await handle.press("Alt+Home");
  await expect(primary.locator(".session-title").first()).toHaveText(
    "Worktree conversation",
  );
  await page.reload();
  await expect(primary.locator(".session-title").first()).toHaveText(
    "Worktree conversation",
  );
  await expect(page.locator(".transcript")).toContainText("Tool bash =>");
  await expect(page.locator(".checkout-badge")).toHaveText("Desktop feature");
  await page.screenshot({ path: "test-results/project-worktree.png" });
  await newSession();
  await expect(
    page.getByRole("button", { name: "Choose working location", exact: true }),
  ).toContainText("Project folder");
  await expect(composer).toHaveValue("");
  // Imported archives deliberately preserve thread IDs. Their drafts and
  // sidebar identities must still be scoped to the actual working folder.
  const shared = await client.createThread({
    title: "Imported identity",
    provider: "echo",
    model: "echo-1",
  });
  const archive = await client.exportSessionArchive(shared.id);
  await request("importSessionArchive", [{ archive }]);
  const rootRow = primary
    .locator(".session-row")
    .filter({
      has: page.getByRole("button", { name: "Imported identity", exact: true }),
    })
    .filter({ hasNot: page.locator(".session-location") });
  const treeRow = primary
    .locator(".session-row")
    .filter({
      has: page.getByRole("button", { name: "Imported identity", exact: true }),
    })
    .filter({ has: page.locator(".session-location") });
  await expect(rootRow).toHaveCount(1);
  await expect(treeRow).toHaveCount(1);
  await rootRow
    .getByRole("button", { name: "Imported identity", exact: true })
    .click();
  await composer.fill("Main folder draft");
  await treeRow
    .getByRole("button", { name: "Imported identity", exact: true })
    .click();
  await expect(composer).toHaveValue("");
  await composer.fill("Worktree draft");
  await rootRow
    .getByRole("button", { name: "Imported identity", exact: true })
    .click();
  await expect(composer).toHaveValue("Main folder draft");
  await treeRow
    .getByRole("button", { name: "Imported identity", exact: true })
    .click();
  await expect(composer).toHaveValue("Worktree draft");
  await newSession();
  expect(errors).toEqual([]);
});

test("worktree view groups sessions, isolates drafts and terminals, and manages checkouts from the sidebar", async () => {
  test.setTimeout(240000);
  const project = page.locator(
    '.project-group[aria-label="Project workspace"]',
  );
  const composer = page.getByRole("textbox", { name: "Message", exact: true });
  const menu = async (name: string) => {
    await project
      .getByRole("button", {
        name: "Project options for workspace",
        exact: true,
      })
      .click();
    await page.getByRole("menuitem", { name, exact: true }).click();
  };
  await menu("Enable worktree view");
  const local = project.getByRole("region", {
    name: "Checkout Project folder",
    exact: true,
  });
  await expect(local).toBeVisible();
  await expect(local.locator(".worktree-label")).toContainText("main");
  await project
    .getByRole("button", { name: "New worktree", exact: true })
    .click();
  let dialog = page.getByRole("dialog", { name: "New worktree", exact: true });
  await dialog
    .getByRole("textbox", { name: "Name", exact: true })
    .fill("Sidebar feature");
  await dialog
    .getByRole("button", { name: "Create worktree", exact: true })
    .click();
  const tree = project.getByRole("region", {
    name: "Checkout Sidebar feature",
    exact: true,
  });
  await expect(tree).toBeVisible();
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
  );
  const checkout = (await client.workspacesList()).workspaces.find(
    (item) => item.name === "Sidebar feature",
  )!;
  extraWorkspaces.push(checkout.path);
  const treeId = "local:" + checkout.path;
  const request = async <T>(method: string, args: unknown[] = []) =>
    page.evaluate(
      async ({ id, method, args }) => window.haarness.api(id, method, args),
      { id: treeId, method, args },
    ) as Promise<T>;
  await expect(
    page.getByRole("button", { name: "Choose working location", exact: true }),
  ).toContainText("Sidebar feature");
  await composer.fill("Worktree starter draft");
  await local
    .getByRole("button", { name: "New chat in Project folder", exact: true })
    .click();
  await expect(composer).toHaveValue("");
  await composer.fill("Local starter draft");
  await tree
    .getByRole("button", { name: "New chat in Sidebar feature", exact: true })
    .click();
  await expect(composer).toHaveValue("Worktree starter draft");
  await page.reload();
  await expect(tree).toBeVisible();
  await expect(composer).toHaveValue("Worktree starter draft");
  await local
    .getByRole("button", { name: "New chat in Project folder", exact: true })
    .click();
  await expect(composer).toHaveValue("Local starter draft");
  // The switch to the checkout takes a moment (here its first step, saving
  // which checkouts are open, is held for 1.5 s). The message box waits for
  // it: typed sooner, the text would land in the draft of the chat being
  // left, and Send would then send the checkout's saved draft.
  await app.evaluate(({ ipcMain }) => {
    const handlers = (ipcMain as unknown as { _invokeHandlers: Map<string, (...args: any[]) => unknown> })._invokeHandlers;
    const original = handlers.get("haarness:storage")!;
    ipcMain.removeHandler("haarness:storage");
    ipcMain.handle("haarness:storage", async (event, key, value) => {
      if (key !== "worktree-expansion:v1") return original(event, key, value);
      ipcMain.removeHandler("haarness:storage");
      ipcMain.handle("haarness:storage", original);
      await new Promise((resolve) => setTimeout(resolve, 1500));
      return original(event, key, value);
    });
  });
  await tree
    .getByRole("button", { name: "New chat in Sidebar feature", exact: true })
    .click();
  const sidebarCommand =
    '!tool bash {"command":"pwd; printf sidebar-proof > sidebar-proof.txt"}';
  await send(sidebarCommand);
  await expect(
    page.getByRole("button", { name: "Allow once", exact: true }),
  ).toBeVisible();
  await expect(page.locator("article.message.user").last()).toContainText("sidebar-proof");
  const thread = (await request<{ id: string }[]>("listThreads"))[0];
  await request("updateThread", [
    thread.id,
    { title: "Sidebar checkout conversation" },
  ]);
  await expect(
    tree.getByRole("button", {
      name: "Sidebar checkout conversation",
      exact: true,
    }),
  ).toBeVisible();
  await expect(
    local.getByRole("button", {
      name: "Sidebar checkout conversation",
      exact: true,
    }),
  ).toHaveCount(0);
  await local
    .getByRole("button", { name: "New chat in Project folder", exact: true })
    .click();
  // The folder's saved draft loads after the switch; typing before it lands
  // would be replaced by it.
  await expect(composer).toHaveValue("Local starter draft");
  await send("Local checkout conversation");
  await expect(page.locator(".transcript")).toContainText(
    "Echo: Local checkout conversation",
  );
  expect((await request<unknown[]>("listPermissions")).length).toBe(1);
  await tree
    .getByRole("button", {
      name: "Checkout options for Sidebar feature",
      exact: true,
    })
    .click();
  await page
    .getByRole("menuitem", { name: "Remove worktree…", exact: true })
    .click();
  await page
    .getByRole("dialog", { name: "Remove worktree?", exact: true })
    .getByRole("button", { name: "Remove worktree", exact: true })
    .click();
  await expect(page.locator(".error-banner")).toContainText("active tasks");
  expect((await request<unknown[]>("listPermissions")).length).toBe(1);

  await tree
    .getByRole("button", { name: "Sidebar checkout conversation", exact: true })
    .click();
  await page.getByRole("button", { name: "Allow once", exact: true }).click();
  await expect(page.locator(".transcript")).toContainText("Tool bash =>");
  expect(await readFile(join(checkout.path, "sidebar-proof.txt"), "utf8")).toBe(
    "sidebar-proof",
  );
  expect(
    await readFile(join(workspace, "sidebar-proof.txt"), "utf8").catch(
      () => "missing",
    ),
  ).toBe("missing");
  await newSession();
  await expect(
    page.getByRole("button", { name: "Choose working location", exact: true }),
  ).toContainText("Sidebar feature");
  await expect(composer).toHaveValue("");
  await page
    .getByRole("button", { name: "Toggle terminal", exact: true })
    .click();
  const input = page.locator(".xterm-helper-textarea");
  await expect(input).toBeFocused();
  const ptys = await request<{ id: string; cwd: string }[]>("listPtys");
  expect(ptys).toHaveLength(1);
  expect(ptys[0].cwd).toBe(checkout.path);
  await input.fill("pwd > sidebar-terminal.txt");
  await input.press("Enter");
  await expect
    .poll(() =>
      readFile(join(checkout.path, "sidebar-terminal.txt"), "utf8").catch(
        () => "",
      ),
    )
    .toBe(checkout.path + "\n");
  await page
    .getByRole("button", { name: "Hide terminal", exact: true })
    .click();
  await local
    .getByRole("button", { name: "New chat in Project folder", exact: true })
    .click();
  expect((await client.listPtys()).some((item) => item.id === ptys[0].id)).toBe(
    false,
  );
  await tree
    .getByRole("button", { name: "Sidebar checkout conversation", exact: true })
    .click();
  await page
    .getByRole("button", { name: "Toggle terminal", exact: true })
    .click();
  await expect(input).toBeFocused();
  expect(
    (await request<{ id: string }[]>("listPtys")).map((item) => item.id),
  ).toEqual([ptys[0].id]);
  await page
    .getByRole("button", { name: "Hide terminal", exact: true })
    .click();
  await request("deletePty", [ptys[0].id]);
  await tree
    .getByRole("button", { name: "New chat in Sidebar feature", exact: true })
    .click();
  await composer.fill("A fresh checkout draft after a conversation");
  await page.reload();
  await expect(
    page.getByRole("button", { name: "Choose working location", exact: true }),
  ).toBeVisible();
  await expect(composer).toHaveValue(
    "A fresh checkout draft after a conversation",
  );
  await tree
    .getByRole("button", { name: "Sidebar checkout conversation", exact: true })
    .click();
  await expect(page.locator(".transcript")).toContainText("Tool bash =>");
  await expect
    .poll(() =>
      page.evaluate((id) => window.haarness.storage("session:" + id), treeId),
    )
    .toBe(thread.id);
  // Collapse hides only this checkout; the state survives a renderer restart.
  await tree
    .getByRole("button", {
      name: "Collapse checkout Sidebar feature",
      exact: true,
    })
    .click();
  await expect(tree.locator(".session-row")).toHaveCount(0);
  await expect(local.locator(".session-row")).not.toHaveCount(0);
  await page.reload();
  await expect(
    tree.getByRole("button", {
      name: "Expand checkout Sidebar feature",
      exact: true,
    }),
  ).toBeVisible();
  await tree
    .getByRole("button", {
      name: "Expand checkout Sidebar feature",
      exact: true,
    })
    .press("Enter");
  await expect(
    tree.getByRole("button", {
      name: "Sidebar checkout conversation",
      exact: true,
    }),
  ).toBeVisible();
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
  );
  await expect(page.locator(".transcript")).toContainText("Tool bash =>");
  await page.screenshot({ path: "test-results/worktree-sidebar.png" });
  await menu("Disable worktree view");
  await expect(project.locator(".worktree-group")).toHaveCount(0);
  await expect(
    project.getByRole("button", {
      name: "Sidebar checkout conversation",
      exact: true,
    }),
  ).toBeVisible();
  await page.reload();
  await expect(project.locator(".worktree-group")).toHaveCount(0);
  await menu("Enable worktree view");
  await expect(
    page.locator(".sidebar").getByRole("button", { name: "Worktrees", exact: true }),
  ).toHaveCount(0);
  const action = async (label: string) => {
    await tree
      .getByRole("button", {
        name: "Checkout options for Sidebar feature",
        exact: true,
      })
      .click();
    await page.getByRole("menuitem", { name: label, exact: true }).click();
  };
  // Dirty checkout protection remains enforced by the real server.
  await action("Remove worktree…");
  await page
    .getByRole("dialog", { name: "Remove worktree?", exact: true })
    .getByRole("button", { name: "Remove worktree", exact: true })
    .click();
  // The server checks the checkout with git first, which a busy machine slows.
  await expect(page.locator(".error-banner")).toContainText("local changes", {
    timeout: 45_000,
  });
  await expect(tree).toBeVisible();
  expect(await readFile(join(checkout.path, "sidebar-proof.txt"), "utf8")).toBe(
    "sidebar-proof",
  );
  await action("Reset worktree…");
  dialog = page.getByRole("dialog", { name: "Reset worktree", exact: true });
  await selectValue(
    dialog.getByRole("combobox", { name: "Uncommitted changes", exact: true }),
    "Discard tracked and untracked changes",
  );
  await dialog
    .getByRole("button", { name: "Reset worktree", exact: true })
    .click();
  await expect
    .poll(() =>
      readFile(join(checkout.path, "sidebar-proof.txt"), "utf8").catch(
        () => "missing",
      ),
    )
    .toBe("missing");
  await action("Remove worktree…");
  await page
    .getByRole("dialog", { name: "Remove worktree?", exact: true })
    .getByRole("button", { name: "Remove worktree", exact: true })
    .click();
  await expect(tree).toHaveCount(0);
  expect(
    (await client.workspacesList()).workspaces.some(
      (item) => item.id === checkout.id,
    ),
  ).toBe(false);
  await expect(local).toBeVisible();
  await menu("Disable worktree view");
  expect(errors).toEqual([]);
});

test("worktree discovery, missing checkouts and inactive-project menus retain the owning project", async () => {
  test.setTimeout(180000);
  const project = page.locator(
    '.project-group[aria-label="Project workspace"]',
  );
  const menu = async (name: string) => {
    await project
      .getByRole("button", {
        name: "Project options for workspace",
        exact: true,
      })
      .click();
    await page.getByRole("menuitem", { name, exact: true }).click();
  };
  await menu("Enable worktree view");
  const created = await client.createWorkspace({ name: "Discovered checkout" });
  extraWorkspaces.push(created.workspace.path);
  await menu("Refresh project");
  const discovered = project.getByRole("region", {
    name: "Checkout Discovered checkout",
    exact: true,
  });
  await expect(discovered).toBeVisible();
  await discovered
    .getByRole("button", {
      name: "New chat in Discovered checkout",
      exact: true,
    })
    .click();
  await send("Remember this checkout");
  await expect(page.locator(".transcript")).toContainText(
    "Echo: Remember this checkout",
  );
  await project
    .getByRole("button", { name: "New chat in Project folder", exact: true })
    .click();
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
  );
  // Move the original checkout aside so restoration preserves its Git identity.
  execFileSync(
    binary,
    ["--workspace", created.workspace.path, "serve", "--stop"],
    { env, stdio: "ignore", timeout: 30000 },
  );
  const movedCheckout = join(temporary, "temporarily-unavailable-checkout");
  await rename(created.workspace.path, movedCheckout);
  await menu("Refresh project");
  await expect(discovered).toHaveCount(0);
  expect(
    (await client.workspacesList()).workspaces.find(
      (item) => item.id === created.workspace.id,
    )?.exists,
  ).toBe(false);
  await page
    .getByRole("button", { name: "Choose working location", exact: true })
    .click();
  await expect(
    page.getByRole("menuitemradio", { name: "Discovered checkout", exact: true }),
  ).toHaveCount(0);
  await expect(page.getByRole("menuitemradio", { name: "Project folder", exact: true })).toBeVisible();
  await page.keyboard.press("Escape");
  await menu("Show unavailable worktrees (1)");
  await expect(discovered).toContainText("Folder not found");
  await expect(
    discovered.getByRole("button", {
      name: "New chat in Discovered checkout",
      exact: true,
    }),
  ).toBeDisabled();
  await menu("Hide unavailable worktrees");
  await expect(discovered).toHaveCount(0);
  await menu("Show unavailable worktrees (1)");
  await expect(discovered).toBeVisible();
  await page.reload();
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
  );
  await expect(discovered).toHaveCount(0);
  await menu("Disable worktree view");
  await expect(
    project
      .locator(".session-row")
      .filter({ hasText: "Remember this checkout" }),
  ).toHaveCount(0);
  await expect(project.locator(".project-error")).toHaveCount(0);
  await menu("Enable worktree view");
  await expect(discovered).toHaveCount(0);

  // Restoring the real checkout makes it usable again with its original history.
  await rename(movedCheckout, created.workspace.path);
  await menu("Refresh project");
  await expect(discovered).toBeVisible();
  await expect(discovered).not.toContainText("Folder not found");
  await discovered.locator(".session-row > button").first().click();
  await expect(page.locator(".transcript")).toContainText(
    "Echo: Remember this checkout",
  );
  await project
    .getByRole("button", { name: "New chat in Project folder", exact: true })
    .click();
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
  );
  execFileSync(
    binary,
    ["--workspace", created.workspace.path, "serve", "--stop"],
    {
      env,
      stdio: "ignore",
      timeout: 30000,
    },
  );
  execFileSync("git", ["worktree", "remove", created.workspace.path], {
    cwd: workspace,
    stdio: "ignore",
  });
  await menu("Refresh project");
  await expect(discovered).toHaveCount(0);
  await menu("Show unavailable worktrees (1)");
  await expect(discovered).toContainText("Folder not found");
  await discovered
    .getByRole("button", {
      name: "Checkout options for Discovered checkout",
      exact: true,
    })
    .click();
  await page
    .getByRole("menuitem", { name: "Remove missing worktree…", exact: true })
    .click();
  await page
    .getByRole("dialog", { name: "Remove worktree?", exact: true })
    .getByRole("button", { name: "Remove worktree", exact: true })
    .click();
  await expect(discovered).toHaveCount(0);
  expect(
    (await client.workspacesList()).workspaces.some(
      (item) => item.id === created.workspace.id,
    ),
  ).toBe(false);
  const plain = join(temporary, "ordinary-folder");
  await mkdir(plain);
  extraWorkspaces.push(plain);
  await app.evaluate(({ dialog }, directory) => {
    const original = dialog.showOpenDialog;
    dialog.showOpenDialog = async () => {
      dialog.showOpenDialog = original;
      return { canceled: false, filePaths: [directory] };
    };
  }, plain);
  await page
    .getByRole("button", { name: "Open another project", exact: true })
    .click();
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
  );
  const folder = page.locator(
    '.project-group[aria-label="Project ordinary-folder"]',
  );
  await folder
    .getByRole("button", {
      name: "Project options for ordinary-folder",
      exact: true,
    })
    .click();
  await expect(
    page.getByRole("menuitem", { name: "Enable worktree view", exact: true }),
  ).toHaveCount(0);
  await page.keyboard.press("Escape");
  // Create on the Git project's sidebar while a different, non-Git project is active.
  await project
    .getByRole("button", { name: "New worktree", exact: true })
    .click();
  const dialog = page.getByRole("dialog", {
    name: "New worktree",
    exact: true,
  });
  await dialog
    .getByRole("textbox", { name: "Name", exact: true })
    .fill("Correct owner");
  await dialog
    .getByRole("button", { name: "Create worktree", exact: true })
    .click();
  const owned = project.getByRole("region", {
    name: "Checkout Correct owner",
    exact: true,
  });
  await expect(owned).toBeVisible();
  const checkout = (await client.workspacesList()).workspaces.find(
    (item) => item.name === "Correct owner",
  )!;
  extraWorkspaces.push(checkout.path);
  expect(checkout.projectId).toBe(created.workspace.projectId);
  await expect(folder.locator(".worktree-group")).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: "Choose working location", exact: true }),
  ).toContainText("Correct owner");
  await project
    .getByRole("button", { name: "New chat in Project folder", exact: true })
    .click();
  await menu("Disable worktree view");
  expect(errors).toEqual([]);
});

test("large projects remain editable beyond ten thousand snapshot entries and two thousand listed files", async () => {
  test.setTimeout(180000);
  const directory = join(temporary, "large-project");
  extraWorkspaces.push(directory);
  await mkdir(join(directory, ".haarness"), { recursive: true });
  await mkdir(join(directory, "many"));
  await writeFile(
    join(directory, ".haarness/config.toml"),
    '[defaults]\nprovider="echo"\nmodel="echo-1"\n[permissions]\nbash="ask"\n',
  );
  // Empty source files reproduce the old entry-count rejection without relying
  // on large byte payloads or a paid model.
  for (let start = 0; start < 10005; start += 100)
    await Promise.all(
      Array.from({ length: Math.min(100, 10005 - start) }, (_, offset) =>
        writeFile(
          join(
            directory,
            "many",
            String(start + offset).padStart(5, "0") + ".txt",
          ),
          "",
        ),
      ),
    );
  execFileSync("git", ["init", "-b", "main"], {
    cwd: directory,
    stdio: "ignore",
  });
  await app.evaluate(({ dialog }, path) => {
    const original = dialog.showOpenDialog;
    dialog.showOpenDialog = async () => {
      dialog.showOpenDialog = original;
      return { canceled: false, filePaths: [path] };
    };
  }, directory);
  await page
    .getByRole("button", { name: "Open another project", exact: true })
    .click();
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
  );
  await newSession();
  await send(
    '!tool bash {"command":"printf large-project-proof > many/10004.txt"}',
  );
  await expect(
    page.getByRole("button", { name: "Allow once", exact: true }),
  ).toBeVisible();
  await page.getByRole("button", { name: "Allow once", exact: true }).click();
  await expect(page.locator(".transcript")).toContainText("Tool bash =>", {
    timeout: 90000,
  });
  await expect
    .poll(() => readFile(join(directory, "many/10004.txt"), "utf8"))
    .toBe("large-project-proof");
  const listing = await page.evaluate(
    async (id) =>
      window.haarness.files<{
        entries: { path: string }[];
        truncated: boolean;
      }>(id, "list", { path: "many" }),
    "local:" + directory,
  );
  expect(listing.truncated).toBe(false);
  expect(listing.entries.length).toBe(10005);
  expect(listing.entries.at(-1)?.path).toBe("many/10004.txt");
  await openProjectPanel("Files");
  await page
    .getByRole("textbox", { name: "Find files", exact: true })
    .fill("10004");
  await page
    .getByRole("button", { name: "many/10004.txt", exact: true })
    .click();
  await expect(page.locator(".cm-content")).toContainText(
    "large-project-proof",
  );
  await page
    .getByRole("button", { name: "Hide side panel", exact: true })
    .click();
  await page
    .locator('.project-group[aria-label="Project workspace"] .project-heading')
    .click();
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
  );
  expect(errors).toEqual([]);
});

test("themed pickers search, stay within the window, support keyboard selection and contain no native selects", async () => {
  // Search appears for larger catalogs. Use connected fixture models instead
  // of depending on the disconnected static providers that the UI now hides.
  const configPath = join(workspace, ".haarness/config.toml");
  const originalConfig = await readFile(configPath, "utf8");
  await writeFile(
    configPath,
    originalConfig +
      '\n[providers.picker-fixture]\nprotocol="openai-chat-completions"\nbase_url="http://127.0.0.1:1/v1"\nrequires_auth=false\nmodels=' +
      JSON.stringify(
        Array.from({ length: 10 }, (_, index) => `picker-model-${index}`),
      ) +
      "\n",
  );
  try {
    await expect
      .poll(async () =>
        (await client.providers()).some(
          (provider) =>
            provider.id === "picker-fixture" &&
            provider.authenticated &&
            provider.models.length === 10,
        ),
      )
      .toBe(true);
    await newSession();
    await app.evaluate(({ BrowserWindow }) =>
      BrowserWindow.getAllWindows()[0].setSize(900, 700),
    );
    const model = page.getByRole("combobox", { name: "Model", exact: true });
    await model.click();
    const popup = page.locator(".select-popover");
    const search = page.getByRole("combobox", {
      name: "Search model",
      exact: true,
    });
    await expect(search).toBeFocused();
    await search.fill("echo");
    await expect(popup.getByRole("option")).not.toHaveCount(0);
    await expect(popup.getByRole("option").first()).toContainText(/echo/i);
    const bounds = await popup.evaluate((element) => {
      const box = element.getBoundingClientRect();
      return {
        left: box.left,
        top: box.top,
        right: box.right,
        bottom: box.bottom,
        width: innerWidth,
        height: innerHeight,
      };
    });
    expect(bounds.left).toBeGreaterThanOrEqual(0);
    expect(bounds.top).toBeGreaterThanOrEqual(0);
    expect(bounds.right).toBeLessThanOrEqual(bounds.width);
    expect(bounds.bottom).toBeLessThanOrEqual(bounds.height);
    await page.screenshot({ path: "test-results/model-picker-dark.png" });
    await search.fill("there-is-no-such-model");
    await expect(popup).toContainText("No matches");
    await search.press("Enter");
    await expect(popup).toBeVisible();
    await search.fill("echo");
    await search.press("Enter");
    await expect(model).toHaveAttribute("data-value", "echo/echo-1");
    await expect(popup).toBeVisible();
    await popup.getByRole("button", { name: "Done", exact: true }).click();
    await expect(popup).toHaveCount(0);
    await expect(model).toBeFocused();
    // A keyboard choice keeps the highlight on the chosen model while the
    // picker applies it, so another Enter keeps that model and ArrowDown
    // moves on from it rather than from the first row.
    await model.click();
    await expect(search).toBeFocused();
    for (let step = 0; step < 3; step++) await search.press("ArrowDown");
    const highlighted = popup.locator(".select-option.highlighted");
    const target = (await highlighted.getAttribute("data-value"))!;
    expect(target).not.toBe("echo/echo-1");
    await search.press("Enter");
    await expect(model).toHaveAttribute("data-value", target);
    await expect(popup.locator(".model-picker-detail")).toHaveAttribute("aria-busy", "false");
    const chosen = popup.locator('[role="option"][aria-selected="true"]');
    await expect(chosen).toHaveAttribute("data-value", target);
    await expect(highlighted).toHaveCount(1);
    await expect(highlighted).toHaveAttribute("data-value", target);
    await expect(search).toHaveAttribute("aria-activedescendant", (await chosen.getAttribute("id"))!);
    await expect(popup.getByRole("listbox", { name: "Model", exact: true })).toHaveAttribute(
      "aria-activedescendant",
      (await chosen.getAttribute("id"))!,
    );
    await page.screenshot({ path: "test-results/model-picker-keyboard-choice.png" });
    const choices = await popup
      .locator('[role="option"]:not([aria-disabled="true"])')
      .evaluateAll((items) => items.map((item) => item.getAttribute("data-value")));
    await search.press("ArrowDown");
    await expect(highlighted).toHaveAttribute(
      "data-value",
      choices[(choices.indexOf(target) + 1) % choices.length]!,
    );
    await search.press("ArrowUp");
    await search.press("Enter");
    await expect(model).toHaveAttribute("data-value", target);
    await search.press("Escape");
    await expect(popup).toHaveCount(0);
    await selectValue(model, "echo/echo-1");
    await expect(model).toHaveAttribute("data-value", "echo/echo-1");
    await expect(popup).toHaveCount(0);
    await model.focus();
    await model.press("ArrowDown");
    await expect(search).toBeFocused();
    await search.press("Escape");
    await expect(model).toBeFocused();
    await expect(page.locator("select")).toHaveCount(0);
    await page.getByRole("button", { name: "Settings", exact: true }).click();
    const settings = page.getByRole("dialog", {
      name: "Settings",
      exact: true,
    });
    const appearance = settings.getByRole("combobox", {
      name: "Appearance",
      exact: true,
    });
    await selectValue(appearance, "light");
    await expect(appearance).toHaveAttribute("data-value", "light");
    await appearance.press("ArrowDown");
    const list = settings.getByRole("listbox", {
      name: "Appearance",
      exact: true,
    });
    await expect(list).toBeFocused();
    await page.screenshot({ path: "test-results/appearance-picker-light.png" });
    await list.press("Escape");
    await expect(settings).toBeVisible();
    await expect(appearance).toBeFocused();
    await appearance.click();
    await list.press("End");
    await list.press("Enter");
    await expect(appearance).toHaveAttribute("data-value", "dark");
    await expect(popup).toHaveCount(0);
    await appearance.click();
    await list.press("l");
    await list.press(" ");
    await expect(appearance).toHaveAttribute("data-value", "light");
    await appearance.click();
    await list.press("d");
    await list.press(" ");
    await expect(appearance).toHaveAttribute("data-value", "dark");
    await appearance.click();
    await list.press("Tab");
    await expect(popup).toHaveCount(0);
    await expect(settings).toBeVisible();
    await selectValue(appearance, "dark");
    await appearance.click();
    await settings
      .getByRole("heading", { name: "General", exact: true })
      .click();
    await expect(popup).toHaveCount(0);
    await expect(page.locator("select")).toHaveCount(0);
    await settings
      .getByRole("button", { name: "Close settings", exact: true })
      .click();
    await app.evaluate(({ BrowserWindow }) =>
      BrowserWindow.getAllWindows()[0].setSize(1420, 940),
    );
    expect(errors).toEqual([]);
  } finally {
    await writeFile(configPath, originalConfig);
    await expect
      .poll(async () =>
        (await client.providers()).some(
          (provider) => provider.id === "picker-fixture",
        ),
      )
      .toBe(false);
  }
});

test("the folder chip chooses a project or no folder, takes typed text along to an empty new chat, and keeps drafts across selection and reload", async () => {
  await page
    .locator(".project-list .project-heading")
    .filter({ hasText: /^workspace$/ })
    .click();
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
  );
  await newSession();
  const heading = page.locator(".conversation-empty h2");
  // The heading only names the project; the composer's chip chooses it.
  await expect(heading.getByRole("button")).toHaveCount(0);
  const trigger = page.getByRole("button", { name: /^Folder for new chat: / });
  const popup = page.getByRole("dialog", {
    name: "Choose a folder",
    exact: true,
  });
  const search = popup.getByRole("combobox", {
    name: "Search folders",
    exact: true,
  });
  const composer = page.getByRole("textbox", { name: "Message", exact: true });
  await composer.fill("keep this project draft");
  await trigger.click();
  await expect(search).toBeFocused();
  await expect(
    popup.getByRole("option", { name: "workspace", exact: true }),
  ).toHaveAttribute("aria-selected", "true");
  await search.fill("not-a-project-match");
  await expect(
    popup.getByText("No projects found", { exact: true }),
  ).toBeVisible();
  await expect(
    popup.getByRole("button", { name: "Open folder…", exact: true }),
  ).toBeVisible();
  await search.press("Escape");
  await expect(popup).toHaveCount(0);
  await expect(trigger).toBeFocused();
  await expect(composer).toHaveValue("keep this project draft");

  // Cancelling the native folder picker retains both the selected project and its draft.
  await app.evaluate(({ dialog }) => {
    const original = dialog.showOpenDialog;
    dialog.showOpenDialog = async () => {
      dialog.showOpenDialog = original;
      return { canceled: true, filePaths: [] };
    };
  });
  await trigger.click();
  await popup.getByRole("button", { name: "Open folder…", exact: true }).click();
  await expect(heading).toHaveText("What should we work on in workspace?");
  await expect(composer).toHaveValue("keep this project draft");

  const directory = join(temporary, "picker-project");
  await mkdir(join(directory, ".haarness"), { recursive: true });
  await writeFile(
    join(directory, ".haarness", "config.toml"),
    '[defaults]\nprovider="echo"\nmodel="echo-1"\n',
  );
  extraWorkspaces.push(directory);
  await app.evaluate(({ dialog }, directory) => {
    const original = dialog.showOpenDialog;
    dialog.showOpenDialog = async () => {
      dialog.showOpenDialog = original;
      return { canceled: false, filePaths: [directory] };
    };
  }, directory);
  await trigger.click();
  await popup.getByRole("button", { name: "Open folder…", exact: true }).click();
  await expect(heading).toHaveText("What should we work on in picker-project?");
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
  );
  await expect(composer).toBeFocused();
  // Typed first, then a folder chosen: the text goes along to its new chat,
  // which had none, and leaves the one it was typed in.
  await expect(composer).toHaveValue("keep this project draft");
  await composer.fill("keep the other draft");
  // Back in workspace by the sidebar, not the chip: nothing goes along.
  await page.locator(".project-list .project-heading").filter({ hasText: /^workspace$/ }).click();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
  await newSession();
  await expect(composer).toHaveValue("");
  await composer.fill("workspace's own draft");
  // A new chat with a draft of its own keeps it, and the text stays where
  // it was typed.
  await trigger.click();
  await search.fill("picker-project");
  await search.press("Enter");
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
  await expect(composer).toHaveValue("keep the other draft");
  await trigger.click();
  await search.fill("workspace");
  await popup.getByRole("option", { name: "workspace", exact: true }).click();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
  await expect(composer).toHaveValue("workspace's own draft");

  // On this computer the list leads with No folder, a chat of its own
  // without a project; its new chat had nothing, so the text goes along.
  await trigger.click();
  await expect(popup.getByRole("option").first()).toHaveText("No folder");
  await popup.getByRole("option", { name: "No folder", exact: true }).click();
  await expect(heading).toHaveText("What should we work on?");
  await expect(trigger).toHaveAccessibleName("Folder for new chat: No folder");
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
  await expect(composer).toHaveValue("workspace's own draft");
  await trigger.click();
  await popup.getByRole("option", { name: "picker-project", exact: true }).click();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
  await expect(composer).toHaveValue("keep the other draft");
  await expect(trigger).toHaveAccessibleName("Folder for new chat: picker-project");
  const parentThreads = await client.listThreads();
  await send("project-context-proof");
  await expect(page.locator(".transcript")).toContainText(
    "Echo: project-context-proof",
  );
  expect((await client.listThreads()).map((item) => item.id)).toEqual(
    parentThreads.map((item) => item.id),
  );
  await page.reload();
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
  );
  await expect(page.locator(".transcript")).toContainText(
    "Echo: project-context-proof",
  );
  await newSession();
  await trigger.click();
  await search.fill("workspace");
  await popup.getByRole("option", { name: "workspace", exact: true }).click();
  // Its text went along to No folder earlier.
  await expect(composer).toHaveValue("");
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
  await trigger.click();
  await expect(
    popup.getByRole("button", { name: "Open folder…", exact: true }),
  ).toHaveAttribute("data-last-action", "true");
  await popup.getByRole("button", { name: "Open folder…", exact: true }).focus();
  await popup
    .getByRole("button", { name: "Open folder…", exact: true })
    .press("Tab");
  await expect(popup).toHaveCount(0);
  await expect(trigger).toBeFocused();
  await page.screenshot({
    path: "test-results/composer-folder-chip.png",
  });
  await composer.fill("");
});

test("assistant hover icons copy Markdown and fork through exactly that reply", async () => {
  const header = page.locator(".project-list .project-heading").filter({ hasText: /^workspace$/ });
  if (!(await header.getAttribute("class"))?.includes("current")) await header.click();
  await expandPrimaryProject();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
  const source = await client.createThread({
    title: "Message action source",
    provider: "echo",
    model: "echo-1",
  });
  const first = await client.startTurn(source.id, {
    text: "**first** reply to keep",
    requestId: crypto.randomUUID(),
  });
  await client.waitTurn(first.id);
  const second = await client.startTurn(source.id, {
    text: "later reply to exclude",
    requestId: crypto.randomUUID(),
  });
  await client.waitTurn(second.id);
  await page
    .getByRole("region", { name: "Project workspace", exact: true })
    .getByRole("button", { name: source.title, exact: true })
    .click();
  const reply = page
    .locator("article.message.assistant")
    .filter({ hasText: "Echo: first reply to keep" });
  await expect(reply).toBeVisible();
  const user = page.locator("article.message.user").filter({
    hasText: "first reply to keep",
  });
  await user.hover();
  await expect(
    user.getByRole("button", { name: "Fork from message" }),
  ).toHaveCount(0);
  const userCopy = user.getByRole("button", {
    name: "Copy message",
    exact: true,
  });
  // Native clipboard writes need the same foreground window a real click uses.
  await app.evaluate(({ app, BrowserWindow }) => {
    app.focus({ steal: true });
    BrowserWindow.getAllWindows()[0].focus();
  });
  await expect.poll(() => page.evaluate(() => document.hasFocus())).toBe(true);
  await userCopy.focus();
  await userCopy.press("Enter");
  await expect(
    user.getByRole("button", { name: "Copied", exact: true }),
  ).toBeVisible();
  await expect
    .poll(() =>
      app.evaluate(
        async ({ clipboard }) =>
          (await clipboard.readText()) === "**first** reply to keep",
      ),
    )
    .toBe(true);
  await user.screenshot({ path: "test-results/user-message-copy-only.png" });
  await expect(reply.locator(".message-label")).toHaveCount(0);
  await expect(reply.locator(".message-footer .message-time")).toBeVisible();
  const toolbar = reply.locator(".message-tools");
  await page.getByRole("textbox", { name: "Message", exact: true }).hover();
  await expect(toolbar).toHaveCSS("opacity", "0");
  await reply.hover();
  await expect(toolbar).toHaveCSS("opacity", "1");
  await expect(toolbar.getByRole("button")).toHaveCount(2);
  await expect(
    toolbar
      .getByRole("button", { name: "Copy message", exact: true })
      .locator("svg"),
  ).toBeVisible();
  await toolbar
    .getByRole("button", { name: "Copy message", exact: true })
    .click();
  await expect(
    toolbar.getByRole("button", { name: "Copied", exact: true }),
  ).toBeVisible();
  await expect
    .poll(() =>
      app.evaluate(
        async ({ clipboard }) =>
          (await clipboard.readText()) === "Echo: **first** reply to keep",
      ),
    )
    .toBe(true);
  if (process.platform === "darwin") {
    expect(
      execFileSync("/usr/bin/pbpaste", { encoding: "utf8" }) ===
        "Echo: **first** reply to keep",
    ).toBe(true);
  }
  const fork = toolbar.getByRole("button", {
    name: "Fork from message",
    exact: true,
  });
  await fork.focus();
  await page.getByRole("textbox", { name: "Message", exact: true }).hover();
  await expect(toolbar).toHaveCSS("opacity", "1");
  await page.screenshot({ path: "test-results/assistant-message-actions.png" });
  await fork.press("Enter");
  await expect(page.locator(".transcript")).toContainText(
    "Echo: first reply to keep",
  );
  await expect(page.locator(".transcript")).not.toContainText(
    "later reply to exclude",
  );
  await expect(
    page
      .getByRole("region", { name: "Project workspace", exact: true })
      .getByRole("button", { name: "Fork of " + source.title, exact: true }),
  ).toHaveAttribute("aria-current", "true");
  const created = (await client.listThreads()).find(
    (item) => item.parentId === source.id,
  )!;
  const history = await client.listItems(created.id);
  const original = await client.listItems(source.id);
  const boundary = original.find(
    (item) => item.data.type === "assistant_message",
  )!;
  expect(created.forkOrigin).toEqual({
    title: source.title,
    sequence: boundary.sequence,
    itemId: boundary.id,
  });
  const origin = page.getByRole("note", { name: "Chat origin" });
  await expect(origin).toHaveText("Continued from " + source.title);
  await expect(origin.getByRole("button")).toBeEnabled();
  await origin.screenshot({ path: "test-results/fork-origin-divider.png" });
  expect(
    history
      .filter((item) =>
        ["user_message", "assistant_message"].includes(item.data.type),
      )
      .map((item) => item.data),
  ).toEqual(
    original
      .filter(
        (item) =>
          item.sequence <= boundary.sequence &&
          ["user_message", "assistant_message"].includes(item.data.type),
      )
      .map((item) => item.data),
  );
  expect(
    original.some(
      (item) =>
        "text" in item.data &&
        item.data.text.includes("later reply to exclude"),
    ),
  ).toBe(true);
  expect(errors).toEqual([]);
});

test("fork origin survives continuation and reload, and opens the source at its original reply", async () => {
  const source = await client.createThread({
    title: "Fork origin source",
    provider: "echo",
    model: "echo-1",
  });
  const first = await client.startTurn(source.id, {
    text: "original fork point",
    requestId: crypto.randomUUID(),
  });
  await client.waitTurn(first.id);
  const boundary = (await client.listItems(source.id)).find(
    (item) => item.data.type === "assistant_message",
  )!;
  // A fork made by another client carries the same durable provenance.
  const fork = await client.forkThread(source.id, {
    title: "Fork origin continuation",
    upToSequence: boundary.sequence,
  });
  const project = page.getByRole("region", {
    name: "Project workspace",
    exact: true,
  });
  await project.getByRole("button", { name: fork.title, exact: true }).click();
  const origin = page.getByRole("note", { name: "Chat origin" });
  await expect(origin).toHaveText("Continued from " + source.title);
  await send("only in the continuation");
  await expect(page.locator("article.message.assistant").last()).toContainText(
    "Echo: only in the continuation",
  );
  const seamOrder = async () =>
    page.locator(".transcript").evaluate((element) => {
      const inherited = [
        ...element.querySelectorAll("article.message.assistant"),
      ].find((node) => node.textContent?.includes("original fork point"))!;
      const marker = element.querySelector(".fork-origin")!;
      const own = [...element.querySelectorAll("article.message.user")].find(
        (node) => node.textContent?.includes("only in the continuation"),
      )!;
      return (
        !!(
          inherited.compareDocumentPosition(marker) &
          Node.DOCUMENT_POSITION_FOLLOWING
        ) &&
        !!(
          marker.compareDocumentPosition(own) & Node.DOCUMENT_POSITION_FOLLOWING
        )
      );
    });
  expect(await seamOrder()).toBe(true);
  await client.updateThread(source.id, {
    title: "Renamed origin",
    archived: true,
  });
  await client.updateThread(fork.id, { title: "Renamed continuation" });
  await expect(origin).toHaveText("Continued from Renamed origin");
  await page.reload();
  await expect(origin).toHaveText("Continued from Renamed origin");
  expect(await seamOrder()).toBe(true);
  // Later history puts the source's fork point well outside its bottom viewport.
  const later = await client.startTurn(source.id, {
    text: Array.from(
      { length: 90 },
      (_, i) => `Later source paragraph ${i}.`,
    ).join("\n\n"),
    requestId: crypto.randomUUID(),
  });
  await client.waitTurn(later.id);
  await origin.getByRole("button").focus();
  await origin.getByRole("button").press("Enter");
  const originalReply = page.locator(`[data-item-id="${boundary.id}"]`);
  await expect(originalReply).toBeInViewport();
  await expect(page.locator(".transcript")).toBeFocused();
  await expect(page.locator(".fork-origin")).toHaveCount(0);
  // A normal SSE refresh must not pull us away from the source boundary.
  await client.updateThread(fork.id, { title: "Updated continuation" });
  await expect(
    project.getByRole("button", { name: "Updated continuation", exact: true }),
  ).toBeVisible();
  await expect(originalReply).toBeInViewport();
  await expect(
    page.getByRole("button", { name: "Jump to latest", exact: true }),
  ).toBeVisible();
  await page.screenshot({ path: "test-results/fork-source-navigation.png" });
  await project
    .getByRole("button", { name: "Updated continuation", exact: true })
    .click();
  await expect(origin).toHaveText("Continued from Renamed origin");
  await expect(page.locator("article.message.assistant").last()).toContainText(
    "Echo: only in the continuation",
  );
  expect(await seamOrder()).toBe(true);
  await page.screenshot({ path: "test-results/fork-origin-continuation.png" });
  await client.updateThread(source.id, { archived: false });
  const empty = await client.forkThread(source.id, { upToSequence: 0 });
  const archive = await client.exportSessionArchive(empty.id);
  expect(archive.threads).toHaveLength(1);
  expect(archive.threads[0].items).toHaveLength(0);
  const importedId = crypto.randomUUID();
  archive.rootThreadId = importedId;
  archive.threads[0].thread.id = importedId;
  archive.threads[0].thread.title = "Imported fork without source";
  await client.importSessionArchive({ archive });
  await project
    .getByRole("button", { name: "Imported fork without source", exact: true })
    .click();
  await expect(origin).toHaveText("Continued from Renamed origin");
  await expect(origin.getByRole("button")).toBeDisabled();
  await expect(
    page.getByRole("textbox", { name: "Message", exact: true }),
  ).toBeVisible();
  await expect(page.locator(".conversation-empty")).toHaveCount(0);
  expect(errors).toEqual([]);
});

test("dictation inserts at the cursor, revises partial speech, stops cleanly and stays with its conversation", async () => {
  test.skip(
    process.platform !== "darwin",
    "Native dictation uses macOS Speech",
  );
  // This probe loads the actual compiled native adapter without requesting mic access.
  const executable = process.env.HAARNESS_DESKTOP_EXECUTABLE
    ? resolve(
        process.env.HAARNESS_DESKTOP_EXECUTABLE,
        "../../Resources/bin/haarness-voice",
      )
    : resolve("resources/haarness-voice");
  const probe = JSON.parse(
    execFileSync(executable, ["--probe", "--locale", "en-US"], {
      encoding: "utf8",
      timeout: 15000,
    }),
  );
  expect(probe.supported).toBe(true);
  await newSession();
  const composer = page.getByRole("textbox", { name: "Message", exact: true });
  const start = page.getByRole("button", {
    name: "Start dictation",
    exact: true,
  });
  const stop = page.getByRole("button", {
    name: "Stop dictation",
    exact: true,
  });
  const events = join(temporary, "voice-events.json");
  await composer.fill("Before after");
  await composer.evaluate((element: HTMLTextAreaElement) =>
    element.setSelectionRange(7, 7),
  );
  await writeFile(events, "[]");
  await start.click();
  await expect(
    page.getByRole("status").filter({ hasText: /^Listening…$/ }),
  ).toBeVisible();
  await writeFile(
    events,
    JSON.stringify([{ type: "transcript", text: "dictated", isFinal: false }]),
  );
  await expect(composer).toHaveValue("Before dictated after");
  await writeFile(
    events,
    JSON.stringify([
      { type: "transcript", text: "dictated", isFinal: false },
      { type: "transcript", text: "dictated words", isFinal: false },
    ]),
  );
  await expect(composer).toHaveValue("Before dictated words after");
  await stop.click();
  await expect(start).toBeVisible();
  await expect(composer).toHaveValue("Before dictated words final after");
  await expect(page.locator(".conversation-empty")).toBeVisible();

  await writeFile(events, "[]");
  await start.click();
  await expect(stop).toBeVisible();
  await composer.fill("A manual edit");
  await expect(start).toBeVisible();
  await writeFile(
    events,
    JSON.stringify([{ type: "transcript", text: "late words", isFinal: true }]),
  );
  await expect(composer).toHaveValue("A manual edit");
  const source = await client.createThread({
    title: "Dictation ownership",
    provider: "echo",
    model: "echo-1",
  });
  await page
    .getByRole("region", { name: "Project workspace", exact: true })
    .getByRole("button", { name: source.title, exact: true })
    .click();
  await composer.fill("Thread draft");
  await writeFile(events, "[]");
  await start.click();
  await expect(
    page.getByRole("status").filter({ hasText: /^Listening…$/ }),
  ).toBeVisible();
  const pid = Number(await readFile(join(temporary, "voice-pid"), "utf8"));
  await newSession();
  await expect(start).toBeVisible();
  await expect
    .poll(() => {
      try {
        process.kill(pid, 0);
        return false;
      } catch {
        return true;
      }
    })
    .toBe(true);
  await expect(composer).toHaveValue("A manual edit");
  await writeFile(events, "[]");
  await composer.press("Meta+Shift+m");
  await expect(stop).toBeVisible();
  await writeFile(
    events,
    JSON.stringify([
      {
        type: "error",
        message: "Allow Haarness in macOS Microphone settings.",
      },
    ]),
  );
  await expect(
    page
      .getByRole("alert")
      .filter({ hasText: "Allow Haarness in macOS Microphone settings." }),
  ).toBeVisible();
  await expect(start).toBeVisible();
  await expect(composer).toHaveValue("A manual edit");
  await page
    .getByRole("button", { name: "Dismiss dictation error", exact: true })
    .click();
  await expect(page.locator(".dictation-error")).toHaveCount(0);
  await writeFile(join(temporary, "voice-preparing"), "");
  await writeFile(events, "[]");
  await start.click();
  await expect(
    page.getByRole("status").filter({ hasText: "Downloading speech model…" }),
  ).toBeVisible();
  await composer.fill("Cancel model preparation");
  await expect(start).toBeVisible();
  await expect(page.locator(".dictation-error")).toHaveCount(0);
  await rm(join(temporary, "voice-preparing"));
  await writeFile(events, "[]");
  await start.click();
  await expect(stop).toBeVisible();
  await page.reload();
  await expect(start).toBeVisible();
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
  );
  await composer.fill("");
  if (probe.engine) {
    // Exercise real recognition through the Electron IPC and composer using
    // synthetic audio, so this check never records the test operator's voice.
    const helper = join(temporary, "voice-fixture");
    const fixture = await readFile(helper, "utf8");
    const audio = join(temporary, "dictation-proof.aiff");
    execFileSync(
      "say",
      [
        "-v",
        "Samantha",
        "-o",
        audio,
        "The quick brown fox jumps over the lazy dog. Please review this code.",
      ],
      { timeout: 15000 },
    );
    try {
      await writeFile(
        helper,
        `#!/usr/bin/env python3\nimport os\nos.execv(${JSON.stringify(executable)}, ${JSON.stringify([executable, "--file", audio, "--locale", "en-US"])})\n`,
      );
      await start.click();
      await expect(composer).toHaveValue(
        /quick brown fox[\s\S]*review this code/i,
        { timeout: 60000 },
      );
      await expect(start).toBeVisible();
      await expect(page.locator(".dictation-error")).toHaveCount(0);
      await expect(page.locator(".conversation-empty")).toBeVisible();
    } finally {
      await composer.fill("");
      await writeFile(helper, fixture);
    }
  }
});

test("delegated agents show one live task row and stay out of the sidebar", async () => {
  const parent = await client.createThread({
    title: "Agent task presentation",
    provider: "echo",
    model: "echo-1",
  });
  const sidebar = page.locator(".sidebar-lists");
  await sidebar
    .getByRole("button", { name: parent.title, exact: true })
    .click();
  const turn = await client.startTurn(parent.id, {
    requestId: crypto.randomUUID(),
    text:
      "!multi " +
      JSON.stringify([
        {
          name: "task",
          arguments: {
            description: "Inspect the authentication flow",
            agent: "general",
            background: true,
            prompt: '!tool bash {"command":"printf delegated-agent-proof"}',
          },
        },
        {
          name: "task",
          arguments: {
            description: "Review the routing structure",
            agent: "general",
            prompt: "routing-agent-proof",
          },
        },
      ]),
  });
  const first = page.getByRole("button", {
    name: "Open agent task: Inspect the authentication flow",
    exact: true,
  });
  const second = page.getByRole("button", {
    name: "Open agent task: Review the routing structure",
    exact: true,
  });
  await expect(first).toHaveCount(1);
  await expect(first).toHaveAttribute("aria-description", "Needs approval");
  await expect(first).toContainText("Echo 1");
  await expect(second).toHaveCount(1);
  await expect(second).toHaveAttribute("aria-description", "Completed");
  await expect(
    page.getByRole("button", { name: "Open agent session", exact: true }),
  ).toHaveCount(0);
  const children = (await client.listThreads()).filter(
    (thread) => thread.parentId === parent.id,
  );
  expect(children).toHaveLength(2);
  for (const child of children) {
    if (!process.env.HAARNESS_DESKTOP_LEGACY_TEST)
      expect(child.isSubagent).toBe(true);
    await expect(
      sidebar.getByRole("button", { name: child.title, exact: true }),
    ).toHaveCount(0);
  }
  await first.screenshot({ path: "test-results/agent-task-approval.png" });
  const childId = await first.getAttribute("data-child-id");
  expect(childId).toBeTruthy();
  const composer = page.getByRole("textbox", { name: "Message", exact: true });
  await composer.fill("Continue reviewing from the parent");
  await first.click();
  await expect(page.locator(".transcript")).toContainText(
    "delegated-agent-proof",
  );
  await expect(
    sidebar.getByRole("button", { name: parent.title, exact: true }),
  ).toBeVisible();
  await expect(composer).toHaveCount(0);
  await expect(page.locator(".composer, .dictation-button")).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: "Allow once", exact: true }),
  ).toBeVisible();
  // The native dictation shortcut must not start a recorder in this view.
  const voicePid = await readFile(join(temporary, "voice-pid"), "utf8").catch(
    () => null,
  );
  await page.keyboard.press("Meta+Shift+KeyM");
  // The child offers a direct way back, including when it is absent from the sidebar.
  await page
    .getByRole("button", { name: "Back to parent chat", exact: true })
    .click();
  await expect(composer).toHaveValue("Continue reviewing from the parent");
  expect(
    await readFile(join(temporary, "voice-pid"), "utf8").catch(() => null),
  ).toBe(voicePid);
  // Keyboard activation of the same row must navigate as well.
  await first.focus();
  await first.press("Enter");
  await expect(page.locator(".transcript")).toContainText(
    "delegated-agent-proof",
  );
  await expect(composer).toHaveCount(0);
  await page.getByRole("button", { name: "Allow once", exact: true }).click();
  await expect
    .poll(() => client.activeTurns().then((turns) => turns.length))
    .toBe(0);
  // Completion and a full renderer reload must not expose a child composer.
  await page.reload();
  await expect(
    page.getByRole("button", {
      name: "Back to parent chat",
      exact: true,
    }),
  ).toBeVisible();
  await expect(composer).toHaveCount(0);
  await page.screenshot({ path: "test-results/subagent-read-only.png" });
  await sidebar
    .getByRole("button", { name: parent.title, exact: true })
    .click();
  await expect(first).toHaveAttribute("aria-description", "Completed");
  await expect(composer).toHaveValue("Continue reviewing from the parent");
  await composer.fill("");
  await expect(first).toHaveCount(1);
  await expect(second).toHaveCount(1);
  await first.screenshot({ path: "test-results/agent-task-completed.png" });
  // A fork has parent lineage too; it remains an ordinary sidebar conversation.
  const fork = await client.forkThread(parent.id, {
    title: "Independent fork conversation",
  });
  if (!process.env.HAARNESS_DESKTOP_LEGACY_TEST)
    expect(fork.isSubagent).toBe(false);
  await expect(
    sidebar.getByRole("button", { name: fork.title, exact: true }),
  ).toBeVisible();
  await page.reload();
  await expect(first).toHaveAttribute("aria-description", "Completed");
  await expect(first).toHaveCount(1);
  await expect(second).toHaveCount(1);
  await expect(
    sidebar.getByRole("button", { name: fork.title, exact: true }),
  ).toBeVisible();
  for (const child of children)
    await expect(
      sidebar.getByRole("button", { name: child.title, exact: true }),
    ).toHaveCount(0);
  await sidebar.getByRole("button", { name: fork.title, exact: true }).click();
  await expect(composer).toBeVisible();
  expect((await client.getTurn(turn.id)).status).toBe("completed");
  expect(errors).toEqual([]);
});

test("agent stop controls cancel only their child and remain separate from row navigation", async () => {
  const parent = await client.createThread({
    title: "Independent agent controls",
    provider: "echo",
    model: "echo-1",
  });
  const heading = page
    .locator(".project-list .project-heading")
    .filter({ hasText: /^workspace$/ });
  if ((await heading.getAttribute("aria-expanded")) === "false")
    await heading.click();
  await page
    .locator(".sidebar-lists")
    .getByRole("button", { name: parent.title, exact: true })
    .click();
  const parentTurn = await client.startTurn(parent.id, {
    requestId: crypto.randomUUID(),
    text:
      "!multi " +
      JSON.stringify([
        {
          name: "task",
          arguments: {
            description: "Inspect authentication",
            agent: "general",
            background: true,
            prompt: '!tool bash {"command":"sleep 60"}',
          },
        },
        {
          name: "task",
          arguments: {
            description: "Review deployment",
            agent: "general",
            background: true,
            prompt: '!tool bash {"command":"printf sibling-proof"}',
          },
        },
        { name: "bash", arguments: { command: "printf parent-proof" } },
      ]),
  });
  const first = page.getByRole("button", {
    name: "Open agent task: Inspect authentication",
    exact: true,
  });
  const second = page.getByRole("button", {
    name: "Open agent task: Review deployment",
    exact: true,
  });
  const firstStop = page.getByRole("button", {
    name: "Stop agent task: Inspect authentication",
    exact: true,
  });
  const secondStop = page.getByRole("button", {
    name: "Stop agent task: Review deployment",
    exact: true,
  });
  await expect(firstStop).toBeVisible();
  await expect(secondStop).toBeVisible();
  await expect(first).toHaveAttribute("aria-description", "Needs approval");
  await expect(second).toHaveAttribute("aria-description", "Needs approval");
  const firstId = (await first.getAttribute("data-child-id"))!;
  const secondId = (await second.getAttribute("data-child-id"))!;
  const firstTurn = (await client.activeTurn(firstId))!;
  const secondTurn = (await client.activeTurn(secondId))!;
  const permission = (await client.listPermissions()).find(
    (item) => item.threadId === firstId,
  )!;
  await client.replyPermission(permission.permissionId, { decision: "once" });
  await expect(first.locator(".activity-shimmer")).toBeVisible();
  await expect(second.locator(".agent-task-detail")).toHaveCount(0);
  await page.reload();
  await expect(firstStop).toBeVisible();
  await expect(secondStop).toBeVisible();
  await expect(first.locator(".activity-shimmer")).toBeVisible();
  // Stop is a sibling of the navigation button, not a nested click target.
  await expect(first.locator("button")).toHaveCount(0);
  await expect(first.locator(".agent-task-status")).toHaveText("");
  await expect(second.locator(".agent-task-status")).toHaveText("");
  await first.click();
  await expect(
    page.getByRole("button", {
      name: "Back to parent chat",
      exact: true,
    }),
  ).toBeVisible();
  expect((await client.activeTurn(firstId))?.id).toBe(firstTurn.id);
  await page
    .getByRole("button", { name: "Back to parent chat", exact: true })
    .click();
  await second.focus();
  await second.press("Enter");
  await expect(
    page.getByRole("button", { name: "Allow once", exact: true }),
  ).toBeVisible();
  expect((await client.activeTurn(secondId))?.id).toBe(secondTurn.id);
  await page
    .getByRole("button", { name: "Back to parent chat", exact: true })
    .click();
  // Both targets stay usable in a narrow chat, with the activity still visible.
  // Other scenarios may leave Files open; narrow windows show that panel in
  // place of chat, so return to chat before checking its agent controls.
  const hidePanel = page.getByRole("button", {
    name: "Hide side panel",
    exact: true,
  });
  if (await hidePanel.isVisible()) await hidePanel.click();
  await expect(first).toBeVisible();
  const viewport = page.viewportSize();
  await page.setViewportSize({ width: 820, height: 800 });
  await first.scrollIntoViewIfNeeded();
  await page.screenshot({
    path: "test-results/agent-controls-working.png",
    animations: "allow",
  });
  await firstStop.click();
  await expect(first).toHaveAttribute("aria-description", "Stopped");
  await expect(firstStop).toHaveCount(0);
  await expect(first).toBeFocused();
  await expect(first.locator(".activity-shimmer")).toHaveCount(0);
  expect((await client.getTurn(firstTurn.id)).status).toBe("interrupted");
  expect((await client.activeTurn(secondId))?.id).toBe(secondTurn.id);
  expect((await client.activeTurn(parent.id))?.id).toBe(parentTurn.id);
  await expect(
    page.getByRole("textbox", { name: "Message", exact: true }),
  ).toBeVisible();
  // The pending-approval agent can also be stopped with the keyboard.
  await secondStop.focus();
  await secondStop.press("Space");
  await expect(second).toHaveAttribute("aria-description", "Stopped");
  await expect(secondStop).toHaveCount(0);
  await expect(second).toBeFocused();
  expect((await client.getTurn(secondTurn.id)).status).toBe("interrupted");
  expect((await client.activeTurn(parent.id))?.id).toBe(parentTurn.id);
  await expect
    .poll(async () =>
      (await client.listPermissions()).filter(
        (item) => item.threadId !== parent.id,
      ),
    )
    .toEqual([]);
  await page.screenshot({ path: "test-results/agent-controls-stopped.png" });
  await client.interruptThreadTree(parent.id);
  // Electron windows report no viewport, so restore the suite's own size.
  await page.setViewportSize(viewport ?? { width: 1420, height: 940 });
  await page.reload();
  await expect(first).toHaveAttribute("aria-description", "Stopped");
  await expect(second).toHaveAttribute("aria-description", "Stopped");
  await expect(page.locator(".agent-task-stop")).toHaveCount(0);
  expect(errors).toEqual([]);
});

test("activity shimmer stays continuous through refreshes, navigation and reduced motion", async () => {
  const parent = await client.createThread({
    title: "Continuous activity",
    provider: "echo",
    model: "echo-1",
  });
  await page
    .locator(".sidebar-lists")
    .getByRole("button", {
      name: parent.title,
      exact: true,
    })
    .click();
  await client.startTurn(parent.id, {
    requestId: crypto.randomUUID(),
    text:
      "!tool task " +
      JSON.stringify({
        description: "Continuous agent activity",
        agent: "general",
        prompt: '!tool bash {"command":"sleep 60"}',
      }),
  });
  const task = page.getByRole("button", {
    name: "Open agent task: Continuous agent activity",
    exact: true,
  });
  await expect(task).toHaveAttribute("aria-description", "Needs approval");
  const childId = await task.getAttribute("data-child-id");
  const permission = (await client.listPermissions()).find(
    (item) => item.threadId === childId,
  )!;
  await client.replyPermission(permission.permissionId, { decision: "once" });
  const activity = page.locator(".assistant-activity span");
  const taskActivity = task.locator(".agent-task-title > span");
  await expect(activity).toHaveText("Working with an agent");
  await expect(task).toHaveAttribute("data-status", "running");
  expect(
    await activity
      .locator("..")
      .evaluate((element) => getComputedStyle(element, "::before").content),
  ).toBe("none");
  // Compare the sweep's time relative to the document, rather than checking
  // only that an animation exists. A restarted sweep changes this offset.
  const phase = async (label: Locator) => {
    const clock = await label.evaluate(async (element) => {
      await new Promise(requestAnimationFrame);
      await new Promise(requestAnimationFrame);
      const animation = element.getAnimations().find(
        (item) => item instanceof CSSAnimation && item.animationName === "activity-shimmer",
      );
      if (!animation || animation.currentTime === null) throw new Error("Missing shimmer");
      return {
        text: element.textContent,
        start: animation.startTime,
        current: animation.currentTime,
        timeline: document.timeline.currentTime,
        pending: animation.pending,
        state: animation.playState,
        phase: Number(animation.currentTime) - Number(document.timeline.currentTime),
      };
    });
    await test.info().attach("shimmer-clock", {
      body: JSON.stringify(clock), contentType: "application/json",
    });
    return clock.phase;
  };
  const before = {
    activity: await phase(activity),
    task: await phase(taskActivity),
  };
  // Inspect rendered pixels on either side of the loop, not just its clock.
  // A shared startTime alone cannot catch a gradient that jumps at each repeat.
  const duration = await activity.evaluate((element) => {
    const animation = element
      .getAnimations()
      .find(
        (item) =>
          item instanceof CSSAnimation &&
          item.animationName === "activity-shimmer",
      )!;
    animation.pause();
    return Number(animation.effect!.getTiming().duration);
  });
  const frame = async (time: number) => {
    await activity.evaluate((element, time) => {
      element
        .getAnimations()
        .find(
          (item) =>
            item instanceof CSSAnimation &&
            item.animationName === "activity-shimmer",
        )!.currentTime = time;
    }, time);
    return activity.screenshot({ animations: "allow" });
  };
  const difference = (first: Buffer, second: Buffer) =>
    page.evaluate(
      async (frames) => {
        const pixels = async (source: string) => {
          const image = new Image();
          image.src = "data:image/png;base64," + source;
          await image.decode();
          const canvas = document.createElement("canvas");
          canvas.width = image.width;
          canvas.height = image.height;
          const context = canvas.getContext("2d")!;
          context.drawImage(image, 0, 0);
          return context.getImageData(0, 0, image.width, image.height).data;
        };
        const [first, second] = await Promise.all(frames.map(pixels));
        if (first.length !== second.length)
          throw new Error("Frame dimensions changed");
        let total = 0;
        for (let index = 0; index < first.length; index++)
          total += Math.abs(first[index] - second[index]);
        return total / first.length;
      },
      [first.toString("base64"), second.toString("base64")],
    );
  expect(
    await difference(
      await frame(duration - 0.001),
      await frame(duration + 0.001),
    ),
  ).toBeLessThan(0.1);
  // Increasing the label's width must not shift the highlight over its text.
  await frame(duration * 0.65);
  const box = (await activity.boundingBox())!;
  const original = await page.screenshot({ clip: box, animations: "allow" });
  await activity.evaluate((element) => {
    (element as HTMLElement).style.paddingRight = "96px";
  });
  const wider = await page.screenshot({ clip: box, animations: "allow" });
  expect(await difference(original, wider)).toBeLessThan(0.1);
  await activity.evaluate((element) => {
    (element as HTMLElement).style.removeProperty("padding-right");
    const animation = element
      .getAnimations()
      .find(
        (item) =>
          item instanceof CSSAnimation &&
          item.animationName === "activity-shimmer",
      )!;
    animation.play();
    animation.startTime = 0;
  });
  // Reconnect with the real server still working: REST snapshots replace the
  // turn and item data, then SSE resumes. It must not restart the visual sweep.
  await app.evaluate(({ BrowserWindow }, connection) => {
    BrowserWindow.getAllWindows()[0].webContents.send("haarness:event", {
      kind: "connection",
      connection,
      data: "Reconnect fixture",
    });
  }, "local:" + workspace);
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "false",
  );
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
  );
  expect(Math.abs((await phase(activity)) - before.activity)).toBeLessThan(50);
  expect(Math.abs((await phase(taskActivity)) - before.task)).toBeLessThan(50);
  // Leaving and returning actually remounts the status elements.
  await task.click();
  await expect(activity).toHaveText("Running a command");
  await page
    .getByRole("button", { name: "Back to parent chat", exact: true })
    .click();
  await expect(activity).toHaveText("Working with an agent");
  expect(Math.abs((await phase(activity)) - before.activity)).toBeLessThan(50);
  expect(Math.abs((await phase(taskActivity)) - before.task)).toBeLessThan(50);
  await page.emulateMedia({ reducedMotion: "reduce" });
  await expect(activity).toHaveCSS("animation-name", "none");
  await expect(taskActivity).toHaveCSS("animation-name", "none");
  await page.emulateMedia({ reducedMotion: "no-preference" });
  expect(Math.abs((await phase(activity)) - before.activity)).toBeLessThan(50);
  expect(Math.abs((await phase(taskActivity)) - before.task)).toBeLessThan(50);
  await page.screenshot({ path: "test-results/continuous-activity.png" });
  await client.interruptThreadTree(parent.id);
  await expect(page.locator(".assistant-activity")).toHaveCount(0);
  await expect(task).toHaveAttribute("data-status", "stopped");
  await expect(taskActivity).toHaveCSS("animation-name", "none");
  expect(errors).toEqual([]);
});

test("stopped and unsuccessful delegations keep their final state without duplicate rows", async () => {
  const parent = await client.createThread({
    title: "Agent terminal states",
    provider: "echo",
    model: "echo-1",
  });
  await page
    .locator(".sidebar-lists")
    .getByRole("button", { name: parent.title, exact: true })
    .click();
  await client.startTurn(parent.id, {
    requestId: crypto.randomUUID(),
    text:
      "!tool task " +
      JSON.stringify({
        description: "Review before stopping",
        background: true,
        prompt: '!tool bash {"command":"sleep 30"}',
      }),
  });
  const stopped = page.getByRole("button", {
    name: "Open agent task: Review before stopping",
    exact: true,
  });
  await expect(stopped).toHaveAttribute("aria-description", "Needs approval");
  await client.interruptThreadTree(parent.id);
  await expect(stopped).toHaveAttribute("aria-description", "Stopped");
  await expect
    .poll(() => client.activeTurns().then((turns) => turns.length))
    .toBe(0);
  await page.reload();
  await expect(stopped).toHaveAttribute("aria-description", "Stopped");
  await expect(stopped).toHaveCount(1);
  await client.startTurn(parent.id, {
    requestId: crypto.randomUUID(),
    text: '!tool task {"description":"Validate an incomplete delegation"}',
  });
  const failed = page
    .locator(".agent-task")
    .filter({ hasText: "Validate an incomplete delegation" });
  await expect(failed).toHaveAttribute("aria-description", "Failed");
  await expect(failed).toBeDisabled();
  await expect(failed).toHaveCount(1);
  await expect
    .poll(() => client.activeTurns().then((turns) => turns.length))
    .toBe(0);
});

test("invalid tool arguments recover in the same chat and corrected names read files", async () => {
  await page
    .locator(".project-list .project-heading")
    .filter({ hasText: /^workspace$/ })
    .click();
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
  );
  await writeFile(
    join(workspace, "tool-recovery.txt"),
    "tool-recovery-file-proof",
  );
  await newSession();
  await send('!tool Write {"path":"invalid-write.txt","content":17}');
  await expect(page.locator(".transcript")).toContainText(
    "content: expected string",
  );
  await expect(
    page.getByRole("button", { name: "Send message", exact: true }),
  ).toBeVisible();
  expect(await client.listPermissions()).toHaveLength(0);
  await expect(
    readFile(join(workspace, "invalid-write.txt"), "utf8"),
  ).rejects.toThrow();
  await send('!tool FUNCTIONS.Read {"path":"tool-recovery.txt"}');
  await expect(page.locator(".transcript")).toContainText(
    "tool-recovery-file-proof",
  );
  await expect(
    page.getByRole("button", { name: "Send message", exact: true }),
  ).toBeVisible();
});

test("tool activity shows readable commands, batch steps, outputs and failures", async () => {
  const heading = page
    .locator(".project-list .project-heading")
    .filter({ hasText: /^workspace$/ });
  if ((await heading.getAttribute("aria-expanded")) === "false")
    await heading.click();
  const thread = await client.createThread({
    title: "Readable tool activity",
    provider: "echo",
    model: "echo-1",
  });
  await page
    .locator(".sessions")
    .getByRole("button", { name: "Readable tool activity", exact: true })
    .click();
  await writeFile(
    join(workspace, "src", "activity.json"),
    '{"readable":true,"literal":"\\n"}\n',
  );
  const calls = [
    { tool: "list", parameters: { path: "src" } },
    { tool: "glob", parameters: { pattern: "**/*.ts", path: "src" } },
    { tool: "read", parameters: { path: "src/activity.json" } },
    { tool: "read", parameters: { path: "does-not-exist.txt" } },
  ];
  const turn = await client.startTurn(thread.id, {
    requestId: crypto.randomUUID(),
    text: "!tool batch " + JSON.stringify({ tool_calls: calls }),
  });
  await client.waitTurn(turn.id);
  const batch = page.locator(".tool-batch");
  await expect(batch).toHaveCount(1);
  await expect(batch.locator(":scope > summary")).toContainText(
    "Listed files, searched files, read files",
  );
  // Collapsed, the row still names what its steps acted on.
  await expect(batch.locator(":scope > summary .tool-target")).toHaveText(
    "src, **/*.ts in src +2",
  );
  await expect(batch).toHaveAttribute("data-status", "failed");
  await expect(batch).not.toHaveAttribute("open");
  await batch.locator(":scope > summary").press("Enter");
  const children = batch.locator(".tool-child");
  await expect(children).toHaveCount(4);
  await expect(batch.locator("pre")).toHaveCount(0);
  await expect(children.nth(1).locator("summary")).toContainText(
    "**/*.ts in src",
  );
  await children.nth(0).locator("summary").click();
  await expect(children.nth(0).locator(".tool-output")).toContainText(
    "greeting.ts",
  );
  await expect(children.nth(0).locator(".tool-input")).toHaveText("Path: src");
  await children.nth(2).locator("summary").click();
  await expect(children.nth(2).locator(".tool-output")).toContainText(
    '{"readable":true,"literal":"\\n"}',
  );
  await children.nth(3).locator("summary").click();
  await expect(children.nth(3)).toHaveAttribute("data-status", "failed");
  await expect(children.nth(3).locator(".tool-output")).toContainText(
    /not found|No such file|does not exist/i,
  );
  const neutralMark = await children
    .nth(0)
    .locator(".tool-mark")
    .evaluate((element) => getComputedStyle(element).color);
  const neutralOutput = await children
    .nth(0)
    .locator(".tool-output")
    .evaluate((element) => getComputedStyle(element).color);
  await expect(batch.locator(":scope > summary .tool-status")).toHaveCount(0);
  await expect(batch.locator(":scope > summary .tool-mark")).toHaveCSS(
    "color",
    neutralMark,
  );
  await expect(children.nth(3).locator(".tool-status")).toHaveCount(0);
  await expect(children.nth(3).locator(".tool-mark")).toHaveCSS(
    "color",
    neutralMark,
  );
  await expect(children.nth(3).locator(".tool-output")).toHaveCSS(
    "color",
    neutralOutput,
  );
  const failedBatchRail = await batch.evaluate(
    (element) => getComputedStyle(element).borderLeftColor,
  );
  await expect(batch).not.toContainText('"tool_calls"');
  await expect(batch).not.toContainText('"metadata"');
  await batch.screenshot({ path: "test-results/readable-batch.png" });
  await page.reload();
  await expect(batch).toHaveAttribute("data-status", "failed");
  await batch.locator(":scope > summary").click();
  await expect(children).toHaveCount(4);
  await batch.locator(":scope > summary").press("Space");
  await expect(batch).not.toHaveAttribute("open");

  const command =
    "printf 'first line\\nsecond line\\n'\nprintf 'stderr proof\\n' >&2";
  const shell = await client.startTurn(thread.id, {
    requestId: crypto.randomUUID(),
    text: "!tool bash " + JSON.stringify({ command }),
  });
  const activity = page.locator(".message.step > .tool-call").last();
  await expect(activity.locator(":scope > summary")).toContainText("printf");
  // The approval card docks first and moves the step up; then it is opened.
  await expect(page.getByRole("alert", { name: "Permission request" })).toBeVisible();
  await activity.locator(":scope > summary").click();
  await expect(activity.locator(".tool-empty")).toHaveText(
    "Waiting for output…",
  );
  await expect
    .poll(() =>
      client
        .listPermissions()
        .then(
          (values) =>
            values.find((value) => value.threadId === thread.id)?.permissionId,
        ),
    )
    .toBeTruthy();
  const permission = (await client.listPermissions()).find(
    (value) => value.threadId === thread.id,
  )!;
  await client.replyPermission(permission.permissionId, { decision: "once" });
  await client.waitTurn(shell.id);
  await expect(activity).toHaveAttribute("open");
  await expect(activity.locator(".tool-console-label")).toHaveText("Shell");
  await expect(activity.locator(".tool-input")).toHaveText("$ " + command);
  await expect(activity.locator(".tool-output")).toContainText(
    "first line\nsecond line",
  );
  await expect(activity.locator(".tool-output")).toContainText("stderr proof");
  await expect(activity.locator(".tool-console")).toHaveCount(1);
  await expect(activity).not.toContainText('"command":');
  await activity.screenshot({ path: "test-results/readable-shell.png" });
  const size = await app.evaluate(({ BrowserWindow }) =>
    BrowserWindow.getAllWindows()[0].getSize(),
  );
  await app.evaluate(({ BrowserWindow }) =>
    BrowserWindow.getAllWindows()[0].setSize(900, 650),
  );
  await expect(activity.locator(".tool-console")).toBeInViewport();
  expect(
    await activity.evaluate(
      (element) => element.scrollWidth <= element.clientWidth + 2,
    ),
  ).toBe(true);
  await activity.screenshot({ path: "test-results/readable-shell-narrow.png" });
  await app.evaluate(
    ({ BrowserWindow }, size) =>
      BrowserWindow.getAllWindows()[0].setSize(size[0], size[1]),
    size,
  );

  const edit = await client.startTurn(thread.id, {
    requestId: crypto.randomUUID(),
    text: '!tool edit {"path":"src/activity.json","old":"true","new":"false"}',
  });
  await client.waitTurn(edit.id);
  await expect(activity.locator(":scope > summary")).toContainText("Edited");
  await activity.locator(":scope > summary").click();
  await expect(activity.locator(".change-preview")).toBeVisible();
  await expect(activity.locator(".change-lines .add code")).toHaveText("false");
  await expect(activity.locator(".change-lines .remove code")).toHaveText("true");
  await expect(activity.locator(".change-lines .add .diff-sign")).toHaveText("+");
  await expect(activity.locator(".change-lines .remove .diff-sign")).toHaveText("−");
  await expect(activity).not.toContainText('"old":');
  // Opening a finished step whose body pushed the end out of view paused
  // following; closing it takes following back up for the next turn.
  await activity.locator(":scope > summary").click();
  await expect(activity.locator(".change-preview")).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Jump to latest", exact: true })).toHaveCount(0);
  const waiting = await client.startTurn(thread.id, {
    requestId: crypto.randomUUID(),
    text:
      "!tool batch " +
      JSON.stringify({
        tool_calls: [
          { tool: "read", parameters: { path: "src/activity.json" } },
          {
            tool: "question",
            parameters: {
              prompt: "Finish activity verification?",
              choices: ["Continue"],
            },
          },
        ],
      }),
  });
  await expect
    .poll(() =>
      client
        .listQuestions()
        .then(
          (values) =>
            values.find((value) => value.threadId === thread.id)?.questionId,
        ),
    )
    .toBeTruthy();
  const live = page.locator(".tool-batch").last();
  await expect(live.locator(":scope > summary")).toContainText("Read files");
  // The question docks first and the view follows it down; then the batch
  // is opened, so nothing moves between aiming and clicking.
  await expect(page.locator(".prompt-dock.question")).toBeVisible();
  await expect
    .poll(() => page.locator(".transcript").evaluate((element) => element.scrollHeight - element.scrollTop - element.clientHeight))
    .toBeLessThan(5);
  await live.locator(":scope > summary").click();
  // Its steps draw a moment after it opens, and the batch then comes up
  // for them to show: aim at the first one once it has stopped there.
  const reading = live.locator(".tool-child").first();
  await expect(reading.locator("summary")).toBeInViewport({ ratio: 1 });
  let aimed = "";
  await expect
    .poll(async () => {
      const box = JSON.stringify(await reading.locator("summary").boundingBox());
      const still = box === aimed;
      aimed = box;
      return still;
    })
    .toBe(true);
  await reading.locator("summary").click();
  await expect(reading.locator(".tool-empty")).toHaveText(
    "Waiting for output…",
  );
  const question = (await client.listQuestions()).find(
    (value) => value.threadId === thread.id,
  )!;
  await client.replyQuestion(question.questionId, { answer: "Continue" });
  await client.waitTurn(waiting.id);
  await expect(reading).toHaveAttribute("open");
  await expect(reading.locator(".tool-output")).toContainText(
    '"readable":false',
  );
  await expect(live).toHaveCSS("border-left-color", failedBatchRail);
  const neutralRail = await live.evaluate(
    (element) => getComputedStyle(element).borderLeftColor,
  );
  expect(failedBatchRail).toBe(neutralRail);
  // Steps opened above paused following, as scrolling up would; the next
  // turn is read from the latest again.
  const jumpToLatest = page.getByRole("button", { name: "Jump to latest", exact: true });
  if (await jumpToLatest.isVisible()) await jumpToLatest.click();
  await expect(jumpToLatest).toHaveCount(0);
  const missing = await client.startTurn(thread.id, {
    requestId: crypto.randomUUID(),
    text: '!tool glob {"pattern":"**/*.{yml,yaml,json,toml,lock}","path":".missing-tools-folder"}',
  });
  await client.waitTurn(missing.id);
  const failedTool = page.locator(".message.step > .tool-call").last();
  await expect(failedTool).toHaveAttribute("data-status", "failed");
  await failedTool.locator(":scope > summary").click();
  await expect(failedTool.locator(".tool-status")).toHaveCount(0);
  await expect(failedTool).toHaveCSS("border-left-color", neutralRail);
  await expect(failedTool.locator(".tool-mark")).toHaveCSS(
    "color",
    neutralMark,
  );
  await expect(failedTool.locator(".tool-output")).toHaveCSS(
    "color",
    neutralOutput,
  );
  await expect(failedTool.locator(".tool-output")).toContainText(
    /not found|No such file|does not exist/i,
  );
  await failedTool.screenshot({ path: "test-results/neutral-tool-error.png" });
  expect(errors).toEqual([]);
});

test("a tool row's tooltip shows a multi-line command as written", async () => {
  await expandPrimaryProject();
  const thread = await client.createThread({
    title: "Multi-line command tooltip",
    provider: "echo",
    model: "echo-1",
  });
  await page
    .locator(".sessions")
    .getByRole("button", { name: "Multi-line command tooltip", exact: true })
    .click();
  const command = "printf one\nprintf two";
  const turn = await client.startTurn(thread.id, {
    requestId: crypto.randomUUID(),
    text: "!tool bash " + JSON.stringify({ command }),
  });
  // Only the call's arguments matter: refuse it if it asks.
  let finished = false;
  const done = client.waitTurn(turn.id).then(() => (finished = true));
  while (!finished) {
    const asked = (await client.listPermissions()).find((item) => item.threadId === thread.id);
    if (asked) await client.replyPermission(asked.permissionId, { decision: "reject" });
    await Promise.race([done, new Promise((resolve) => setTimeout(resolve, 200))]);
  }
  const label = page
    .locator(".message.step > .tool-call")
    .last()
    .locator(":scope > summary .tool-target");
  await expect(label).toHaveText("printf one printf two");
  await expect(label).toHaveAttribute("title", command);
  expect(errors).toEqual([]);
});

test("dragging across a tool row's label selects it without opening or closing the row", async () => {
  await expandPrimaryProject();
  const thread = await client.createThread({
    title: "Selectable tool rows",
    provider: "echo",
    model: "echo-1",
  });
  await page
    .locator(".sessions")
    .getByRole("button", { name: "Selectable tool rows", exact: true })
    .click();
  const turn = await client.startTurn(thread.id, {
    requestId: crypto.randomUUID(),
    text: "!tool read " + JSON.stringify({ path: "src/greeting.ts" }),
  });
  await client.waitTurn(turn.id);
  const row = page.locator(".message.step > .tool-call").last();
  const label = row.locator(":scope > summary .tool-target");
  await expect(label).toContainText("greeting.ts");
  // An opened row draws its body a moment later, and the transcript keeps
  // its end in view, so the label moves up: measure it once it is still.
  const settled = async () => {
    let last = "";
    for (let read = 0; read < 50; read++) {
      const box = (await label.boundingBox())!;
      if (JSON.stringify(box) === last) return box;
      last = JSON.stringify(box);
      await page.waitForTimeout(100);
    }
    return (await label.boundingBox())!;
  };
  const drag = async () => {
    const box = await settled();
    const y = box.y + box.height / 2;
    await page.mouse.move(box.x + 1, y);
    await page.mouse.down();
    for (let step = 1; step <= 8; step++)
      await page.mouse.move(box.x + 1 + ((box.width - 2) * step) / 8, y);
    await page.mouse.up();
  };
  const selected = () => page.evaluate(() => getSelection()?.toString() ?? "");
  await drag();
  expect(await selected()).toContain("greeting");
  await expect(row).not.toHaveAttribute("open");
  // A plain click still opens the row; selecting in it then leaves it open.
  await page.evaluate(() => getSelection()?.removeAllRanges());
  await label.click();
  await expect(row).toHaveAttribute("open", "");
  await drag();
  expect(await selected()).toContain("greeting");
  await expect(row).toHaveAttribute("open", "");
  // The keyboard still toggles it.
  await row.locator(":scope > summary").press("Enter");
  await expect(row).not.toHaveAttribute("open");
  expect(errors).toEqual([]);
});

test("bundled ripgrep searches from chat and the integrated terminal", async () => {
  const heading = page
    .locator(".project-list .project-heading")
    .filter({ hasText: /^workspace$/ });
  if ((await heading.getAttribute("aria-expanded")) === "false")
    await heading.click();
  await mkdir(join(workspace, "rg space"), { recursive: true });
  await writeFile(
    join(workspace, "rg space", "café.txt"),
    "first\npackaged-ripgrep-proof café\n",
  );
  const thread = await client.createThread({
    title: "Bundled ripgrep",
    provider: "echo",
    model: "echo-1",
  });
  await page
    .locator(".sessions")
    .getByRole("button", { name: "Bundled ripgrep", exact: true })
    .click();
  const command =
    'command -v rg; rg --version; rg -n packaged-ripgrep-proof "rg space"';
  await send("!tool bash " + JSON.stringify({ command }));
  await expect
    .poll(() =>
      client
        .listPermissions()
        .then((values) => values.find((value) => value.threadId === thread.id)),
    )
    .toBeTruthy();
  const permission = (await client.listPermissions()).find(
    (value) => value.threadId === thread.id,
  )!;
  await client.replyPermission(permission.permissionId, { decision: "once" });
  await expect.poll(() => client.activeTurn(thread.id)).toBeNull();
  const activity = page.locator(".message.step > .tool-call").last();
  await activity.locator(":scope > summary").click();
  const output = activity.locator(".tool-output");
  await expect(output).toContainText("ripgrep 15.2.0");
  await expect(output).toContainText("café.txt:2:packaged-ripgrep-proof café");
  await expect(output).toContainText("/ripgrep/15.2.0/");
  await activity.screenshot({ path: "test-results/bundled-ripgrep-chat.png" });
  const hide = page.getByRole("button", { name: "Hide terminal", exact: true });
  if (await hide.count()) await hide.click();
  for (const terminal of await client.listPtys())
    await client.deletePty(terminal.id);
  // A plain shell avoids user startup files replacing the PATH supplied by
  // the PTY service. The normal shell-launch flow is covered separately.
  await client.createPty({ command: "/bin/sh", args: [], cols: 80, rows: 24, title: "Bundled ripgrep shell" });
  await page
    .getByRole("button", { name: "Toggle terminal", exact: true })
    .click();
  const input = page.locator(".xterm-helper-textarea");
  await expect(input).toBeFocused();
  await input.fill(
    'command -v rg > rg-terminal-path.txt; rg -n packaged-ripgrep-proof "rg space/café.txt" > rg-terminal-result.txt',
  );
  await input.press("Enter");
  await expect
    .poll(() =>
      readFile(join(workspace, "rg-terminal-result.txt"), "utf8").catch(
        () => "",
      ),
    )
    .toBe("2:packaged-ripgrep-proof café\n");
  expect(
    await readFile(join(workspace, "rg-terminal-path.txt"), "utf8"),
  ).toContain("/ripgrep/15.2.0/");
  await hide.click();
  for (const terminal of await client.listPtys())
    await client.deletePty(terminal.id);
});

test("universal search opens settings controls, shortcuts, files and commands directly", async () => {
  test.setTimeout(180000);
  const find = async (text: string) => {
    if (
      await page.getByRole("dialog", { name: "Settings", exact: true }).count()
    ) {
      await app.evaluate(({ BrowserWindow }) => {
        BrowserWindow.getAllWindows()[0].webContents.send("haarness:event", {
          kind: "menu",
          data: "palette",
        });
      });
    } else
      await page.getByRole("button", { name: "Search", exact: true }).click();
    const input = page.getByRole("combobox", {
      name: "Search Haarness",
      exact: true,
    });
    await expect(input).toBeFocused();
    await input.fill(text);
    return input;
  };
  const settings = page.getByRole("dialog", { name: "Settings", exact: true });
  for (const title of [
    "General",
    "Keyboard shortcuts",
    "Servers",
    "Projects",
    "Profiles",
    "Providers",
    "Models",
    "Agents",
    "Commands",
    "Skills",
    "Tools",
    "MCP servers",
    "Plugins",
    "Extensions",
    "Schedules",
    "About",
  ]) {
    await find("settings " + title);
    await page.getByRole("option", { name: title, exact: true }).click();
    await expect(
      settings.getByRole("heading", { name: title, exact: true }),
    ).toBeVisible();
  }
  let input = await find("terminal font");
  await input.press("Enter");
  await expect(
    settings.getByRole("spinbutton", {
      name: "Terminal text size",
      exact: true,
    }),
  ).toBeFocused();
  await expect(
    settings.locator('[data-setting="Terminal text size"]'),
  ).toHaveClass(/setting-search-target/);
  input = await find("wrap editor");
  await input.press("Enter");
  await expect(
    settings.getByRole("switch", { name: "Wrap editor lines", exact: true }),
  ).toBeFocused();
  // Esc closes only Search and restores focus inside the existing settings pane.
  input = await find("no result zzzzq");
  await expect(page.getByRole("option")).toHaveCount(0);
  await input.press("Escape");
  await expect(settings).toBeVisible();
  await expect(
    settings.getByRole("switch", { name: "Wrap editor lines", exact: true }),
  ).toBeFocused();
  await find("Shortcut: New chat");
  await page
    .getByRole("option", { name: "Shortcut: New chat", exact: true })
    .click();
  const shortcut = settings.getByRole("textbox", {
    name: "Shortcut new",
    exact: true,
  });
  await expect(shortcut).toBeFocused();
  const original = await shortcut.inputValue();
  await shortcut.fill("Mod+Shift+L");
  await shortcut.press("Tab");
  await find("new chat");
  await expect(
    page.getByRole("option", { name: "New chat", exact: true }),
  ).toContainText(process.platform === "darwin" ? "⌘ ⇧ L" : "Ctrl+Shift+L");
  await page
    .getByRole("combobox", { name: "Search Haarness", exact: true })
    .press("Escape");
  await shortcut.fill(original);
  await shortcut.press("Tab");
  await settings
    .getByRole("button", { name: "Close settings", exact: true })
    .click();
  await newSession();
  const composer = page.getByRole("textbox", { name: "Message", exact: true });
  await composer.fill("Keep this draft");
  await find("/ui-layout-short");
  await page
    .getByRole("option", { name: "/ui-layout-short", exact: true })
    .click();
  await expect(composer).toHaveValue("/ui-layout-short Keep this draft");
  await expect(composer).toBeFocused();
  const greeting = await readFile(join(workspace, "src/greeting.ts"), "utf8");
  await find("greeting.ts");
  await page.getByRole("option", { name: "greeting.ts", exact: true }).click();
  // Earlier editor scenarios intentionally save this shared fixture. Search
  // must open its current content, rather than an obsolete initial value.
  await expect(page.locator(".cm-content")).toHaveText(greeting);
  await find("New file");
  await page.getByRole("option", { name: "New file", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "New file", exact: true });
  await expect(dialog).toBeVisible();
  await dialog.getByRole("button", { name: "Cancel", exact: true }).click();
  await find("terminal font");
  await page.screenshot({ path: "test-results/universal-search-settings.png" });
  await page
    .getByRole("combobox", { name: "Search Haarness", exact: true })
    .fill("");
  await page.screenshot({ path: "test-results/universal-search.png" });
  await page
    .getByRole("combobox", { name: "Search Haarness", exact: true })
    .press("Escape");
  await composer.fill("");
  expect(errors).toEqual([]);
});

test("universal search keeps each result's row while other chats change, so a click on it lands", async () => {
  const target = await client.createThread({ title: "Steadyrow result", provider: "echo", model: "echo-1" });
  const busy = await client.createThread({ title: "Steadyrow neighbour", provider: "echo", model: "echo-1" });
  await page.getByRole("button", { name: "Search", exact: true }).click();
  await page.getByRole("combobox", { name: "Search Haarness", exact: true }).fill("Steadyrow");
  const option = page.getByRole("option", { name: "Steadyrow result", exact: true });
  await expect(option).toBeVisible();
  const row = (await option.elementHandle())!;
  // Activity in another chat rebuilds the results: a rename, then a whole turn.
  await client.updateThread(busy.id, { title: "Steadyrow neighbour renamed" });
  await expect(page.getByRole("option", { name: "Steadyrow neighbour renamed", exact: true })).toBeVisible();
  await client.startTurn(busy.id, { text: "keep the results busy", requestId: crypto.randomUUID() });
  await expect.poll(() => client.activeTurns().then((turns) => turns.length)).toBe(0);
  expect(await row.evaluate((element) => element.isConnected)).toBe(true);
  // A change to the result's own chat shows in the same row.
  await client.updateThread(target.id, { title: "Steadyrow result renamed" });
  await expect(page.getByRole("option", { name: "Steadyrow result renamed", exact: true })).toBeVisible();
  expect(await row.evaluate((element) => element.isConnected && element.getAttribute("aria-label"))).toBe("Steadyrow result renamed");
  await row.click();
  await expect(page.getByRole("dialog", { name: "Universal search" })).toHaveCount(0);
  await expect(page.locator(".session-bar-name, .workspace-tab.active")).toContainText("Steadyrow result renamed");
  await client.updateThread(busy.id, { archived: true });
  expect(errors).toEqual([]);
});

test("universal search includes all conversations and navigates collapsed projects and worktrees", async () => {
  test.setTimeout(180000);
  const retained = [];
  for (let i = 0; i < 12; i++)
    retained.push(
      await client.createThread({
        title: `Universal history ${i}`,
        provider: "echo",
        model: "echo-1",
      }),
    );
  await client.updateThread(retained[11].id, { archived: true });
  const checkout = (await client.createWorkspace({ name: "Search feature" }))
    .workspace;
  extraWorkspaces.push(checkout.path);
  const linked = JSON.parse(
    execFileSync(
      binary,
      ["--workspace", checkout.path, "--trust-project", "desktop"],
      {
        env,
        input: JSON.stringify({ action: "connect" }),
        encoding: "utf8",
        timeout: 30000,
      },
    ),
  );
  const child = new Haarness({ baseUrl: linked.url, token: linked.token });
  const thread = await child.createThread({
    title: "Universal feature conversation",
    provider: "echo",
    model: "echo-1",
  });
  await child.startTurn(thread.id, {
    text: "Worktree history marker",
    requestId: crypto.randomUUID(),
  });
  await expect
    .poll(() => child.activeTurns().then((turns) => turns.length))
    .toBe(0);
  child.close();
  await page.evaluate(async () => {
    await window.haarness.projects();
  });
  const find = async (text: string) => {
    await page.getByRole("button", { name: "Search", exact: true }).click();
    const input = page.getByRole("combobox", {
      name: "Search Haarness",
      exact: true,
    });
    await input.fill(text);
    return input;
  };
  // Refresh through the same application action users have, then collapse it.
  await find("Refresh project");
  await page
    .getByRole("option", { name: "Refresh project", exact: true })
    .click();
  const project = page.locator(
    '.project-group[aria-label="Project workspace"]',
  );
  if (
    (await project
      .getByRole("button", { name: "workspace", exact: true })
      .getAttribute("aria-expanded")) === "true"
  )
    await project
      .getByRole("button", { name: "Collapse project workspace", exact: true })
      .click();
  await find("Universal history");
  await expect(
    page.getByRole("option", { name: /^Universal history \d+$/ }),
  ).toHaveCount(12);
  await page
    .getByRole("option", { name: "Universal history 11", exact: true })
    .click();
  await expect(
    page.locator(".session-bar-name, .workspace-tab.active"),
  ).toContainText("Universal history 11");
  await find("Universal feature conversation");
  const featureResult = page.getByRole("option", { name: "Universal feature conversation", exact: true });
  await expect(featureResult).toHaveAttribute("aria-description", "workspace · Search feature");
  await expect(featureResult).not.toContainText(checkout.path);
  await page.screenshot({ path: test.info().outputPath("chat-search-without-paths.png"), animations: "disabled" });
  await page
    .getByRole("option", {
      name: "Universal feature conversation",
      exact: true,
    })
    .click();
  await expect(page.locator(".titlebar .checkout-badge")).toHaveText(
    "Search feature",
  );
  await expect(page.locator(".transcript")).toContainText(
    "Worktree history marker",
  );
  await find("workspace");
  await page.getByRole("option", { name: "workspace", exact: true }).click();
  await expect(page.locator(".titlebar .checkout-badge")).toHaveCount(0);
  expect(errors).toEqual([]);
});

test("closing Electron detaches from ongoing server work", async () => {
  await page.screenshot({ path: "test-results/desktop.png" });
  const thread = await client.createThread({
    title: "Detached work",
    provider: "echo",
    model: "echo-1",
  });
  const turn = await client.startTurn(thread.id, {
    requestId: crypto.randomUUID(),
    text: '!tool bash {"command":"printf detached-proof"}',
  });
  await expect
    .poll(async () => (await client.listPermissions()).length)
    .toBe(1);
  await app.close();
  expect((await client.health()).status).toBe("ok");
  expect((await client.getTurn(turn.id)).status).toBe("running");
  const permission = (await client.listPermissions())[0];
  await client.replyPermission(permission.permissionId, { decision: "once" });
  expect((await client.waitTurn(turn.id)).status).toBe("completed");
  const executablePath = process.env.HAARNESS_DESKTOP_EXECUTABLE;
  const launchEnv = { ...env };
  if (executablePath) delete launchEnv.HAARNESS_BIN;
  app = await electron.launch({ ...(executablePath ? { executablePath, args: [] } : { args: ["."] }), env: launchEnv });
  page = await app.firstWindow();
  page.on("pageerror", (error) => errors.push(error.message));
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
});

test("structured asset editors preserve scope, permissions, tool overrides and command arguments", async () => {
  // A prior scenario may have restored a worktree after restarting Electron.
  // Select this test's owning folder through the normal project-opening flow.
  await app.evaluate(({ dialog }, directory) => {
    const original = dialog.showOpenDialog;
    dialog.showOpenDialog = async () => {
      dialog.showOpenDialog = original;
      return { canceled: false, filePaths: [directory] };
    };
  }, workspace);
  await page.getByRole("button", { name: "Open another project", exact: true }).click();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
  await expect(page.locator(".titlebar .checkout-badge")).toHaveCount(0);
  const native = (kind: string, input: Record<string, unknown>) =>
    page.evaluate(
      ({ directory, kind, input }) =>
        window.haarness.native("local:" + directory, {
          action: "asset",
          kind,
          input,
        }),
      { directory: workspace, kind, input },
    );
  for (const scope of ["global", "local"])
    await native("command", {
      action: "create",
      name: "form-scope",
      scope,
      description: "Scoped form fixture",
      template: scope + " original",
    });
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  const settings = page.getByRole("dialog", { name: "Settings", exact: true });
  await settings.getByRole("button", { name: "Commands", exact: true }).click();
  const global = settings
    .locator(".setting-row")
    .filter({ hasText: "form-scope" })
    .filter({ hasText: "All projects" });
  await global.getByRole("button", { name: "Edit" }).click();
  let form = page.getByRole("dialog", { name: "Edit command", exact: true });
  await expect(form.getByLabel("Prompt template", { exact: true })).toHaveValue(
    "global original",
  );
  await expect(
    form.getByRole("combobox", { name: "Available in" }),
  ).toBeDisabled();
  await form
    .getByLabel("Prompt template", { exact: true })
    .fill("global updated");
  await form.getByRole("button", { name: "Save changes", exact: true }).click();
  await expect(form).toHaveCount(0);
  expect(
    await native("command", {
      action: "get",
      name: "form-scope",
      scope: "global",
    }),
  ).toMatchObject({ template: "global updated" });
  expect(
    await native("command", {
      action: "get",
      name: "form-scope",
      scope: "local",
    }),
  ).toMatchObject({ template: "local original" });

  await settings.getByRole("button", { name: "Agents", exact: true }).click();
  await settings
    .getByRole("button", { name: "Create agent", exact: true })
    .click();
  form = page.getByRole("dialog", { name: "Create agent", exact: true });
  await form.getByLabel("Name", { exact: true }).fill("invalid/name");
  await form
    .getByLabel("Description", { exact: true })
    .fill("Form agent verification");
  await form
    .getByLabel("Instructions", { exact: true })
    .fill("Return the requested fixture evidence.");
  await selectValue(
    form.getByRole("combobox", { name: "Available in" }),
    "local",
  );
  await selectValue(
    form.getByRole("combobox", { name: "Available as" }),
    "primary",
  );
  const model = form.getByRole("combobox", { name: "Model", exact: true });
  await model.click();
  await expect(
    page
      .getByRole("option")
      .filter({ hasText: /disconnected|Desktop credential fixture/ }),
  ).toHaveCount(0);
  await page.keyboard.press("Escape");
  await selectValue(model, "echo/echo-1");
  await form.getByText("Tool access", { exact: false }).first().click();
  await form
    .getByRole("button", { name: "Add tool override", exact: true })
    .click();
  await selectValue(
    form.getByRole("combobox", { name: "Tool 1", exact: true }),
    "bash",
  );
  await selectValue(
    form.getByRole("combobox", { name: "Tool 1 access", exact: true }),
    "disabled",
  );
  await form.getByRole("button", { name: "Create", exact: true }).click();
  await expect(form.getByRole("alert")).toContainText(/name/i);
  await expect(form.getByLabel("Instructions", { exact: true })).toHaveValue(
    "Return the requested fixture evidence.",
  );
  await form.getByLabel("Name", { exact: true }).fill("form-reviewer");
  await page.screenshot({ path: test.info().outputPath("agent-editor.png") });
  const viewport = page.viewportSize();
  await page.setViewportSize({ width: 540, height: 840 });
  expect(
    await form.evaluate((el) => el.scrollWidth <= el.clientWidth + 1),
  ).toBe(true);
  await page.screenshot({
    path: test.info().outputPath("agent-editor-narrow.png"),
  });
  await page.setViewportSize(viewport ?? { width: 1420, height: 940 });
  await form.getByRole("button", { name: "Create", exact: true }).click();
  await expect(form).toHaveCount(0);
  expect(
    await native("agent", {
      action: "get",
      name: "form-reviewer",
      scope: "local",
    }),
  ).toMatchObject({
    mode: "primary",
    model: "echo/echo-1",
    tools: { bash: false },
  });
  // Unknown metadata and permissions must survive a normal UI edit.
  const agentPath = join(workspace, ".haarness/agents/form-reviewer.md");
  const saved = await readFile(agentPath, "utf8");
  await writeFile(
    agentPath,
    saved.replace(
      "---\n",
      "---\ncustom_metadata: keep-me\npermissions:\n  bash: deny\n",
    ),
  );
  const prior = (await native("agent", {
    action: "get",
    name: "form-reviewer",
    scope: "local",
  })) as Record<string, unknown>;
  await settings
    .locator(".setting-row")
    .filter({ hasText: "form-reviewer" })
    .getByRole("button", { name: "Edit" })
    .click();
  form = page.getByRole("dialog", { name: "Edit agent", exact: true });
  await form
    .getByLabel("Instructions", { exact: true })
    .fill("Updated instructions.");
  await form.getByRole("button", { name: "Save changes", exact: true }).click();
  await expect(form).toHaveCount(0);
  const after = (await native("agent", {
    action: "get",
    name: "form-reviewer",
    scope: "local",
  })) as Record<string, unknown>;
  expect(after.permissions).toEqual(prior.permissions);
  expect(after.tools).toEqual({ bash: false });
  expect(await readFile(agentPath, "utf8")).toContain(
    "custom_metadata: keep-me",
  );

  await settings.getByRole("button", { name: "Commands", exact: true }).click();
  await settings
    .getByRole("button", { name: "Create command", exact: true })
    .click();
  form = page.getByRole("dialog", { name: "Create command", exact: true });
  await form.getByLabel("Name", { exact: true }).fill("form-command");
  await form
    .getByLabel("Description", { exact: true })
    .fill("Structured command verification");
  await form.getByLabel("Prompt template", { exact: true }).fill("Review");
  await form
    .getByRole("button", { name: "Insert arguments", exact: true })
    .click();
  await expect(form.getByLabel("Prompt template", { exact: true })).toHaveValue(
    "Review $ARGUMENTS",
  );
  await form
    .getByLabel("Argument hint", { exact: true })
    .fill("file or request");
  await selectValue(
    form.getByRole("combobox", { name: "Agent", exact: true }),
    "form-reviewer",
  );
  await selectValue(
    form.getByRole("combobox", { name: "Model", exact: true }),
    "echo/echo-1",
  );
  await selectValue(
    form.getByRole("combobox", { name: "Available in" }),
    "local",
  );
  await page.screenshot({ path: test.info().outputPath("command-editor.png") });
  await form.getByRole("button", { name: "Create", exact: true }).click();
  await expect(form).toHaveCount(0);
  expect(
    await native("command", {
      action: "get",
      name: "form-command",
      scope: "local",
    }),
  ).toMatchObject({
    agent: "form-reviewer",
    model: "echo/echo-1",
    argumentHint: "file or request",
    template: "Review $ARGUMENTS",
  });
  await settings.getByRole("button", { name: "Skills", exact: true }).click();
  await settings
    .getByRole("button", { name: "Create skill", exact: true })
    .click();
  form = page.getByRole("dialog", { name: "Create skill", exact: true });
  await form.getByLabel("Name", { exact: true }).fill("form-skill");
  await form
    .getByLabel("Description", { exact: true })
    .fill("Form skill verification");
  await form
    .getByLabel("Instructions", { exact: true })
    .fill("Use the fixture instructions.");
  await form
    .getByLabel("Notes (optional)", { exact: true })
    .fill("Keep these setup notes.");
  await form.getByRole("button", { name: "Create", exact: true }).click();
  await expect(form).toHaveCount(0);
  expect(
    await native("skill", {
      action: "get",
      name: "form-skill",
      scope: "global",
    }),
  ).toMatchObject({ notes: "Keep these setup notes." });
  await settings.getByRole("button", { name: "Close settings" }).click();
  await expect
    .poll(
      async () =>
        (await client.commands()).find((item) => item.name === "form-command")
          ?.agent,
    )
    .toBe("form-reviewer");
  await newSession();
  await send("/form-command src/greeting.ts");
  await expect(page.locator(".transcript")).toContainText(
    "Echo: Review src/greeting.ts",
  );
  await expect.poll(async () => (await client.activeTurns()).length).toBe(0);
  expect(errors).toEqual([]);
});

test("custom provider form discovers authenticated models and runs them without manual identifiers", async () => {
  const received: { path?: string; authorization?: string; model?: string }[] =
    [];
  const server = createServer(async (request, response) => {
    if (request.method === "GET") {
      received.push({
        path: request.url,
        authorization: request.headers.authorization,
      });
      response
        .writeHead(200, { "Content-Type": "application/json" })
        .end(JSON.stringify({ data: [{ id: "form-model" }] }));
      return;
    }
    const chunks: Buffer[] = [];
    for await (const chunk of request) chunks.push(Buffer.from(chunk));
    const body = JSON.parse(Buffer.concat(chunks).toString());
    received.push({
      path: request.url,
      authorization: request.headers.authorization,
      model: body.model,
    });
    response.writeHead(200, { "Content-Type": "text/event-stream" }).end(
      "data: " +
        JSON.stringify({
          choices: [
            {
              index: 0,
              delta: { content: "provider-form-execution-proof" },
              finish_reason: null,
            },
          ],
        }) +
        "\n\ndata: " +
        JSON.stringify({
          choices: [{ index: 0, delta: {}, finish_reason: "stop" }],
        }) +
        "\n\ndata: [DONE]\n\n",
    );
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  if (!address || typeof address === "string")
    throw Error("No provider fixture");
  const configPath = join(workspace, ".haarness/config.toml");
  const config = await readFile(configPath, "utf8");
  try {
    await page.getByRole("button", { name: "Settings", exact: true }).click();
    const settings = page.getByRole("dialog", {
      name: "Settings",
      exact: true,
    });
    await settings
      .getByRole("button", { name: "Providers", exact: true })
      .click();
    await settings
      .getByRole("button", { name: "Add custom provider", exact: true })
      .click();
    const form = page.getByRole("dialog", {
      name: "Custom provider",
      exact: true,
    });
    await form.getByLabel("Provider ID", { exact: true }).fill("form-provider");
    await form
      .getByLabel("Display name", { exact: true })
      .fill("Form provider");
    await form
      .getByLabel("Base URL", { exact: true })
      .fill(`http://127.0.0.1:${address.port}/v1`);
    await selectValue(
      form.getByRole("combobox", { name: "Available in" }),
      "local",
    );
    await selectValue(
      form.getByRole("combobox", { name: "Authentication" }),
      "environment",
    );
    await expect(form.getByLabel("API key", { exact: true })).toHaveCount(0);
    await form
      .getByLabel("Environment variable", { exact: true })
      .fill("FORM_API_KEY");
    await selectValue(
      form.getByRole("combobox", { name: "Authentication" }),
      "key",
    );
    await form.getByLabel("API key", { exact: true }).fill("form-fixture-key");
    await expect(form.getByLabel("API key", { exact: true })).toHaveAttribute(
      "type",
      "password",
    );
    await selectValue(
      form.getByRole("combobox", { name: "Models", exact: true }),
      "manual",
    );
    for (let i = 1; i <= 2; i++) {
      await form
        .getByRole("button", { name: "Add model", exact: true })
        .click();
      await form
        .getByLabel(`Model ID ${i}`, { exact: true })
        .fill("form-model");
    }
    await form
      .getByRole("button", { name: "Add provider", exact: true })
      .click();
    await expect(form.getByRole("alert")).toContainText("duplicates");
    await selectValue(
      form.getByRole("combobox", { name: "Models", exact: true }),
      "dynamic",
    );
    await expect(form.getByLabel("Model ID 1", { exact: true })).toHaveCount(0);
    await expect(form.getByRole("alert")).toHaveCount(0);
    await page.screenshot({
      path: test.info().outputPath("provider-editor.png"),
    });
    await form
      .getByRole("button", { name: "Add provider", exact: true })
      .click();
    await expect(form).toHaveCount(0);
    await expect
      .poll(async () =>
        (await client.refreshProviders())
          .find((p) => p.id === "form-provider")
          ?.models.map((m) => m.id),
      )
      .toEqual(["form-model"]);
    await settings
      .getByRole("button", { name: "Refresh models", exact: true })
      .click();
    await expect(
      settings.locator(".setting-block").filter({ hasText: "Form provider" }),
    ).toContainText("Connected");
    expect(
      received.some(
        (item) =>
          item.path === "/v1/models" &&
          item.authorization === "Bearer form-fixture-key",
      ),
    ).toBe(true);
    expect(await readFile(configPath, "utf8")).not.toContain(
      "form-fixture-key",
    );
    await settings
      .getByRole("button", { name: "Add custom provider", exact: true })
      .click();
    await form.getByLabel("Provider ID", { exact: true }).fill("form-provider");
    await form
      .getByLabel("Base URL", { exact: true })
      .fill("https://example.test");
    await form
      .getByRole("button", { name: "Add provider", exact: true })
      .click();
    await expect(form.getByRole("alert")).toContainText("already in use");
    await form.getByRole("button", { name: "Cancel", exact: true }).click();
    await settings.getByRole("button", { name: "Close settings" }).click();
    await newSession();
    await selectValue(
      page
        .locator(".composer")
        .getByRole("combobox", { name: "Model", exact: true }),
      "form-provider/form-model",
    );
    await send("Return provider form evidence.");
    await expect(page.locator(".transcript")).toContainText(
      "provider-form-execution-proof",
    );
    await expect.poll(async () => (await client.activeTurns()).length).toBe(0);
    expect(
      received.some(
        (item) =>
          item.model === "form-model" &&
          item.authorization === "Bearer form-fixture-key",
      ),
    ).toBe(true);
    await selectValue(
      page
        .locator(".composer")
        .getByRole("combobox", { name: "Model", exact: true }),
      "echo/echo-1",
    );
  } finally {
    await writeFile(configPath, config);
    await page.evaluate(
      (directory) =>
        window.haarness.native("local:" + directory, {
          action: "remove_credential",
          provider: "form-provider",
        }),
      workspace,
    );
    server.closeAllConnections();
    await new Promise<void>((resolve) => server.close(() => resolve()));
  }
  expect(errors).toEqual([]);
});

test("install source forms switch fields, browse, recover from errors and register a marketplace", async () => {
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  const settings = page.getByRole("dialog", { name: "Settings", exact: true });
  await settings.getByRole("button", { name: "Plugins", exact: true }).click();
  await settings
    .getByRole("button", { name: "Install plugin", exact: true })
    .click();
  let form = page.getByRole("dialog", {
    name: "Install a plugin",
    exact: true,
  });
  await form
    .getByLabel("Folder", { exact: true })
    .fill(join(temporary, "missing-plugin"));
  await selectValue(
    form.getByRole("combobox", { name: "Source", exact: true }),
    "repository",
  );
  await expect(form.getByLabel("Folder", { exact: true })).toHaveCount(0);
  await form
    .getByLabel("Repository URL", { exact: true })
    .fill("https://example.test/fixture.git");
  await selectValue(
    form.getByRole("combobox", { name: "Source", exact: true }),
    "marketplace",
  );
  await expect(form.getByLabel("Repository URL", { exact: true })).toHaveCount(
    0,
  );
  await form.getByLabel("Plugin name", { exact: true }).fill("fixture");
  await form
    .getByLabel("Marketplace name", { exact: true })
    .fill("local-market");
  await selectValue(
    form.getByRole("combobox", { name: "Source", exact: true }),
    "local",
  );
  await form.getByRole("button", { name: "Install", exact: true }).click();
  await expect(form.getByRole("alert")).toBeVisible();
  await expect(form.getByLabel("Folder", { exact: true })).toHaveValue(
    join(temporary, "missing-plugin"),
  );
  // Native picker is the only stub; the actual install still goes through the server.
  const folder = join(temporary, "form-plugin");
  await mkdir(join(folder, ".claude-plugin"), { recursive: true });
  await writeFile(
    join(folder, ".claude-plugin/plugin.json"),
    JSON.stringify({ name: "form-plugin", version: "1.0.0" }),
  );
  await app.evaluate(({ dialog }, folder) => {
    const original = dialog.showOpenDialog;
    dialog.showOpenDialog = async () => {
      dialog.showOpenDialog = original;
      return { canceled: false, filePaths: [folder] };
    };
  }, folder);
  await form
    .getByRole("button", { name: "Browse folder", exact: true })
    .click();
  await expect(form.getByLabel("Folder", { exact: true })).toHaveValue(folder);
  await form.getByRole("button", { name: "Install", exact: true }).click();
  await expect(form).toHaveCount(0);
  await expect(
    settings.getByRole("switch", {
      name: "Enable plugin form-plugin",
      exact: true,
    }),
  ).toBeChecked();

  const marketplace = join(temporary, "form-marketplace");
  await mkdir(join(marketplace, ".claude-plugin"), { recursive: true });
  await writeFile(
    join(marketplace, ".claude-plugin/marketplace.json"),
    JSON.stringify({
      name: "form-market",
      owner: { name: "Fixture" },
      plugins: [],
    }),
  );
  await settings
    .getByRole("button", { name: "Register marketplace", exact: true })
    .click();
  form = page.getByRole("dialog", {
    name: "Register a marketplace",
    exact: true,
  });
  await form.getByLabel("Folder", { exact: true }).fill(marketplace);
  await form.getByRole("button", { name: "Register", exact: true }).click();
  await expect(form).toHaveCount(0);
  const registered = await readFile(
    join(temporary, "state/data/plugins/marketplaces.json"),
    "utf8",
  );
  expect(registered).toContain("form-market");

  await settings
    .getByRole("button", { name: "Extensions", exact: true })
    .click();
  await settings
    .getByRole("button", { name: "Install extension", exact: true })
    .click();
  form = page.getByRole("dialog", {
    name: "Install an extension",
    exact: true,
  });
  await selectValue(
    form.getByRole("combobox", { name: "Source", exact: true }),
    "package",
  );
  await expect(form.getByLabel("Extension path", { exact: true })).toHaveCount(
    0,
  );
  await form.getByLabel("Package name", { exact: true }).fill("not a package");
  await form.getByLabel("Version (optional)", { exact: true }).fill("1.2.3");
  await form
    .getByRole("button", { name: "Trust & install", exact: true })
    .click();
  await expect(form.getByRole("alert")).toContainText("npm package");
  await form
    .getByLabel("Package name", { exact: true })
    .fill("@team/extension");
  await page.screenshot({
    path: test.info().outputPath("extension-source.png"),
  });
  await selectValue(
    form.getByRole("combobox", { name: "Source", exact: true }),
    "local",
  );
  await expect(form.getByLabel("Package name", { exact: true })).toHaveCount(0);
  await form.getByRole("button", { name: "Cancel", exact: true }).click();
  await settings.getByRole("button", { name: "Close settings" }).click();
  expect(errors).toEqual([]);
});

test("selects dismiss on their labels and outside controls without reopening or losing form drafts", async () => {
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  const settings = page.getByRole("dialog", { name: "Settings", exact: true });
  await settings
    .getByRole("button", { name: "Providers", exact: true })
    .click();
  await settings
    .getByRole("button", { name: "Add custom provider", exact: true })
    .click();
  const form = page.getByRole("dialog", {
    name: "Custom provider",
    exact: true,
  });
  const scope = form.getByRole("combobox", {
    name: "Available in",
    exact: true,
  });
  const popup = form.locator(".select-popover");
  const input = form.getByLabel("Provider ID", { exact: true });
  await input.fill("keep-select-draft");
  const label = (name: string) =>
    form
      .locator("label.field > span")
      .filter({ hasText: new RegExp("^" + name + "$") });
  for (const name of [
    "Available in",
    "API format",
    "Authentication",
    "Models",
  ]) {
    const control = form.getByRole("combobox", { name, exact: true });
    for (let repeat = 0; repeat < 3; repeat++) {
      await label(name).click();
      await expect(control).toHaveAttribute("aria-expanded", "true");
      await label(name).click();
      await expect(control).toHaveAttribute("aria-expanded", "false");
      await expect(popup).toHaveCount(0);
    }
  }
  // Moving straight between controls must leave exactly one menu open.
  await scope.click();
  const format = form.getByRole("combobox", {
    name: "API format",
    exact: true,
  });
  await format.click();
  await expect(scope).toHaveAttribute("aria-expanded", "false");
  await expect(format).toHaveAttribute("aria-expanded", "true");
  await expect(popup).toHaveCount(1);
  await format.click();
  await expect(popup).toHaveCount(0);

  // Editors and other widgets can stop bubbling. Suppress both bubbling and
  // focus fallback on a real outside click to exercise the capture handler.
  await scope.click();
  const heading = form.getByRole("heading", {
    name: "Custom provider",
    exact: true,
  });
  await heading.evaluate((element) =>
    element.addEventListener(
      "pointerdown",
      (event) => {
        event.stopPropagation();
        event.preventDefault();
      },
      { once: true },
    ),
  );
  await heading.click();
  await expect(popup).toHaveCount(0);
  await expect(form).toBeVisible();
  await scope.click();
  await input.evaluate((element) =>
    element.addEventListener("focusin", (event) => event.stopPropagation(), {
      once: true,
    }),
  );
  await input.focus();
  await expect(popup).toHaveCount(0);
  await expect(input).toBeFocused();
  // Genuine option clicks must remain inside the menu until selection commits.
  await selectValue(scope, "local");
  await expect(scope).toHaveAttribute("data-value", "local");
  await scope.press("ArrowDown");
  await form
    .getByRole("listbox", { name: "Available in", exact: true })
    .press("Escape");
  await expect(scope).toBeFocused();
  await scope.click();
  await input.click();
  await expect(popup).toHaveCount(0);
  await expect(input).toHaveValue("keep-select-draft");
  await expect(scope).toHaveAttribute("data-value", "local");
  await form.getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(form).toHaveCount(0);
  await expect(settings).toBeVisible();

  await settings
    .getByRole("button", { name: "MCP servers", exact: true })
    .click();
  await settings
    .getByRole("button", { name: "Add MCP server", exact: true })
    .click();
  const mcp = page.getByRole("dialog", { name: "Add MCP server", exact: true });
  const mcpScope = mcp.getByRole("combobox", {
    name: "Available in",
    exact: true,
  });
  await mcp.getByLabel("Name", { exact: true }).fill("unsaved-select-fixture");
  await mcpScope.click();
  await mcp
    .locator("label > span")
    .filter({ hasText: /^Available in$/ })
    .click();
  await expect(mcpScope).toHaveAttribute("aria-expanded", "false");
  await mcpScope.click();
  await mcp.getByLabel("Executable", { exact: true }).click();
  await expect(mcp.locator(".select-popover")).toHaveCount(0);
  await mcp.getByRole("button", { name: "Cancel", exact: true }).click();
  expect(
    (await client.mcpServers()).some(
      (server) => server.name === "unsaved-select-fixture",
    ),
  ).toBe(false);

  await settings.getByRole("button", { name: "General", exact: true }).click();
  const appearance = settings.getByRole("combobox", {
    name: "Appearance",
    exact: true,
  });
  await appearance.click();
  await settings.getByRole("heading", { name: "General", exact: true }).click();
  await expect(settings.locator(".select-popover")).toHaveCount(0);
  await settings
    .getByRole("button", { name: "Close settings", exact: true })
    .click();
  await newSession();
  const composerModel = page
    .locator(".composer")
    .getByRole("combobox", { name: "Model", exact: true });
  await composerModel.click();
  await page.getByRole("textbox", { name: "Message", exact: true }).click();
  await expect(page.locator(".select-popover")).toHaveCount(0);
  expect(errors).toEqual([]);
});

test("chat-only composer preserves recall and drafts and rejects removed shell entry points", async () => {
  await page.locator(".project-list .project-heading").filter({ hasText: /^workspace$/ }).click();
  await newSession();
  await selectValue(page.getByRole("combobox", { name: "Agent", exact: true }), "build");
  await selectValue(page.getByRole("combobox", { name: "Model", exact: true }), "echo/echo-1");
  const input = page.getByRole("textbox", { name: "Message", exact: true });
  const boundary = async (field: Locator, at: "start" | "end") => field.evaluate((element, at) => {
    const input = element as HTMLTextAreaElement;
    input.setSelectionRange(at === "start" ? 0 : input.value.length, at === "start" ? 0 : input.value.length);
  }, at);
  for (const text of ["history-first", "history-second"]) {
    await send(text);
    await expect(page.locator(".transcript")).toContainText("Echo: " + text);
    await expect(page.locator(".composer")).not.toHaveClass(/running/);
  }
  await input.fill("unfinished\nsecond line");
  await input.evaluate(element => (element as HTMLTextAreaElement).setSelectionRange(14, 14));
  await input.press("ArrowUp");
  await expect(input).toHaveValue("unfinished\nsecond line");
  await boundary(input, "start");
  await input.press("ArrowUp");
  await expect(input).toHaveValue("unfinished\nsecond line");
  await input.fill("history-");
  await boundary(input, "start");
  await input.press("ArrowUp");
  await expect(input).toHaveValue("history-second");
  await input.press("ArrowUp");
  await expect(input).toHaveValue("history-first");
  await input.press("ArrowDown");
  await expect(input).toHaveValue("history-second");
  await input.press("ArrowDown");
  await expect(input).toHaveValue("history-");
  await input.fill("unfinished\nsecond line");
  await expect(page.getByRole("combobox", { name: "Input mode", exact: true })).toHaveCount(0);
  for (const shortcut of ["ControlOrMeta+Shift+x", "ControlOrMeta+Shift+e", "Escape"]) {
    await input.press(shortcut);
    await expect(input).toHaveValue("unfinished\nsecond line");
    await expect(page.getByRole("textbox", { name: "Shell command", exact: true })).toHaveCount(0);
  }
  await expect(page.getByRole("combobox", { name: "Model", exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "Attach", exact: true })).toBeEnabled();
  await page.getByRole("button", { name: "Search", exact: true }).click();
  await page.getByRole("combobox", { name: "Search Haarness", exact: true }).fill("Switch to shell mode");
  await expect(page.getByRole("option", { name: "Switch to shell mode", exact: true })).toHaveCount(0);
  await page.keyboard.press("Escape");
  const rejected = await page.evaluate(async () => {
    const id = await window.haarness.storage("active-project") as string;
    await window.haarness.storage("prompt-history:v1:shell:" + id, [{ text: "legacy-shell-command", attachments: [], context: [] }]);
    await window.haarness.storage("shell-draft:" + id + ":", "legacy-shell-draft");
    try {
      await window.haarness.api(id, "runShell", ["unused", { command: "printf removed > removed-shell.txt" }]);
      return "unexpected success";
    } catch (error) { return String(error); }
  });
  expect(rejected).toContain("Unsupported API operation");
  await page.reload();
  await expect(input).toHaveValue("unfinished\nsecond line");
  await expect(page.getByRole("combobox", { name: "Input mode", exact: true })).toHaveCount(0);
  await boundary(input, "start");
  await input.press("ArrowUp");
  await expect(input).toHaveValue("unfinished\nsecond line");
  await input.press("ArrowDown");
  await expect(input).toHaveValue("unfinished\nsecond line");
  await send("printf chat-only-proof > removed-shell.txt");
  await expect(page.locator(".transcript")).toContainText("Echo: printf chat-only-proof > removed-shell.txt");
  expect(await readFile(join(workspace, "removed-shell.txt"), "utf8").catch(() => null)).toBeNull();
  await expect(page.getByRole("button", { name: "Allow once", exact: true })).toHaveCount(0);
  await page.screenshot({ path: test.info().outputPath("chat-only-composer.png") });
  expect(errors).toEqual([]);
});

test("composer border follows the selected agent in drafts and saved conversations", async () => {
  await mkdir(join(workspace, ".haarness/agents"), { recursive: true });
  for (const [name, mode] of [["picker-delegate", "subagent"], ["picker-shared", "all"]])
    await writeFile(join(workspace, ".haarness/agents", name + ".md"), `---\ndescription: Picker visibility fixture\nmode: ${mode}\n---\nInspect the workspace.\n`);
  await expect.poll(async () => (await client.agents()).some(agent => agent.id === "picker-delegate" && agent.mode === "subagent")).toBe(true);
  await page.locator(".project-list .project-heading").filter({ hasText: /^workspace$/ }).click();
  await page.reload();
  await newSession();
  const agent = page.getByRole("combobox", { name: "Agent", exact: true });
  await agent.click();
  for (const name of ["title", "compaction", "picker-delegate"])
    await expect(page.getByRole("option", { name, exact: true })).toHaveCount(0);
  await expect(page.getByRole("option", { name: "picker-shared", exact: true })).toBeVisible();
  await page.keyboard.press("Escape");
  const composer = page.locator(".composer");
  const input = page.getByRole("textbox", { name: "Message", exact: true });
  const border = () => composer.evaluate((element) => getComputedStyle(element).borderTopColor);
  // Remove interpolation so we can compare the final colors at each step.
  await page.emulateMedia({ reducedMotion: "reduce" });
  await selectValue(agent, "build");
  await input.focus();
  await expect(composer).toHaveAttribute("data-agent", "build");
  const build = await border();
  await selectValue(agent, "plan");
  await input.focus();
  await expect(composer).toHaveAttribute("data-agent", "plan");
  await expect.poll(border).not.toBe(build);
  const plan = await border();
  await page.screenshot({ path: test.info().outputPath("agent-plan-dark.png") });
  await selectValue(agent, "review");
  await input.focus();
  await expect.poll(border).not.toBe(plan);
  await selectValue(agent, "plan");
  await input.focus();
  await expect.poll(border).toBe(plan);
  await selectValue(page.getByRole("combobox", { name: "Model", exact: true }), "echo/echo-1");
  await send("agent-border-proof");
  await expect(page.locator(".transcript")).toContainText("agent-border-proof");
  await expect(composer).not.toHaveClass(/running/);
  await page.reload();
  await expect(agent).toHaveAttribute("data-value", "plan");
  await expect(composer).toHaveAttribute("data-agent", "plan");
  await input.focus();
  await expect.poll(border).toBe(plan);
  await selectValue(agent, "explore");
  await expect(composer).toHaveAttribute("data-agent", "explore");
  await page.reload();
  await expect(composer).toHaveAttribute("data-agent", "explore");
  await page.evaluate(async () => { await window.haarness.preferences({ theme: "light" }); });
  await input.focus();
  const lightExplore = await border();
  await selectValue(agent, "plan");
  await input.focus();
  await expect.poll(border).not.toBe(lightExplore);
  await page.screenshot({ path: test.info().outputPath("agent-plan-light.png") });
  await page.evaluate(async () => { await window.haarness.preferences({ theme: "dark" }); });
  await page.emulateMedia({ reducedMotion: "no-preference" });
  for (const name of ["picker-delegate", "picker-shared"])
    await rm(join(workspace, ".haarness/agents", name + ".md"));
  expect(errors).toEqual([]);
});

test("provider artwork and combined reasoning follow each model's actual capability scale", async () => {
  const provider = createServer((_request, response) => {
    response.writeHead(200, { "Content-Type": "application/json" });
    response.end(JSON.stringify({ data: [
      { id: "compact-reasoner", supported_parameters: ["reasoning_effort"], reasoning_efforts: ["low", "high"] },
      { id: "extended-reasoner", supported_parameters: ["reasoning_effort"], reasoning_efforts: ["none", "minimal", "medium", "high", "xhigh"] },
      { id: "plain-model", reasoning_efforts: [] },
    ] }));
  });
  await new Promise<void>((resolve) => provider.listen(0, "127.0.0.1", resolve));
  const address = provider.address();
  if (!address || typeof address === "string") throw Error("Provider fixture did not bind");
  const directory = join(temporary, "reasoning-picker");
  await mkdir(join(directory, ".haarness"), { recursive: true });
  await writeFile(join(directory, ".haarness/config.toml"), `[defaults]\nprovider="picker-fixture"\nmodel="compact-reasoner"\n[providers.picker-fixture]\nname="Picker fixture"\nprotocol="openai-chat-completions"\nbase_url="http://127.0.0.1:${address.port}/v1"\nrequires_auth=false\ntools=true\nreasoning=true\n`);
  extraWorkspaces.push(directory);
  try {
    await app.evaluate(({ dialog }, directory) => {
      const original = dialog.showOpenDialog;
      dialog.showOpenDialog = async () => {
        dialog.showOpenDialog = original;
        return { canceled: false, filePaths: [directory] };
      };
    }, directory);
    await page.getByRole("button", { name: "Open another project", exact: true }).click();
    await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
    const model = page.getByRole("combobox", { name: "Model", exact: true });
    const slider = page.getByRole("slider", { name: "Reasoning effort", exact: true });
    await expect(model).toHaveAttribute("data-value", "picker-fixture/compact-reasoner");
    await expect(model.locator('[data-provider-icon="generic"]')).toHaveCount(1);
    await expect(page.getByRole("combobox", { name: "Reasoning effort", exact: true })).toHaveCount(0);
    await model.click();
    await expect(slider).toHaveAttribute("max", "2");
    await slider.focus();
    await slider.press("End");
    await expect(slider).toHaveAttribute("aria-valuetext", "High");
    await slider.press("ArrowLeft");
    await expect(slider).toHaveAttribute("aria-valuetext", "Low");
    // The slider handles its own arrows; it must not move the model-list cursor.
    await expect(model).toHaveAttribute("data-value", "picker-fixture/compact-reasoner");
    await page.getByRole("option", { name: "extended-reasoner", exact: true }).click();
    await expect(slider).toHaveAttribute("max", "5");
    await expect(slider).toHaveAttribute("aria-valuetext", "Model default");
    await slider.focus();
    await slider.press("End");
    await expect(slider).toHaveAttribute("aria-valuetext", "Extra high");
    await page.getByRole("button", { name: "Reset reasoning to model default", exact: true }).click();
    await expect(slider).toHaveAttribute("aria-valuetext", "Model default");
    await slider.focus();
    await slider.press("ArrowRight");
    await expect(slider).toHaveAttribute("aria-valuetext", "None");
    await page.getByRole("option", { name: "plain-model", exact: true }).click();
    await expect(slider).toHaveCount(0);
    await expect(page.locator(".reasoning-unavailable")).toContainText("no adjustable reasoning levels");
    await page.keyboard.press("Escape");
    await expect(model).toBeFocused();
    await page.getByRole("button", { name: "Settings", exact: true }).click();
    const settings = page.getByRole("dialog", { name: "Settings", exact: true });
    await settings.getByRole("button", { name: "Providers", exact: true }).click();
    // The connected custom provider is listed with the neutral icon; the
    // catalog, with each brand's artwork, is in Connect a provider.
    await expect(settings.locator('[data-setting="provider:picker-fixture"] [data-provider-icon="generic"]')).toHaveCount(1);
    await expect(settings.locator('[data-setting="provider:anthropic"]')).toHaveCount(0);
    await settings.getByRole("button", { name: "Connect a provider", exact: true }).click();
    const connect = page.getByRole("dialog", { name: "Connect a provider", exact: true });
    for (const id of ["anthropic", "openai", "gemini"])
      await expect(connect.locator(`.connect-row[data-provider="${id}"] [data-provider-icon="${id}"]`)).toHaveCount(1);
    const icon = connect.locator('.connect-row[data-provider="anthropic"] .provider-icon-mask');
    await expect.poll(() => icon.evaluate((element) => getComputedStyle(element).maskImage)).not.toBe("none");
    await page.screenshot({ path: test.info().outputPath("provider-icons-dark.png") });
    await connect.getByRole("button", { name: "Close dialog", exact: true }).click();
    await settings.getByRole("button", { name: "Models", exact: true }).click();
    // Models of providers that are not connected wait behind Show all.
    await expect(settings.locator('tbody [data-provider-icon="anthropic"]')).toHaveCount(0);
    await settings.getByRole("switch", { name: "Show all", exact: true }).check();
    await expect(settings.locator('tbody [data-provider-icon="anthropic"]').first()).toBeVisible();
    await settings.getByRole("button", { name: "Close settings", exact: true }).click();
    await page.getByRole("button", { name: "Search", exact: true }).click();
    await page.getByRole("combobox", { name: "Search Haarness", exact: true }).fill("Anthropic");
    await expect(page.getByRole("option", { name: "Anthropic", exact: true }).locator('[data-provider-icon="anthropic"]')).toHaveCount(1);
    await page.keyboard.press("Escape");
    await selectValue(model, "picker-fixture/compact-reasoner");
    await model.click();
    await slider.focus();
    await slider.press("End");
    await page.screenshot({ path: test.info().outputPath("combined-model-reasoning-dark.png") });
    await page.keyboard.press("Tab");
    await expect(page.getByRole("button", { name: "Done", exact: true })).toBeFocused();
    await page.keyboard.press("Enter");
    await expect(model).toBeFocused();
    // Light theme must retain the monochrome mask's contrast without alternate assets.
    await page.evaluate(async () => { await window.haarness.preferences({ theme: "light" }); });
    await model.click();
    await page.screenshot({ path: test.info().outputPath("combined-model-reasoning-light.png") });
    await page.keyboard.press("Escape");
    await page.evaluate(async () => { await window.haarness.preferences({ theme: "dark" }); });
    expect(errors).toEqual([]);
  } finally { await new Promise<void>((resolve) => provider.close(() => resolve())); }
});

test("prompt context survives navigation and sends file ranges and agent delegation through a real turn", async () => {
  // This flow reads Files and the composer together. Earlier shared-window
  // scenarios can leave a narrow viewport where Files intentionally hides chat.
  await page.setViewportSize({ width: 1420, height: 940 });
  const received: any[] = [];
  let release: (() => void) | undefined;
  let held = false;
  const provider = createServer(async (request, response) => {
    if (request.method === "GET") {
      response.writeHead(200, { "Content-Type": "application/json" });
      response.end(JSON.stringify({ data: [{ id: "context-model" }] }));
      return;
    }
    const chunks: Buffer[] = [];
    for await (const chunk of request) chunks.push(Buffer.from(chunk));
    const body = JSON.parse(Buffer.concat(chunks).toString());
    received.push(body);
    const messages = body.messages ?? [];
    const isChild = messages.some((message: any) => message.role === "system" && JSON.stringify(message.content).includes("CONTEXT_HELPER_MARK"));
    const isMain = messages.some((message: any) => message.role === "user" && JSON.stringify(message.content).includes("CTX_REQUEST"));
    const hasResult = messages.some((message: any) => message.role === "tool");
    if (!held && messages.some((message: any) => message.role === "user" && JSON.stringify(message.content).includes("HOLD_CONTEXT"))) {
      held = true;
      await new Promise<void>(resolve => { release = resolve; });
    }
    const tool = isMain && !isChild && !hasResult;
    response.writeHead(200, { "Content-Type": "text/event-stream" });
    response.end("data: " + JSON.stringify({ choices: [{ index: 0, delta: tool ? {
      tool_calls: [{ index: 0, id: "context-delegation", type: "function", function: { name: "task", arguments: JSON.stringify({ agent: "context-helper", description: "Inspect attached context", prompt: "Report delegated-context-proof for the requested review." }) } }],
    } : { content: isChild ? "delegated-context-proof" : "context-turn-complete" }, finish_reason: null }] })
      + "\n\ndata: " + JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: tool ? "tool_calls" : "stop" }] }) + "\n\ndata: [DONE]\n\n");
  });
  await new Promise<void>(resolve => provider.listen(0, "127.0.0.1", resolve));
  const address = provider.address();
  if (!address || typeof address === "string") throw Error("Provider fixture did not bind");
  const directory = join(temporary, "prompt-context-project");
  await mkdir(join(directory, ".haarness/agents"), { recursive: true });
  await mkdir(join(directory, "src"));
  await mkdir(join(directory, ".haarness/commands"));
  await writeFile(join(directory, ".haarness/commands/context-note.md"), "---\ndescription: Context command fixture\n---\nCOMMAND_CONTEXT $ARGUMENTS\n");
  await writeFile(join(directory, ".haarness/config.toml"), `[defaults]\nprovider="context-fixture"\nmodel="context-model"\n[permissions]\ntask="allow"\n[providers.context-fixture]\nprotocol="openai-chat-completions"\nbase_url="http://127.0.0.1:${address.port}/v1"\nrequires_auth=false\ntools=true\n`);
  await writeFile(join(directory, ".haarness/agents/context-helper.md"), "---\ndescription: Context delegation fixture\nmode: subagent\n---\nCONTEXT_HELPER_MARK\nReport the context review result.\n");
  await writeFile(join(directory, "README.md"), "WHOLE_FILE_CONTEXT\nReference material.\n");
  await writeFile(join(directory, "remove-me.txt"), "REMOVED_CONTEXT_MUST_NOT_BE_SENT");
  await writeFile(join(directory, "too-large.txt"), "x".repeat(140000));
  await writeFile(join(directory, "src/range.ts"), "EXCLUDED_FIRST_LINE\nSELECTED_SECOND_LINE\nSELECTED_THIRD_LINE\nEXCLUDED_LAST_LINE\n");
  extraWorkspaces.push(directory);
  try {
    await app.evaluate(({ dialog }, directory) => {
      const original = dialog.showOpenDialog;
      dialog.showOpenDialog = async () => { dialog.showOpenDialog = original; return { canceled: false, filePaths: [directory] }; };
    }, directory);
    await page.getByRole("button", { name: "Open another project", exact: true }).click();
    await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
    const input = page.getByRole("textbox", { name: "Message", exact: true });
    const context = page.getByLabel("Attached context", { exact: true });
    const picker = page.getByRole("listbox", { name: "Files and agents", exact: true });
    async function mention(query: string) {
      await input.fill("CTX_REQUEST @" + query);
      await expect(picker).toBeVisible();
    }
    await mention("README");
    await expect(picker.getByRole("option").filter({ hasText: "README.md" })).toBeVisible();
    await input.press("Enter");
    await expect(context).toContainText("README.md");
    await expect(input).toHaveValue("CTX_REQUEST ");
    await mention("remove-me");
    await picker.getByRole("option").filter({ hasText: "remove-me.txt" }).click();
    await page.getByRole("button", { name: "Remove context remove-me.txt", exact: true }).click();
    await expect(context).not.toContainText("remove-me.txt");
    await mention("too-large");
    await picker.getByRole("option").filter({ hasText: "too-large.txt" }).click();
    await expect(page.locator(".mention-popover [role=alert]")).toContainText("128 KiB");
    await expect(input).toHaveValue("CTX_REQUEST @too-large");
    await input.press("Escape");
    await mention("context-helper");
    await expect(picker.getByRole("option").filter({ hasText: "context-helper" })).toBeVisible();
    await input.press("Tab");
    await expect(context).toContainText("@context-helper");
    await expect(page.getByRole("combobox", { name: "Agent", exact: true })).toHaveAttribute("data-value", "build");
    await mention("compaction");
    await expect(picker.getByRole("option").filter({ hasText: "compaction" })).toHaveCount(0);
    await input.press("Escape");
    await input.fill("CTX_REQUEST");
    await openProjectPanel("Files");
    await page.getByRole("textbox", { name: "Find files", exact: true }).fill("range.ts");
    await page.locator(".file-entry").filter({ hasText: "src/range.ts" }).click();
    const editor = page.locator(".file-preview .cm-content");
    await expect(editor).toContainText("SELECTED_SECOND_LINE");
    await editor.focus();
    await editor.press("Control+Home");
    await editor.press("ArrowDown");
    await editor.press("Home");
    await editor.pressSequentially("UNSAVED_");
    await expect(editor).toContainText("UNSAVED_SELECTED_SECOND_LINE");
    await editor.press("Home");
    await editor.press("Shift+ArrowDown");
    await editor.press("Shift+ArrowDown");
    await page.getByRole("button", { name: "Add selection to context", exact: true }).click();
    await expect(context).toContainText("src/range.ts:2-3 · unsaved");
    // Discard the editor edit; the captured selection must remain unchanged.
    await openProjectPanel("Files");
    await editor.focus();
    await editor.press(process.platform === "darwin" ? "Meta+z" : "Control+z");
    await expect(editor).not.toContainText("UNSAVED_SELECTED_SECOND_LINE");
    await page.reload();
    await expect(context).toContainText("README.md");
    await expect(context).toContainText("@context-helper");
    await expect(context).toContainText("src/range.ts:2-3");
    await expect(input).toHaveValue("CTX_REQUEST");
    await page.getByRole("region", { name: "Project workspace", exact: true }).getByRole("button", { name: "workspace", exact: true }).click();
    await newSession();
    await expect(context).toHaveCount(0);
    await page.getByRole("region", { name: "Project prompt-context-project", exact: true }).getByRole("button", { name: "prompt-context-project", exact: true }).click();
    await expect(context).toContainText("src/range.ts:2-3");
    await expect(input).toHaveValue("CTX_REQUEST");
    await expect(page.getByRole("combobox", { name: "Model", exact: true })).toHaveAttribute("data-value", "context-fixture/context-model");
    await page.locator(".composer").screenshot({ path: test.info().outputPath("prompt-context-draft.png") });
    await page.getByRole("button", { name: "Send message", exact: true }).click();
    await expect(page.locator(".transcript")).toContainText("context-turn-complete");
    await expect(context).toHaveCount(0);
    await expect(input).toHaveValue("");
    const mainRequests = received.filter(body => body.messages?.some((message: any) => message.role === "user" && JSON.stringify(message.content).includes("CTX_REQUEST")));
    expect(mainRequests.length).toBeGreaterThan(0);
    const user = JSON.stringify(mainRequests[0].messages.filter((message: any) => message.role === "user"));
    expect(user).toContain("WHOLE_FILE_CONTEXT");
    expect(user).toContain("UNSAVED_SELECTED_SECOND_LINE");
    expect(user).toContain("unsaved editor snapshot");
    expect(user).toContain("SELECTED_THIRD_LINE");
    expect(user).not.toContain("EXCLUDED_FIRST_LINE");
    expect(user).not.toContain("EXCLUDED_LAST_LINE");
    expect(user).not.toContain("REMOVED_CONTEXT_MUST_NOT_BE_SENT");
    expect(user).toContain('task tool with agent');
    const connection = "local:" + directory;
    const threads = await page.evaluate(connection => window.haarness.api<any[]>(connection, "listThreads", []), connection);
    expect(threads.some(thread => thread.agent === "context-helper" && thread.isSubagent)).toBe(true);
    await page.reload();
    await expect(context).toHaveCount(0);
    await expect(page.locator(".transcript")).toContainText("context-turn-complete");
    await expect(page.locator('.message[data-type="user_message"] summary').filter({ hasText: "src/range.ts:2-3" })).toBeVisible();
    // Custom slash commands must receive attached context in their arguments.
    await newSession();
    await input.fill("/context-note @README");
    await picker.getByRole("option").filter({ hasText: "README.md" }).click();
    await page.getByRole("button", { name: "Send message", exact: true }).click();
    await expect.poll(() => received.some(body => body.messages?.some((message: any) => message.role === "user"
      && JSON.stringify(message.content).includes("COMMAND_CONTEXT") && JSON.stringify(message.content).includes("WHOLE_FILE_CONTEXT")))).toBe(true);
    await expect(page.locator(".composer")).not.toHaveClass(/running/);
    // Context also accompanies guidance submitted while a provider request is running.
    await send("HOLD_CONTEXT");
    await expect.poll(() => !!release).toBe(true);
    await input.fill("QUEUED_CONTEXT @README");
    await picker.getByRole("option").filter({ hasText: "README.md" }).click();
    await page.getByRole("button", { name: "Send message", exact: true }).click();
    await expect(context).toHaveCount(0);
    release!();
    await expect.poll(() => received.some(body => body.messages?.some((message: any) => message.role === "user"
      && JSON.stringify(message.content).includes("QUEUED_CONTEXT") && JSON.stringify(message.content).includes("WHOLE_FILE_CONTEXT")))).toBe(true);
    await expect(page.locator(".composer")).not.toHaveClass(/running/);
    // An agent removed from delegation choices must not silently lose the draft.
    await newSession();
    await mention("context-helper");
    await picker.getByRole("option").filter({ hasText: "context-helper" }).click();
    await writeFile(join(directory, ".haarness/agents/context-helper.md"), "---\ndescription: Context delegation fixture\nmode: primary\n---\nCONTEXT_HELPER_MARK\n");
    await expect.poll(async () => page.evaluate(connection => window.haarness.api<any[]>(connection, "agents", []).then(agents => agents.find(agent => agent.id === "context-helper")?.mode), connection)).toBe("primary");
    await page.reload();
    await expect(page.getByRole("combobox", { name: "Model", exact: true })).toHaveAttribute("data-value", "context-fixture/context-model");
    await expect(context).toContainText("@context-helper");
    await page.getByRole("button", { name: "Send message", exact: true }).click();
    await expect(page.getByRole("alert")).toContainText("no longer available for delegation");
    await expect(context).toContainText("@context-helper");
    await expect(input).toHaveValue("CTX_REQUEST ");
    await page.getByRole("button", { name: "Remove context @context-helper", exact: true }).click();
    await expect(context).toHaveCount(0);
    expect(errors).toEqual([]);
  } finally {
    release?.();
    provider.closeAllConnections();
    await new Promise<void>(resolve => provider.close(() => resolve()));
  }
});

test("remote prompt context reads server files and sends a context-only message", async () => {
  await writeFile(join(workspace, "remote-context.txt"), "REMOTE_FILE_CONTEXT_PROOF\n");
  const endpoint = JSON.parse(execFileSync(binary, ["--workspace", workspace, "--trust-project", "desktop"], {
    env, input: JSON.stringify({ action: "connect" }), encoding: "utf8", timeout: 30000,
  }));
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  const settings = page.getByRole("dialog", { name: "Settings", exact: true });
  await settings.getByRole("button", { name: "Servers", exact: true }).click();
  await settings.getByRole("button", { name: "Add remote server", exact: true }).click();
  const form = page.getByRole("dialog", { name: "Connect to server", exact: true });
  await form.getByLabel("Server URL", { exact: true }).fill(endpoint.url);
  await form.getByLabel("Name", { exact: true }).fill("Context remote");
  await form.getByRole("button", { name: "Connect", exact: true }).click();
  await expect(form).toHaveCount(0);
  if (await settings.isVisible()) await settings.getByRole("button", { name: "Close settings", exact: true }).click();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
  await expect(page.getByRole("button", { name: "Switch server", exact: true })).toContainText("Context remote");
  await newSession();
  const input = page.getByRole("textbox", { name: "Message", exact: true });
  await input.fill("@remote-context");
  await page.getByRole("listbox", { name: "Files and agents", exact: true }).getByRole("option").filter({ hasText: "remote-context.txt" }).click();
  await expect(input).toHaveValue("");
  const context = page.getByLabel("Attached context", { exact: true });
  await context.locator("summary").click();
  await expect(context.locator("pre")).toHaveText("REMOTE_FILE_CONTEXT_PROOF\n");
  await page.getByRole("button", { name: "Send message", exact: true }).click();
  await expect(page.locator(".transcript .message.assistant")).toContainText("REMOTE_FILE_CONTEXT_PROOF");
  await expect(context).toHaveCount(0);
  expect(errors).toEqual([]);
});

test("conversation controls undo and redo real files, preserve drafts, and compact through the model", async () => {
  await page.locator(".project-list .project-heading").filter({ hasText: /^workspace$/ }).click();
  const expand = page.getByRole("button", { name: "Expand project workspace", exact: true });
  if (await expand.count()) await expand.click();
  const thread = await client.createThread({ title: "History controls fixture", provider: "echo", model: "echo-1", agent: "build" });
  await page.getByRole("button", { name: thread.title, exact: true }).click();
  const input = page.getByRole("textbox", { name: "Message", exact: true });
  const menu = async (name: string) => {
    await page.getByRole("button", { name: "Chat actions", exact: true }).click();
    await page.getByRole("menuitem", { name, exact: true }).click();
  };
  const palette = async (name: string) => {
    await page.getByRole("button", { name: "Search", exact: true }).click();
    await page.getByRole("combobox", { name: "Search Haarness", exact: true }).fill(name);
    await page.getByRole("option", { name, exact: true }).click();
  };
  const approval = page.getByRole("button", { name: /^Approvals: (Review|Auto)$/ });
  if (await approval.getAttribute("aria-label") !== "Approvals: Auto") await approval.click();
  const path = join(workspace, "history-controls.txt");
  const first = '!tool write {"path":"history-controls.txt","content":"first value"}';
  const second = '!tool write {"path":"history-controls.txt","content":"second value"}';
  const finish = async () => {
    await expect.poll(async () => (await client.activeTurns()).some(turn => turn.threadId === thread.id)).toBe(false);
    await expect(page.locator(".composer")).not.toHaveClass(/running/);
  };
  await send(first);
  await expect.poll(() => readFile(path, "utf8").catch(() => "")).toBe("first value");
  await finish();
  await send(second);
  await expect.poll(() => readFile(path, "utf8")).toBe("second value");
  await finish();
  await send("Third prompt with history-needle");
  await expect(page.locator(".transcript")).toContainText("Echo: Third prompt with history-needle");
  await finish();
  const third = (await client.listItems(thread.id)).find(item => item.data.type === "user_message" && item.data.text === "Third prompt with history-needle")!;
  await menu("Undo last turn");
  await expect(input).toHaveValue("Third prompt with history-needle");
  await expect(page.locator('[data-item-id="' + third.id + '"]')).toHaveCount(0);
  expect(await readFile(path, "utf8")).toBe("second value");
  const boundary = (await client.getThread(thread.id)).revert!.sequence;
  // A conflicting external edit must surface an error and preserve everything.
  await writeFile(path, "external edit");
  await palette("Undo last turn");
  await expect(page.locator(".error-banner")).toBeVisible();
  expect(await readFile(path, "utf8")).toBe("external edit");
  expect((await client.getThread(thread.id)).revert!.sequence).toBe(boundary);
  await expect(input).toHaveValue("Third prompt with history-needle");
  await writeFile(path, "second value");
  await menu("Undo last turn");
  await expect.poll(() => readFile(path, "utf8")).toBe("first value");
  await expect(input).toHaveValue(second);
  await page.reload();
  await expect(input).toHaveValue(second);
  await expect(page.locator(".conversation-history-notice")).toContainText("Turn undone");
  await input.press(process.platform === "darwin" ? "Meta+f" : "Control+f");
  const find = page.getByRole("searchbox", { name: "Find in chat", exact: true });
  await find.fill("history-needle");
  await expect(page.locator(".conversation-find [role=status]")).toHaveText("No matches");
  await find.press("Escape");
  await menu("Redo turn");
  await expect.poll(() => readFile(path, "utf8")).toBe("second value");
  await expect(input).toHaveValue("");
  await menu("Redo turn");
  await expect.poll(async () => (await client.getThread(thread.id)).revert ?? null).toBeNull();
  await expect(page.locator(".transcript")).toContainText("Echo: Third prompt with history-needle");
  await input.fill("Keep my unsent draft");
  await menu("Undo last turn");
  await expect(input).toHaveValue("Keep my unsent draft\n\nThird prompt with history-needle");
  await menu("Redo turn");
  await expect(input).toHaveValue("Keep my unsent draft");
  await menu("Compact chat");
  await expect.poll(async () => (await client.listItems(thread.id)).filter(item => item.data.type === "compaction").length).toBeGreaterThan(0);
  await expect(page.locator(".conversation-history-notice")).toHaveCount(0);
  await expect(input).toHaveValue("Keep my unsent draft");
  await page.reload();
  await expect(input).toHaveValue("Keep my unsent draft");
  expect((await client.listItems(thread.id)).some(item => item.data.type === "compaction" && !!item.data.summary)).toBe(true);
  expect(errors).toEqual([]);
});

test("conversation find reaches virtual history, cycles matches, and respects keyboard focus", async () => {
  await page.locator(".project-list .project-heading").filter({ hasText: /^workspace$/ }).click();
  const expand = page.getByRole("button", { name: "Expand project workspace", exact: true });
  if (await expand.count()) await expand.click();
  const source = await client.createThread({ title: "Search source", provider: "echo", model: "echo-1" });
  const archive = await client.exportSessionArchive(source.id);
  const id = crypto.randomUUID();
  const now = new Date().toISOString();
  archive.rootThreadId = id;
  archive.threads[0].thread.id = id;
  archive.threads[0].thread.title = "Find long conversation";
  archive.threads[0].items = Array.from({ length: 220 }, (_, index) => ({
    id: crypto.randomUUID(), threadId: id, turnId: null, sequence: index + 1, createdAt: now,
    data: { type: "assistant_message" as const, text: index === 4 || index === 205 ? "Distinct FIND-TARGET [literal].\n\n" + "Context paragraph. ".repeat(20) : "History message " + index + ".\n\n" + "Context paragraph. ".repeat(20) },
  }));
  await client.importSessionArchive({ archive });
  await page.getByRole("button", { name: "Find long conversation", exact: true }).click();
  const old = archive.threads[0].items[4];
  await expect(page.locator('[data-item-id="' + old.id + '"]')).toHaveCount(0);
  await page.getByRole("textbox", { name: "Message", exact: true }).press(process.platform === "darwin" ? "Meta+f" : "Control+f");
  const find = page.getByRole("searchbox", { name: "Find in chat", exact: true });
  await expect(find).toBeFocused();
  await find.fill("find-target [literal]");
  await expect(page.locator(".conversation-find [role=status]")).toHaveText("1 of 2 messages");
  await expect(page.locator('[data-item-id="' + old.id + '"]')).toBeInViewport();
  await expect(page.locator(".conversation-match")).toContainText("FIND-TARGET");
  await find.press("Enter");
  await expect(page.locator(".conversation-find [role=status]")).toHaveText("2 of 2 messages");
  await expect(page.locator('[data-item-id="' + archive.threads[0].items[205].id + '"]')).toBeInViewport();
  await find.press("Enter");
  await expect(page.locator(".conversation-find [role=status]")).toHaveText("1 of 2 messages");
  await find.press("Shift+Enter");
  await expect(page.locator(".conversation-find [role=status]")).toHaveText("2 of 2 messages");
  await find.fill("no such message");
  await expect(page.locator(".conversation-find [role=status]")).toHaveText("No matches");
  await expect(page.getByRole("button", { name: "Next match", exact: true })).toBeDisabled();
  await find.fill("<img onerror=bad>");
  await expect(page.locator(".conversation-find img")).toHaveCount(0);
  await find.fill("find-target");
  await page.screenshot({ path: test.info().outputPath("conversation-find-dark.png") });
  await page.evaluate(async () => { await window.haarness.preferences({ theme: "light" }); });
  await page.screenshot({ path: test.info().outputPath("conversation-find-light.png") });
  await find.press("Escape");
  await expect(find).toHaveCount(0);
  // The keyboard goes back to where Find was opened from, the message box.
  await expect(page.getByRole("textbox", { name: "Message", exact: true })).toBeFocused();
  await page.getByRole("button", { name: "Chat actions", exact: true }).click();
  await page.getByRole("menuitem", { name: "Find in chat", exact: true }).click();
  await expect(find).toBeFocused();
  await page.getByRole("button", { name: "Search source", exact: true }).click();
  await expect(find).toHaveCount(0);
  await page.evaluate(async () => { await window.haarness.preferences({ theme: "dark" }); });
  expect(errors).toEqual([]);
});

test("conversation commands guard active turns, preserve failed compaction, and replace undone branches", async () => {
  await page.locator(".project-list .project-heading").filter({ hasText: /^workspace$/ }).click();
  const expand = page.getByRole("button", { name: "Expand project workspace", exact: true });
  if (await expand.count()) await expand.click();
  const thread = await client.createThread({ title: "History command guards", provider: "echo", model: "echo-1" });
  await page.getByRole("button", { name: thread.title, exact: true }).click();
  const input = page.getByRole("textbox", { name: "Message", exact: true });
  const actions = page.getByRole("button", { name: "Chat actions", exact: true });
  await send("before the replacement branch");
  await expect(page.locator(".transcript")).toContainText("Echo: before the replacement branch");
  await expect(page.locator(".composer")).not.toHaveClass(/running/);
  await send("/undo");
  await expect(input).toHaveValue("before the replacement branch");
  expect((await client.listItems(thread.id)).some(item => item.data.type === "user_message" && item.data.text === "/undo")).toBe(false);
  await send("/redo");
  await expect.poll(async () => (await client.getThread(thread.id)).revert ?? null).toBeNull();
  await expect(input).toHaveValue("");
  await send("/undo");
  await expect(input).toHaveValue("before the replacement branch");
  await send("replacement branch survives");
  await expect(page.locator(".transcript")).toContainText("Echo: replacement branch survives");
  await expect(page.locator(".composer")).not.toHaveClass(/running/);
  expect((await client.listItems(thread.id)).some(item => item.data.type === "user_message" && item.data.text === "before the replacement branch")).toBe(false);
  await actions.click();
  await expect(page.getByRole("menuitem", { name: "Redo turn", exact: true })).toBeDisabled();
  await page.keyboard.press("Escape");
  await client.updateThread(thread.id, { provider: "desktop-missing-key", model: "fixture" });
  await input.fill("Preserved on compaction failure");
  await actions.click();
  await page.getByRole("menuitem", { name: "Compact chat", exact: true }).click();
  await expect(page.locator(".error-banner")).toContainText(/key|credential|auth/i);
  await expect(input).toBeEnabled();
  await expect(input).toHaveValue("Preserved on compaction failure");
  expect((await client.listItems(thread.id)).some(item => item.data.type === "compaction")).toBe(false);
  await client.updateThread(thread.id, { provider: "echo", model: "echo-1" });
  const active = await client.startTurn(thread.id, { text: '!tool question {"prompt":"History guard question","choices":["Continue"]}', requestId: crypto.randomUUID() });
  await expect(page.locator(".question-prompt").filter({ hasText: /^History guard question$/ })).toBeVisible();
  await actions.click();
  for (const name of ["Undo last turn", "Redo turn", "Compact chat"])
    await expect(page.getByRole("menuitem", { name, exact: true })).toBeDisabled();
  await page.getByRole("menuitem", { name: "Find in chat", exact: true }).click();
  await expect(page.getByRole("searchbox", { name: "Find in chat", exact: true })).toBeFocused();
  await page.getByRole("button", { name: "Close find in chat", exact: true }).click();
  await client.interruptTurn(active.id);
  await expect.poll(async () => (await client.activeTurns()).some(turn => turn.id === active.id)).toBe(false);
  await openProjectPanel("Files");
  const folder = page.getByRole("button", { name: "src", exact: true });
  await folder.click();
  await page.getByRole("button", { name: "greeting.ts", exact: true }).click();
  const editor = page.locator(".cm-content");
  await editor.click();
  await editor.press(process.platform === "darwin" ? "Meta+f" : "Control+f");
  await expect(page.locator(".cm-search")).toBeVisible();
  await expect(page.locator(".conversation-find")).toHaveCount(0);
  await page.keyboard.press("Escape");
  await page.getByRole("button", { name: "Toggle terminal", exact: true }).click();
  const terminal = page.locator(".xterm-helper-textarea").first();
  await expect(terminal).toBeAttached();
  await terminal.focus();
  await terminal.press(process.platform === "darwin" ? "Meta+f" : "Control+f");
  await expect(page.locator(".conversation-find")).toHaveCount(0);
  expect(errors).toEqual([]);
});

test("slow manual compaction disables duplicate actions and cannot overwrite a different conversation", async () => {
  let release = () => {};
  const gate = new Promise<void>(resolve => { release = resolve; });
  let requested = false;
  const provider = createServer(async (request, response) => {
    if (request.method !== "POST") { response.writeHead(200, { "Content-Type": "application/json" }); response.end('{"data":[{"id":"summary-fixture"}]}'); return; }
    for await (const chunk of request) { void chunk; }
    requested = true;
    await gate;
    response.writeHead(200, { "Content-Type": "text/event-stream" });
    response.end('data: {"choices":[{"index":0,"delta":{"content":"Delayed summary proof"},"finish_reason":null}]}\n\ndata: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}\n\ndata: [DONE]\n\n');
  });
  await new Promise<void>(resolve => provider.listen(0, "127.0.0.1", resolve));
  const address = provider.address();
  if (!address || typeof address === "string") throw Error("Summary provider did not bind");
  const configPath = join(workspace, ".haarness/config.toml");
  const original = await readFile(configPath, "utf8");
  try {
    await writeFile(configPath, original + '\n[providers.slow-summary]\nprotocol="openai-chat-completions"\nbase_url="http://127.0.0.1:' + address.port + '/v1"\nrequires_auth=false\nmodels=["summary-fixture"]\n');
    await expect.poll(async () => (await client.providers()).some(provider => provider.id === "slow-summary")).toBe(true);
    const thread = await client.createThread({ title: "Delayed compaction", provider: "echo", model: "echo-1" });
    const turn = await client.startTurn(thread.id, { text: "Only the first conversation's context", requestId: crypto.randomUUID() });
    await client.waitTurn(turn.id);
    await client.updateThread(thread.id, { provider: "slow-summary", model: "summary-fixture" });
    const other = await client.createThread({ title: "Unaffected conversation", provider: "echo", model: "echo-1" });
    await page.getByRole("button", { name: thread.title, exact: true }).click();
    await page.getByRole("button", { name: "Chat actions", exact: true }).click();
    await page.getByRole("menuitem", { name: "Compact chat", exact: true }).click();
    await expect.poll(() => requested).toBe(true);
    const input = page.getByRole("textbox", { name: "Message", exact: true });
    await expect(input).toBeDisabled();
    await expect(page.getByRole("button", { name: "Send message", exact: true })).toBeDisabled();
    await page.getByRole("button", { name: "Chat actions", exact: true }).click();
    await expect(page.getByRole("menuitem", { name: "Compact chat", exact: true })).toBeDisabled();
    await page.keyboard.press("Escape");
    await page.getByRole("button", { name: other.title, exact: true }).click();
    await input.fill("Other conversation draft");
    release();
    await expect.poll(async () => (await client.listItems(thread.id)).some(item => item.data.type === "compaction" && item.data.summary.includes("Delayed summary proof"))).toBe(true);
    await expect(input).toHaveValue("Other conversation draft");
    await expect(page.locator(".transcript")).not.toContainText("Delayed summary proof");
    await page.getByRole("button", { name: thread.title, exact: true }).click();
    await expect(input).toBeEnabled();
    await expect(page.locator(".transcript")).toContainText("Delayed summary proof");
    expect(errors).toEqual([]);
  } finally {
    release();
    await writeFile(configPath, original);
    await new Promise<void>(resolve => provider.close(() => resolve()));
  }
});

test("conversation context inspector shows measured usage, retained estimates and live conversation isolation", async () => {
  await page.locator(".project-list .project-heading").filter({ hasText: /^workspace$/ }).click();
  const expand = page.getByRole("button", { name: "Expand project workspace", exact: true });
  if (await expand.count()) await expand.click();
  const source = await client.createThread({title:"Context source",provider:"echo",model:"echo-1"});
  const archive = await client.exportSessionArchive(source.id);
  const id = crypto.randomUUID(), now = new Date().toISOString();
  archive.rootThreadId = id;
  archive.threads[0].thread.id = id;
  archive.threads[0].thread.title = "Context accounting fixture";
  const known = (await client.providers()).flatMap(provider => provider.models.map(model => ({provider:provider.id, model}))).find(value => (value.model.contextWindow ?? 0) > 0)!;
  expect(known).toBeTruthy();
  archive.threads[0].thread.provider = known.provider;
  archive.threads[0].thread.model = known.model.id;
  const data = [
    {type:"user_message",text:"u".repeat(400)},
    {type:"assistant_message",text:"a".repeat(200)},
    {type:"usage",input_tokens:1200,output_tokens:300,cache_read_tokens:400,cache_write_tokens:80,cost_usd:0.025},
    {type:"compaction",summary:"s".repeat(40),files_touched:[],outstanding:[]},
    {type:"user_message",text:"next"},
    {type:"assistant_message",text:"reply123"},
    {type:"usage",input_tokens:500,output_tokens:100,cache_read_tokens:200,cache_write_tokens:20,cost_usd:null,first_token_ms:1240,stream_ms:2000,stream_tokens:80},
  ];
  archive.threads[0].items = data.map((data,index) => ({id:crypto.randomUUID(),threadId:id,turnId:null,createdAt:now,sequence:index+1,data})) as (typeof archive.threads)[number]["items"];
  await client.importSessionArchive({archive});
  await page.getByRole("button", {name:"Context accounting fixture",exact:true}).click();
  await expect(page.getByRole("button", {name:"View chat context",exact:true})).toHaveCount(0);
  await page.getByRole("button", { name: "Search", exact: true }).click();
  await page.getByRole("combobox", { name: "Search Haarness", exact: true }).fill("View chat context");
  await page.getByRole("option", { name: "View chat context", exact: true }).click();
  const panel = page.getByRole("region", {name:"Chat context",exact:true});
  const totals = panel.getByRole("region", {name:"Chat usage totals",exact:true});
  const latest = panel.getByRole("region", {name:"Latest request usage",exact:true});
  const metric = (region: Locator, label: string) => region.locator("dl > div").filter({has:page.locator("dt",{hasText:new RegExp("^"+label+"$")})}).locator("dd");
  await expect(metric(totals,"Input tokens")).toHaveText("1,700");
  await expect(metric(totals,"Output tokens")).toHaveText("400");
  await expect(metric(totals,"Cache read tokens")).toHaveText("600");
  await expect(metric(totals,"Cache write tokens")).toHaveText("100");
  await expect(metric(totals,"Known cost")).toHaveText("$0.0250");
  // Numbers follow the display language, as the labels around them do.
  const { translate } = await import("../src/localization");
  const translatedTotals = (code: string) =>
    page
      .getByRole("region", { name: translate(code, "Chat context"), exact: true })
      .getByRole("region", { name: translate(code, "Chat usage totals"), exact: true });
  await page.evaluate(() => window.haarness.preferences({ language: "de" }));
  await expect(page.locator("html")).toHaveAttribute("lang", "de");
  await expect(metric(translatedTotals("de"), translate("de", "Input tokens"))).toHaveText("1.700");
  await expect(metric(translatedTotals("de"), translate("de", "Known cost"))).toHaveText("0,0250\u00a0$");
  await page.screenshot({ path: test.info().outputPath("context-numbers-de-light.png") });
  await page.evaluate(() => window.haarness.preferences({ theme: "dark" }));
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await page.screenshot({ path: test.info().outputPath("context-numbers-de-dark.png") });
  await page.evaluate(() => window.haarness.preferences({ theme: "light", language: "ar" }));
  await expect(page.locator("html")).toHaveAttribute("dir", "rtl");
  await expect(metric(translatedTotals("ar"), translate("ar", "Known cost"))).not.toHaveText("Not available");
  await page.screenshot({ path: test.info().outputPath("context-numbers-rtl.png") });
  await page.evaluate(() => window.haarness.preferences({ language: "en" }));
  await expect(page.locator("html")).toHaveAttribute("lang", "en");
  await expect(metric(latest,"Input")).toHaveText("500");
  // Its pace: 1.24 s to the first content, then 80 streamed tokens over 2 s;
  // the other 20 output tokens never streamed.
  await expect(metric(latest,"First token")).toHaveText("1.2s");
  await expect(metric(latest,"Output speed")).toHaveText("40 tokens/s");
  await expect(latest).toContainText("at least half a second");
  // The last report after the compaction (500 in + 100 out), nothing appended since.
  const inUse = panel.getByRole("region", {name:"Context in use",exact:true});
  await expect(inUse.getByTestId("context-tokens")).toHaveText("600");
  await expect(inUse).toContainText("of " + known.model.contextWindow!.toLocaleString("en-US") + " tokens");
  await expect(inUse.getByRole("progressbar", {name:"Context in use"})).toBeVisible();
  await expect(panel.getByTestId("context-estimate")).toHaveText("13");
  await expect(panel.getByRole("progressbar", {name:"Estimated text share of context limit"})).toBeVisible();
  await expect(panel).toContainText(known.model.contextWindow!.toLocaleString("en-US") + " model context limit");
  await expect(panel).toContainText("Excludes system instructions");
  await expect(panel).toContainText("latest compaction summary");
  await page.screenshot({path:test.info().outputPath("context-inspector-dark.png")});
  const empty = await client.createThread({title:"Context empty fixture",provider:"echo",model:"echo-1"});
  await page.getByRole("button",{name:empty.title,exact:true}).click();
  await expect(panel).toContainText(empty.title);
  await expect(metric(totals,"Input tokens")).toHaveText("0");
  await expect(metric(totals,"Known cost")).toHaveText("Not available");
  await expect(panel.getByTestId("context-estimate")).toHaveText("0");
  await expect(panel).toContainText("Model context limit unavailable");
  await expect(panel.getByRole("progressbar")).toHaveCount(0);
  await expect(panel.getByRole("region", {name:"Context in use",exact:true})).toHaveCount(0);
  await expect(latest).toContainText("No usage report");
  const turn = await client.startTurn(empty.id,{text:"context live update",requestId:crypto.randomUUID()});
  await client.waitTurn(turn.id);
  const usage = await client.threadUsage(empty.id);
  await expect(metric(totals,"Input tokens")).toHaveText(usage.inputTokens.toLocaleString("en-US"));
  await expect(metric(latest,"Input")).toHaveText(usage.inputTokens.toLocaleString("en-US"));
  await expect(latest).toContainText("Echo 1");
  const prepared = latest.getByRole("region", {name:"Prepared request estimate",exact:true});
  await expect(prepared).toContainText("System instructions");
  await expect(prepared).toContainText("Tool definitions");
  const reports = await client.listItems(empty.id);
  const report = reports.filter(item => item.data.type === "usage").at(-1)!;
  expect(report.data.type === "usage" && report.data.prompt?.system_tokens).toBeGreaterThan(0);
  // The server timed the request; an echo reply streams in one burst, so it
  // has a first-token time and no speed.
  expect(report.data.type === "usage" && report.data.first_token_ms).toEqual(expect.any(Number));
  await expect(metric(latest,"First token")).toHaveText(/^\d+(\.\d)?s$/);
  await expect(metric(latest,"Output speed")).toHaveCount(0);
  await page.screenshot({path:test.info().outputPath("prepared-context-live.png")});

  await expect.poll(() => page.evaluate(() => window.haarness.storage("layout:v2"))).toMatchObject({dock:"context"});
  await page.reload();
  await expect(metric(totals,"Input tokens")).toHaveText(usage.inputTokens.toLocaleString("en-US"));
  await page.getByRole("button",{name:"Context accounting fixture",exact:true}).click();
  await expect(metric(totals,"Input tokens")).toHaveText("1,700");
  await page.evaluate(async () => {await window.haarness.preferences({theme:"light"});});
  await page.screenshot({path:test.info().outputPath("context-inspector-light.png")});
  const contextWindow = await app.browserWindow(page);
  const viewport = page.viewportSize();
  // Keep the test driver's emulated viewport in sync with the native resize.
  // A second CDP session cannot clear Playwright's own emulation override.
  await contextWindow.evaluate(win => win.setSize(900,700));
  expect(await contextWindow.evaluate(win => win.getSize()[0])).toBe(900);
  await page.setViewportSize({ width: 900, height: 700 });
  await expect.poll(() => page.evaluate(() => innerWidth)).toBeLessThanOrEqual(900);
  await totals.scrollIntoViewIfNeeded();
  await expect(metric(totals,"Known cost")).toBeInViewport();
  const bounds = await panel.evaluate(element => {const rect = element.getBoundingClientRect(); return {right:rect.right,width:innerWidth};});
  expect(bounds.right).toBeLessThanOrEqual(bounds.width);
  await page.screenshot({path:test.info().outputPath("context-inspector-small.png")});
  await contextWindow.evaluate(win => win.setSize(1420,940));
  await page.setViewportSize(viewport ?? { width: 1420, height: 940 });
  await page.evaluate(async () => {await window.haarness.preferences({theme:"dark"});});
  expect(errors).toEqual([]);
});

test("one remote server opens isolated projects with live events, files, terminal and restored drafts", async () => {
  const remoteFolder = join(temporary, "remote project 日本");
  await mkdir(join(remoteFolder, ".haarness"), { recursive: true });
  await writeFile(join(remoteFolder, ".haarness/config.toml"), '[defaults]\nprovider="echo"\nmodel="echo-1"\n');
  await writeFile(join(remoteFolder, "remote-only.txt"), "REMOTE_PROJECT_FILE\n");
  extraWorkspaces.push(remoteFolder);
  const endpoint = JSON.parse(execFileSync(binary, ["--workspace", workspace, "--trust-project", "desktop"], {
    env, input: JSON.stringify({ action: "connect" }), encoding: "utf8", timeout: 30000,
  }));
  const origin = endpoint.url;
  // Reacquire the source endpoint after any idle runtime replacement.
  client.close();
  client = new Haarness({ baseUrl: origin, token: endpoint.token });
  expect((await fetch(origin + "/v1/host/projects")).status).toBe(401);
  const denied = await fetch(origin + "/v1/host/open", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ path: remoteFolder }) });
  expect(denied.status).toBe(401);
  expect((await fetch(origin + "/v1/host/projects", { headers: { Authorization: "Bearer " + endpoint.token, Origin: "https://unrelated.example" } })).status).toBe(403);
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  const settings = page.getByRole("dialog", { name: "Settings", exact: true });
  await settings.getByRole("button", { name: "Servers", exact: true }).click();
  await settings.getByRole("button", { name: "Add remote server", exact: true }).click();
  const form = page.getByRole("dialog", { name: "Connect to server", exact: true });
  await form.getByLabel("Server URL", { exact: true }).fill(origin);
  await form.getByLabel("Name", { exact: true }).fill("Multi-project server");
  await form.getByRole("button", { name: "Connect", exact: true }).click();
  await expect(form).toHaveCount(0);
  if (await settings.isVisible()) await settings.getByRole("button", { name: "Close settings", exact: true }).click();
  await expect(page.getByRole("button", { name: "Switch server", exact: true })).toContainText("Multi-project server");
  await page.getByRole("button", { name: "Open another project", exact: true }).click();
  const picker = page.getByRole("dialog", { name: "Open remote project", exact: true });
  await expect(picker.getByLabel("Remote folder", { exact: true })).toHaveValue(workspace);
  await picker.getByLabel("Remote folder", { exact: true }).fill(join(temporary, "missing-project"));
  await picker.getByRole("button", { name: "Open project", exact: true }).click();
  await expect(picker.getByRole("alert")).toBeVisible();
  await picker.getByLabel("Remote folder", { exact: true }).fill(temporary);
  await picker.getByLabel("Remote folder", { exact: true }).press("Enter");
  await picker.getByRole("button", { name: "remote project 日本", exact: true }).click();
  await expect(picker.getByLabel("Remote folder", { exact: true })).toHaveValue(remoteFolder);
  await picker.getByRole("button", { name: "Open project", exact: true }).click();
  await expect(picker).toHaveCount(0);
  const boot = await page.evaluate(() => window.haarness.boot());
  const remote = boot.recent.find(item => item.remote && item.workspace === remoteFolder)!;
  expect(remote.id).not.toBe("remote:" + origin);
  expect(remote.remoteWorkspace).toBeTruthy();
  expect(boot.servers.filter(server => server.url === origin)).toHaveLength(1);
  expect((await fetch(origin + "/v1/host/workspaces/" + remote.remoteWorkspace + "/v1/info")).status).toBe(401);
  await expect(page.getByRole("button", { name: "Switch server", exact: true })).toContainText("Multi-project server");
  const scoped = new Haarness({ baseUrl: origin + "/v1/host/workspaces/" + remote.remoteWorkspace, token: endpoint.token });
  try {
    expect((await scoped.info()).workspace).toBe(remoteFolder);
    await send("remote-second-project-proof");
    await expect(page.locator(".transcript")).toContainText("Echo: remote-second-project-proof");
    expect((await client.listThreads()).some(thread => thread.title.includes("remote-second-project-proof"))).toBe(false);
    await page.getByRole("button", { name: "Settings", exact: true }).click();
    await settings.getByRole("button", { name: "Providers", exact: true }).click();
    // A remote server draws this page itself, with the TUI's wording.
    await settings.getByRole("button", { name: "Add a custom endpoint", exact: true }).click();
    await settings.getByLabel("Provider ID", { exact: true }).fill("remote-manager-fixture");
    await settings.getByLabel("Display name", { exact: true }).fill("Remote manager fixture");
    await settings.getByLabel("Endpoint URL (HTTPS or local HTTP)", { exact: true }).fill("http://127.0.0.1:1/v1");
    await settings.getByLabel("Model IDs (comma separated)", { exact: true }).fill("fixture");
    await settings.getByRole("button", { name: "Save", exact: true }).click();
    await expect.poll(async () => (await scoped.providers()).some(provider => provider.id === "remote-manager-fixture")).toBe(true);
    await settings.getByRole("button", { name: "Back to list", exact: true }).click();
    await settings.getByRole("button", { name: "Remote manager fixture", exact: true }).click();
    await settings.getByRole("button", { name: "Use an API key", exact: true }).click();
    await settings.getByLabel("API key", { exact: true }).fill("isolated-fixture-key");
    await expect(settings.getByLabel("API key", { exact: true })).toHaveAttribute("type", "password");
    await settings.getByRole("button", { name: "Save", exact: true }).click();
    await expect(settings.getByRole("heading", { name: "Providers", exact: true })).toBeVisible();
    expect(await settings.innerText()).not.toContain("isolated-fixture-key");
    await settings.getByRole("button", { name: "Close settings", exact: true }).click();
    const file = await page.evaluate(id => window.haarness.files<any>(id, "read", { path: "remote-only.txt" }), remote.id);
    expect(file.content).toBe("REMOTE_PROJECT_FILE\n");
    await page.getByRole("button", { name: "Toggle terminal", exact: true }).click();
    const terminalInput = page.locator(".xterm-helper-textarea");
    await expect(terminalInput).toBeFocused();
    await terminalInput.pressSequentially("pwd > remote-terminal-cwd.txt", { delay: 5 });
    await terminalInput.press("Enter");
    await expect.poll(() => readFile(join(remoteFolder, "remote-terminal-cwd.txt"), "utf8").catch(() => "")).toBe(remoteFolder + "\n");
    const terminal = (await scoped.listPtys())[0];
    await page.getByRole("button", { name: "Hide terminal", exact: true }).click();
    const input = page.getByRole("textbox", { name: "Message", exact: true });
    await input.fill("draft in second remote project");
    const chooseProject = async (name: string) => {
      await page.locator(".project-list .project-heading").filter({ hasText: new RegExp("^" + name + "$") }).click();
      await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
    };
    await chooseProject("workspace");
    await newSession();
    await input.fill("draft in first remote project");
    await chooseProject("remote project 日本");
    await expect(input).toHaveValue("draft in second remote project");
    await page.getByRole("button", { name: "Switch server", exact: true }).click();
    await page.getByRole("menuitem", { name: "This computer", exact: true }).click();
    await expect(page.locator(".project-list")).not.toContainText("remote project 日本");
    await page.getByRole("button", { name: "Switch server", exact: true }).click();
    await page.getByRole("menuitem", { name: "Multi-project server", exact: true }).click();
    await expect(input).toHaveValue("draft in second remote project");
    // A pending question is durable and still reachable after changing projects.
    await input.fill('!tool question {"prompt":"Remote question survives switching","choices":["Continue"]}');
    await page.getByRole("button", { name: "Send message", exact: true }).click();
    await expect.poll(() => scoped.listQuestions().then(items => items.length)).toBe(1);
    await chooseProject("workspace");
    await expect(input).toHaveValue("draft in first remote project");
    await page.reload();
    await expect(input).toHaveValue("draft in first remote project");
    await chooseProject("remote project 日本");
    await expect(page.getByText("Remote question survives switching", { exact: true }).first()).toBeVisible();
    const [question] = await scoped.listQuestions();
    await scoped.replyQuestion(question.questionId, { answer: "Continue" });
    await expect.poll(() => scoped.activeTurns().then(items => items.length)).toBe(0);
    await page.getByRole("button", { name: "Toggle terminal", exact: true }).click();
    await expect(page.locator(".xterm-screen")).toBeVisible();
    expect((await scoped.listPtys())[0].id).toBe(terminal.id);
    await page.getByRole("button", { name: "Hide terminal", exact: true }).click();
    await scoped.deletePty(terminal.id);
    await page.screenshot({ path: test.info().outputPath("remote-projects.png") });
    await page.getByRole("button", { name: "Open another project", exact: true }).click();
    await expect(picker.getByRole("list", { name: "Remote folders", exact: true })).toBeVisible();
    await expect(picker.getByText("Known projects and working folders")).toHaveCount(0);
    await page.screenshot({ path: test.info().outputPath("remote-folder-picker.png") });
    await picker.getByRole("button", { name: "Cancel", exact: true }).click();
    // Forget a folder without deleting the host or its credentials/history.
    await page.evaluate(id => window.haarness.forget(id), remote.id);
    const saved = await page.evaluate(() => window.haarness.boot());
    expect(saved.recent.some(item => item.id === remote.id)).toBe(false);
    expect(saved.servers.some(server => server.url === origin)).toBe(true);
    expect((await scoped.listThreads()).length).toBeGreaterThan(0);
    expect(errors).toEqual([]);
    // Leave the shared test window on the local project for subsequent editors.
    await page.getByRole("button", { name: "Switch server", exact: true }).click();
    await page.getByRole("menuitem", { name: "This computer", exact: true }).click();
    await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
  } finally { scoped.close(); }
});

test("hooks settings execute configured commands and preserve stale editor drafts", async () => {
  await page.locator('.project-group[aria-label="Project workspace"] .project-heading').click();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
  await attachClient();
  const path = join(workspace, "haarness.jsonc");
  const original = await readFile(path, "utf8").catch(() => undefined);
  const marker = join(workspace, "desktop-hook-proof.txt");
  async function reloaded(since: number) {
    for await (const event of client.events({after:0,reconnect:false,signal:AbortSignal.timeout(15000)})) {
      if (event.type === "configuration.reloaded" && Date.parse(event.createdAt) >= since) return;
    }
    throw Error("Configuration reload was not observed");
  }
  let changedAt = 0;
  try {
    changedAt = Date.now();
    await writeFile(path, '{\n // Keep this comment.\n "hooks": {}\n}\n');
    await reloaded(changedAt);
    await page.getByRole("button", { name: "Settings", exact: true }).click();
    const settings = page.getByRole("dialog", { name: "Settings", exact: true });
    await settings.getByRole("button", { name: "Hooks", exact: true }).click();
    await settings.getByRole("button", { name: "Add hook", exact: true }).click();
    const form = page.getByRole("dialog", { name: "Add hook", exact: true });
    // The matcher suggests what the chosen event can match.
    const matcher = form.getByRole("combobox", { name: "Hook matcher", exact: true });
    await selectValue(form.getByRole("combobox", { name: "Hook event", exact: true }), "PostCompact");
    await matcher.press("ArrowDown");
    await expect(page.getByRole("option", { name: /^manual/ })).toBeVisible();
    await expect(page.getByRole("option", { name: /^automatic/ })).toBeVisible();
    await matcher.press("Escape");
    await selectValue(form.getByRole("combobox", { name: "Hook event", exact: true }), "StopFailure");
    await matcher.press("ArrowDown");
    await expect(page.getByRole("option", { name: /^rate_limit/ })).toBeVisible();
    await expect(page.getByRole("option", { name: /^content_filter/ })).toBeVisible();
    await page.screenshot({ path: test.info().outputPath("failure-hook-editor.png") });
    await matcher.press("Escape");
    await selectValue(form.getByRole("combobox", { name: "Hook event", exact: true }), "Stop");
    await expect(matcher).toHaveCount(0);
    const command = "printf desktop-hook-proof > desktop-hook-proof.txt";
    await form.getByLabel("Hook command", { exact: true }).fill(command);
    changedAt = Date.now();
    await form.getByRole("button", { name: "Save hook", exact: true }).click();
    await reloaded(changedAt);
    await expect(form).not.toBeVisible();
    const card = settings.locator(".management-card").filter({ hasText: command });
    await expect(card).toBeVisible();
    expect(await readFile(path, "utf8")).toContain("Keep this comment.");
    await page.screenshot({ path: test.info().outputPath("hooks-settings.png") });
    await settings.getByRole("button", { name: "Close settings" }).click();
    await newSession();
    await send("hook execution proof");
    await expect.poll(() => readFile(marker, "utf8").catch(() => "")).toBe("desktop-hook-proof");
    await expect.poll(() => client.activeTurns().then(items => items.length)).toBe(0);
    await page.getByRole("button", { name: "Settings", exact: true }).click();
    await settings.getByRole("button", { name: "Hooks", exact: true }).click();
    await card.getByRole("button", { name: "Edit hook", exact: true }).click();
    const edit = page.getByRole("dialog", { name: "Edit hook", exact: true });
    await edit.getByLabel("Hook command", { exact: true }).fill("echo retained draft");
    changedAt = Date.now();
    await writeFile(path, (await readFile(path, "utf8")) + "\n// External edit\n");
    await reloaded(changedAt);
    await edit.getByRole("button", { name: "Save hook", exact: true }).click();
    await expect(edit.getByRole("alert")).toContainText("changed outside");
    await expect(edit.getByLabel("Hook command", { exact: true })).toHaveValue("echo retained draft");
    await edit.getByRole("button", { name: "Cancel", exact: true }).click();
    await settings.getByRole("button", { name: "Refresh hooks" }).click();
    changedAt = Date.now();
    await card.getByRole("switch").uncheck();
    await reloaded(changedAt);
    await expect.poll(async () => /"disabled"\s*:\s*true/.test(await readFile(path, "utf8"))).toBe(true);
    await settings.getByRole("button", { name: "Close settings" }).click();
    await rm(marker);
    await newSession();
    await send("disabled hook proof");
    await expect(page.locator(".transcript")).toContainText("Echo: disabled hook proof");
    await expect.poll(() => client.activeTurns().then(items => items.length)).toBe(0);
    expect(await readFile(marker, "utf8").catch(() => "missing")).toBe("missing");
    await page.getByRole("button", { name: "Settings", exact: true }).click();
    await settings.getByRole("button", { name: "Hooks", exact: true }).click();
    await card.getByRole("button", { name: "Remove hook", exact: true }).click();
    changedAt = Date.now();
    await page.getByRole("dialog", { name: "Remove hook?", exact: true }).getByRole("button", { name: "Remove hook", exact: true }).click();
    await expect(card).not.toBeVisible();
    await reloaded(changedAt);
    await settings.getByRole("button", { name: "Close settings" }).click();
    await rm(marker, { force: true });
    await newSession();
    await send("removed hook proof");
    await expect(page.locator(".transcript")).toContainText("Echo: removed hook proof");
    await expect.poll(() => client.activeTurns().then(items => items.length)).toBe(0);
    expect(await readFile(marker, "utf8").catch(() => "missing")).toBe("missing");
    expect(errors).toEqual([]);
  } finally {
    if (original === undefined) await rm(path, { force: true }); else await writeFile(path, original);
    await rm(marker, { force: true });
  }
});

test("memory dialog manages project files, retains conflicting edits and reloads tool availability", async () => {
  await newSession();
  await send("/memory");
  const memory = page.getByRole("dialog", { name: "Memory", exact: true });
  await expect(memory).toBeVisible();
  await memory.getByRole("button", { name: "Add memory", exact: true }).click();
  const add = page.getByRole("dialog", { name: "Add memory", exact: true });
  await add.getByLabel("Memory name", { exact: true }).fill("desktop-memory-proof");
  await add.getByLabel("Memory description", { exact: true }).fill("Architecture decisions");
  await add.getByLabel("Memory content", { exact: true }).fill("Keep the desktop editors thin and reuse native services.");
  await add.getByRole("button", { name: "Save memory", exact: true }).click();
  await expect(add).not.toBeVisible();
  await expect(memory).toContainText("Architecture decisions");
  await memory.getByRole("button", { name: "Edit memory desktop-memory-proof", exact: true }).click();
  const edit = page.getByRole("dialog", { name: "Edit memory", exact: true });
  await expect(edit.getByLabel("Memory content", { exact: true })).toHaveValue("Keep the desktop editors thin and reuse native services.");
  const connection = (await page.evaluate(() => window.haarness.boot())).recent.find(item => !item.remote && !item.chatHome)!;
  const status = await page.evaluate(id => window.haarness.native<any>(id, { action: "asset", kind: "memory", input: { action: "status" } }), connection.id);
  const path = join(status.directory, "desktop-memory-proof.md");
  await writeFile(path, (await readFile(path, "utf8")) + "\nExternal memory edit.\n");
  await edit.getByLabel("Memory content", { exact: true }).fill("Preserve this draft");
  await edit.getByRole("button", { name: "Save memory", exact: true }).click();
  await expect(edit.getByRole("alert")).toContainText("Memory changed");
  await expect(edit.getByLabel("Memory content", { exact: true })).toHaveValue("Preserve this draft");
  await edit.getByRole("button", { name: "Cancel", exact: true }).click();
  await memory.getByLabel("Memory index limit", { exact: true }).fill("1");
  await memory.getByRole("switch", { name: "Enable project memory" }).uncheck();
  await memory.getByRole("button", { name: "Save memory settings", exact: true }).click();
  await expect(memory.getByRole("status")).toContainText("Memory settings saved");
  await expect.poll(async () => (await client.toolCatalog()).tools.find(tool => tool.id === "memory_save")?.enabled).toBe(false);
  await page.screenshot({ path: test.info().outputPath("memory-dialog.png") });
  await memory.getByRole("button", { name: "Close", exact: true }).click();
  await page.getByRole("button", { name: "Search", exact: true }).click();
  await page.getByRole("combobox", { name: "Search Haarness" }).fill("Manage memory");
  await page.getByRole("option", { name: "Manage memory", exact: true }).click();
  await expect(memory.getByRole("switch", { name: "Enable project memory" })).not.toBeChecked();
  await expect(memory.getByLabel("Memory index limit", { exact: true })).toHaveValue("1");
  await memory.getByRole("switch", { name: "Enable project memory" }).check();
  await memory.getByRole("button", { name: "Save memory settings", exact: true }).click();
  await expect.poll(async () => (await client.toolCatalog()).tools.find(tool => tool.id === "memory_save")?.enabled).toBe(true);
  await memory.getByRole("button", { name: "Edit memory desktop-memory-proof", exact: true }).click();
  await expect(edit.getByLabel("Memory content", { exact: true })).toHaveValue(/External memory edit\./);
  await edit.getByRole("button", { name: "Delete memory", exact: true }).click();
  await page.getByRole("dialog", { name: "Delete memory?", exact: true }).getByRole("button", { name: "Delete memory", exact: true }).click();
  await expect(edit).not.toBeVisible();
  await expect(memory.getByRole("button", { name: "Edit memory desktop-memory-proof", exact: true })).not.toBeVisible();
  expect(await readFile(path, "utf8").catch(() => "missing")).toBe("missing");
  await memory.getByRole("button", { name: "Close", exact: true }).click();

  // A settings file Haarness cannot read keeps memory off, and says so.
  const settings = join(status.directory, "settings.json");
  await writeFile(settings, '{"enabled": true,}');
  await page.getByRole("button", { name: "Search", exact: true }).click();
  await page.getByRole("combobox", { name: "Search Haarness" }).fill("Manage memory");
  await page.getByRole("option", { name: "Manage memory", exact: true }).click();
  const unreadable = "Memory is off because its settings file could not be read. Saving replaces it.";
  await expect(memory.getByRole("status")).toHaveText(unreadable);
  await expect(memory.getByRole("switch", { name: "Enable project memory" })).not.toBeChecked();
  await page.screenshot({ path: test.info().outputPath("memory-dialog-unreadable.png") });
  await page.emulateMedia({ colorScheme: "dark" });
  await page.screenshot({ path: test.info().outputPath("memory-dialog-unreadable-dark.png") });
  await page.emulateMedia({ colorScheme: null });
  await memory.getByRole("switch", { name: "Enable project memory" }).check();
  await memory.getByRole("button", { name: "Save memory settings", exact: true }).click();
  await expect(memory.getByRole("status")).toContainText("Memory settings saved");
  await expect(memory).not.toContainText(unreadable);
  expect(JSON.parse(await readFile(settings, "utf8")).enabled).toBe(true);
  await memory.getByRole("button", { name: "Close", exact: true }).click();
  expect(errors).toEqual([]);
});



test("permission reply failures remain attached through unrelated actions and clear after resolution", async () => {
  const thread = await client.createThread({title:"Permission retry fixture",provider:"echo",model:"echo-1"});
  await page.getByRole("button", {name:"Search",exact:true}).click();
  await page.getByRole("combobox", {name:"Search Haarness"}).fill(thread.title);
  await page.getByRole("option").filter({hasText:thread.title}).first().click();
  const turn = await client.startTurn(thread.id,{text:'!tool bash {"command":"printf retry-proof > retry-proof.txt"}',requestId:crypto.randomUUID()});
  await expect(page.getByRole("button",{name:"Allow once",exact:true})).toBeVisible();
  // Inject a failed reply at the native API boundary while the real server,
  // SSE subscription and pending permission remain live.
  await app.evaluate(({ ipcMain }) => {
    const handlers = (ipcMain as unknown as { _invokeHandlers: Map<string, (...args: any[]) => unknown> })._invokeHandlers;
    const original = handlers.get("haarness:api")!;
    ipcMain.removeHandler("haarness:api");
    ipcMain.handle("haarness:api", (event, raw) => {
      if (raw.method === "replyPermission") {
        ipcMain.removeHandler("haarness:api");
        ipcMain.handle("haarness:api", original);
        return {ok:false,error:"Approval transport fixture unavailable"};
      }
      return original(event, raw);
    });
  });
  await page.getByRole("button",{name:"Allow once",exact:true}).click();
  const failure = page.getByTestId("permission-reply-error");
  await expect(failure).toContainText("Approval transport fixture unavailable");
  await page.getByRole("button",{name:"Settings",exact:true}).click();
  const appearance = page.getByRole("dialog",{name:"Settings",exact:true}).getByRole("combobox",{name:"Appearance",exact:true});
  // Saving preferences uses the shared action wrapper that clears global
  // errors; the pending request's own error must survive that successful work.
  await selectValue(appearance, "light");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  await selectValue(appearance, "dark");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await page.getByRole("button",{name:"Close settings",exact:true}).click();
  await expect(failure).toContainText("Approval transport fixture unavailable");
  expect(await client.listPermissions()).toHaveLength(1);
  await page.screenshot({path:test.info().outputPath("permission-retry-retained.png")});
  await page.getByRole("button",{name:"Allow once",exact:true}).click();
  await client.waitTurn(turn.id);
  await expect(failure).toHaveCount(0);
  expect(await readFile(join(workspace,"retry-proof.txt"),"utf8")).toBe("retry-proof");
});

test("a card answered elsewhere says so in one quiet line where its buttons were", async () => {
  const thread = await client.createThread({title:"Settled card fixture",provider:"echo",model:"echo-1"});
  await page.getByRole("button", {name:"Search",exact:true}).click();
  await page.getByRole("combobox", {name:"Search Haarness"}).fill(thread.title);
  await page.getByRole("option").filter({hasText:thread.title}).first().click();
  const turn = await client.startTurn(thread.id,{text:'!tool bash {"command":"printf settled > settled-card.txt"}',requestId:crypto.randomUUID()});
  const card = page.getByRole("alert", { name: "Permission request" });
  const once = card.getByRole("button",{name:"Allow once",exact:true});
  await expect(once).toBeVisible();
  const [pending] = await client.listPermissions();
  // The server answers the reply "not found", as it does for a request
  // settled meanwhile; the request itself stays for this check.
  await app.evaluate(({ ipcMain }, id) => {
    const handlers = (ipcMain as unknown as { _invokeHandlers: Map<string, (...args: any[]) => unknown> })._invokeHandlers;
    const original = handlers.get("haarness:api")!;
    ipcMain.removeHandler("haarness:api");
    ipcMain.handle("haarness:api", (event, raw) => {
      if (raw.method === "replyPermission") {
        ipcMain.removeHandler("haarness:api");
        ipcMain.handle("haarness:api", original);
        return {ok:false,error:`not found: permission ${id}`};
      }
      return original(event, raw);
    });
  }, pending.permissionId);
  // The card eases in: measure where it rests.
  const still = () =>
    page.evaluate(async () => {
      await Promise.all(
        document
          .getAnimations()
          .filter((item) => item.effect?.getComputedTiming().iterations !== Infinity)
          .map((item) => item.finished.catch(() => undefined)),
      );
      await new Promise(requestAnimationFrame);
    });
  await still();
  const footer = card.locator(".prompt-actions");
  const before = (await footer.boundingBox())!;
  const height = (await card.boundingBox())!.height;
  await once.click();
  const settled = card.locator(".approval-settled");
  await expect(settled).toHaveText("Already answered");
  // The click had focus on a button: the line keeps it.
  await expect(settled).toBeFocused();
  await expect(card.getByRole("button")).toHaveCount(0);
  await still();
  const after = (await settled.boundingBox())!;
  expect(Math.abs(after.y - before.y)).toBeLessThan(2);
  expect(Math.abs((await card.boundingBox())!.height - height)).toBeLessThan(2);
  // One quiet line, not a toast, and no error.
  await expect(page.locator(".toast")).toHaveCount(0);
  await expect(page.getByTestId("permission-reply-error")).toHaveCount(0);
  await card.screenshot({path:test.info().outputPath("auto-review-settled.png")});
  await client.replyPermission(pending.permissionId, {decision:"reject"});
  await client.waitTurn(turn.id);
  await expect(card).toHaveCount(0);
  await expect(page.locator(".toast")).toHaveCount(0);
});

test("self-hosted router connects address-first and requests a key only when needed", async () => {
  const calls: { path: string; authorization: string }[] = [];
  const router = createServer((request, response) => {
    calls.push({ path: request.url ?? "", authorization: request.headers.authorization ?? "" });
    if (request.url?.startsWith("/secure/") && request.headers.authorization !== "Bearer router-fixture-key") {
      response.writeHead(401, { "content-type": "application/json" });
      response.end(JSON.stringify({ error: { message: "key required" } }));
      return;
    }
    response.writeHead(200, { "content-type": "application/json" });
    response.end(JSON.stringify({ data: [{ id: "openai/router-fixture" }, { id: "fast" }] }));
  });
  await new Promise<void>((resolve) => router.listen(0, "127.0.0.1", resolve));
  const address = router.address();
  if (!address || typeof address === "string") throw Error("router fixture not listening");
  try {
    await page.getByRole("button", { name: "Settings", exact: true }).click();
    const settings = page.getByRole("dialog", { name: "Settings", exact: true });
    await settings.getByRole("button", { name: "Providers", exact: true }).click();
    for (const [prefix, id, name] of [["open", "router", "Studio router"], ["secure", "router_2", "Team router"]]) {
      await settings.getByRole("button", { name: "Connect Haarness Router", exact: true }).click();
      const form = page.getByRole("dialog", { name: "Connect Haarness Router", exact: true });
      await expect(form.getByLabel("Router access key", { exact: true })).toHaveCount(0);
      await form.getByLabel("Router address", { exact: true }).fill(`http://127.0.0.1:${address.port}/${prefix}`);
      await form.getByRole("button", { name: "Advanced", exact: true }).click();
      await expect(form.getByLabel("Router address", { exact: true })).toHaveValue(`http://127.0.0.1:${address.port}/${prefix}`);
      await form.getByLabel("Connection name (optional)", { exact: true }).fill(name);
      await form.getByRole("button", { name: "Connect", exact: true }).click();
      if (prefix === "secure") {
        const key = form.getByLabel("Router access key", { exact: true });
        await expect(key).toHaveAttribute("type", "password");
        await key.fill("wrong-fixture-key");
        await form.getByRole("button", { name: "Connect", exact: true }).click();
        await expect(form.getByRole("alert")).toContainText("access key rejected");
        expect((await client.refreshProviders()).some((p) => p.id === id)).toBe(false);
        await key.fill("router-fixture-key");
        await form.getByRole("button", { name: "Connect", exact: true }).click();
      }
      await expect(form.getByRole("heading", { name: "Router connected", exact: true })).toBeVisible();
      await expect(form).toContainText(`${name} · Self-hosted · 2 models available`);
      await page.screenshot({ path: test.info().outputPath(`${prefix}-router-connected.png`) });
      await form.getByRole("button", { name: "Done", exact: true }).click();
      await expect.poll(async () => (await client.refreshProviders()).find((p) => p.id === id)?.models.map((m) => m.id).sort()).toEqual(["fast", "openai/router-fixture"]);
    }
    expect(calls.some((call) => call.path === "/open/v1/models" && call.authorization === "")).toBe(true);
    expect(calls.some((call) => call.path === "/secure/v1/models" && call.authorization === "Bearer router-fixture-key")).toBe(true);
  } finally {
    const close = page.getByRole("button", { name: "Close settings", exact: true });
    if (await close.isVisible()) await close.click();
    router.closeAllConnections();
    await new Promise<void>((resolve) => router.close(() => resolve()));
  }
});

test("self-hosted router completes a live Desktop tool round trip", async () => {
  test.skip(!process.env.HAARNESS_ROUTER_LIVE_URL, "Opt-in live router endpoint");
  const settings = page.getByRole("dialog", { name: "Settings", exact: true });
  if (!await settings.isVisible()) await page.getByRole("button", { name: "Settings", exact: true }).click();
  await settings.getByRole("button", { name: "Providers", exact: true }).click();
  await settings.getByRole("button", { name: "Connect Haarness Router", exact: true }).click();
  const form = page.getByRole("dialog", { name: "Connect Haarness Router", exact: true });
  await form.getByLabel("Router address", { exact: true }).fill(process.env.HAARNESS_ROUTER_LIVE_URL!);
  await form.getByRole("button", { name: "Advanced", exact: true }).click();
  await form.getByLabel("Connection name (optional)", { exact: true }).fill("Live self-hosted router");
  await form.getByRole("button", { name: "Connect", exact: true }).click();
  await expect(form.getByRole("heading", { name: "Router connected", exact: true })).toBeVisible();
  await form.getByRole("button", { name: "Done", exact: true }).click();
  let providerId = "";
  await expect.poll(async () => {
    const provider = (await client.refreshProviders()).find((p) => p.name === "Live self-hosted router");
    providerId = provider?.id ?? "";
    return provider?.models.some((model) => model.id === "openai/gpt-6-astra");
  }).toBe(true);
  await settings.getByRole("button", { name: "Close settings", exact: true }).click();
  await newSession();
  await selectValue(page.getByRole("combobox", { name: "Model", exact: true }), `${providerId}/openai/gpt-6-astra`);
  await writeFile(join(workspace, "router-proof.txt"), "The marker is desktop-router-7392.\n");
  await send("Read router-proof.txt using the read tool. Reply with exactly the marker from the file. Do not use other tools.");
  await expect(page.locator(".transcript")).toContainText("desktop-router-7392", { timeout: 90000 });
  const thread = (await client.listThreads()).find((t) => t.provider === providerId);
  expect(thread).toBeTruthy();
  const items = await client.listItems(thread!.id);
  expect(items.some((item) => item.data.type === "tool_call" && item.data.name.split(".").at(-1) === "read")).toBe(true);
  await page.screenshot({ path: test.info().outputPath("live-desktop-router.png") });
  await test.info().attach("live-router-items.json", { body: JSON.stringify(items, null, 2), contentType: "application/json" });
  const routerWindow = app.waitForEvent("window");
  await app.evaluate(({ BrowserWindow }, url) => {
    const window = new BrowserWindow({ show: false, width: 1280, height: 900 });
    void window.loadURL(url + "/ui");
  }, process.env.HAARNESS_ROUTER_LIVE_URL!);
  const playground = await routerWindow;
  await expect(playground.locator("#router-status")).toHaveText("Ready");
  await playground.locator('[data-view="playground"]').click();
  await expect(playground.locator("#playground-key-state")).toHaveText("No key required");
  await playground.locator("#playground-model").selectOption("openai/gpt-5-nano");
  await playground.locator("#playground-message").fill("Reply with exactly: NO-KEY-ROUTER-UI-928");
  await playground.getByText("Tools, attachments & generation controls", { exact: true }).click();
  await playground.locator("#playground-max-tokens").fill("2048");
  const sent = playground.waitForRequest((request) => request.url().endsWith("/v1/chat/completions"));
  await playground.locator("#playground-send").click();
  expect((await sent).headers().authorization).toBeUndefined();
  await expect(playground.locator("#response-content")).toContainText("NO-KEY-ROUTER-UI-928", { timeout: 90000 });
  await playground.getByText("Tools, attachments & generation controls", { exact: true }).click();
  await playground.locator("#playground-key-state").scrollIntoViewIfNeeded();
  await playground.screenshot({ path: test.info().outputPath("router-playground-no-key.png") });
  await playground.close();
});

test("file explorer highlights configuration and programming languages in both themes", async () => {
  test.setTimeout(90000);
  const fixtures = [
    { path: ".explorer-proof.txt", text: 'Visible dotfile.\n', token: null },
    { path: "build.yml", text: '---\n# Build configuration\nhosts: "localhost"\nretries: 3\nenabled: true\n', token: "hosts" },
    { path: "config.toml", text: '[package]\nname = "demo"\nversion = 3\n', token: '"demo"' },
    { path: "env.sh", text: '#!/bin/sh\nif true; then echo "hello"; fi\n', token: "if" },
    { path: "Dockerfile", text: 'FROM alpine:3\nRUN echo "hello"\n', token: "FROM" },
    { path: "main.go", text: 'package main\nfunc main() { println("hello") }\n', token: "func" },
    { path: "Main.java", text: 'public class Main { int count = 3; }\n', token: "public" },
    { path: "main.cpp", text: 'int main() { return 3; }\n', token: "return" },
    { path: "query.sql", text: 'SELECT name FROM users WHERE id = 3;\n', token: "SELECT" },
    { path: "main.swift", text: 'let message = "hello"\n', token: '"hello"' },
    { path: "main.kt", text: 'val message = "hello"\n', token: '"hello"' },
    { path: "sample.ts", text: 'export const message = "hello";\n', token: "const" },
    { path: "sample.py", text: 'def hello():\n    return "hello"\n', token: "return" },
    { path: "notes.txt", text: 'Ordinary unformatted prose.\n', token: null },
  ];
  for (const file of fixtures) await writeFile(join(workspace, file.path), file.text);
  await openProjectPanel("Files");
  await page.getByRole("button", { name: "Refresh file tree", exact: true }).click();
  const tree = page.locator(".file-tree");
  await expect(tree.getByRole("button", { name: ".haarness", exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "Show hidden files", exact: true })).toHaveCount(0);
  const editor = page.locator(".file-preview .cm-content");
  const colored = () => editor.evaluate(element => {
    const base = getComputedStyle(element).color;
    return [...element.querySelectorAll(".cm-line span")].filter(span => getComputedStyle(span).color !== base).map(span => span.textContent).join(" ");
  });
  for (const theme of ["dark", "light"] as const) {
    await page.evaluate(theme => window.haarness.preferences({ theme }), theme);
    for (const file of fixtures) {
      await tree.getByRole("button", { name: file.path, exact: true }).click();
      await expect(editor).toContainText(file.text.trim().split("\n").at(-1)!);
      if (file.token) await expect.poll(colored, { message: `${file.path} is highlighted in ${theme}` }).toContain(file.token);
      else await expect.poll(colored).toBe("");
      if (["build.yml", "Dockerfile", "env.sh"].includes(file.path))
        await page.screenshot({ path: test.info().outputPath(`${file.path}-${theme}.png`), animations: "disabled" });
    }
  }
  // Switching files must not let a deferred parser recolor the wrong document.
  for (const path of ["build.yml", "main.swift", "Dockerfile", "env.sh"])
    await tree.getByRole("button", { name: path, exact: true }).click();
  await expect.poll(colored).toContain("if");
  const updated = '#!/bin/sh\nif true; then echo "edited"; fi\n';
  await editor.fill(updated);
  await expect.poll(colored).toContain('"edited"');
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect.poll(() => readFile(join(workspace, "env.sh"), "utf8")).toBe(updated);
  expect(errors).toEqual([]);
});

// A conversation with these prompts, the first sent through the composer and
// the rest through the server, so the transcript fills the way a user's does.
async function outlineSession(prompts: string[]) {
  await newSession();
  await send(prompts[0]);
  const first = prompts[0].split("\n")[0];
  await expect
    .poll(async () => (await client.listThreads()).find((thread) => thread.title.includes(first))?.id)
    .toBeTruthy();
  const id = (await client.listThreads()).find((thread) => thread.title.includes(first))!.id;
  // No active turn also describes the moment before the composer's turn
  // registers, so wait for its reply before starting the next one.
  await expect(page.locator(".transcript")).toContainText("Echo: " + first);
  await expect.poll(() => client.activeTurn(id)).toBeNull();
  for (const text of prompts.slice(1)) {
    const turn = await client.startTurn(id, { requestId: crypto.randomUUID(), text });
    await client.waitTurn(turn.id, { timeout_ms: 20000 });
  }
  await expect(page.locator(".transcript")).toContainText("Echo: " + prompts.at(-1)!.split("\n")[0]);
  return id;
}
const outline = () => page.locator(".outline");
const outlineDashes = () => page.locator(".outline-dash");
const outlineRows = () => page.locator(".outline-row");
// Hover a dash the way a pointer does, moving onto it until the panel opens.
async function openOutlineOn(index: number) {
  await expect
    .poll(async () => {
      const box = (await outlineDashes().nth(index).boundingBox())!;
      await page.mouse.move(box.x + 4, box.y + 3);
      await page.mouse.move(box.x + 4, box.y + 1);
      return outline().getAttribute("data-state");
    })
    .toBe("open");
}
const transcriptTop = async () => (await page.locator(".transcript").boundingBox())!.y;

test("conversation outline rests as dashes at the top left and opens into a panel of prompts", async () => {
  await page.setViewportSize({ width: 1420, height: 940 });
  const prompts = Array.from({ length: 14 }, (_, i) => {
    const lines = (i + 1) % 4 === 0 ? 60 : 1;
    return Array.from({ length: lines }, (_, line) => `outline turn ${i + 1} line ${line + 1}`).join("\n");
  });
  await outlineSession(prompts);
  // One dash per prompt, stacked 10px apart at the top of the transcript.
  await expect(outlineDashes()).toHaveCount(14);
  const tops = async () =>
    Promise.all((await outlineDashes().all()).map(async (dash) => (await dash.boundingBox())!.y));
  const rest = await tops();
  for (let i = 1; i < rest.length; i += 1) expect(rest[i] - rest[i - 1]).toBeLessThanOrEqual(10.5);
  expect(rest[0] - (await transcriptTop())).toBeLessThan(60);
  // At the end of the conversation the newest prompt is the one being read.
  await expect(outlineDashes().last()).toHaveAttribute("aria-current", "location");
  await expect(outline()).toHaveAttribute("aria-label", "Chat outline");

  // A shorter window shrinks the transcript from the bottom; the stack holds still.
  const dashTop = async () => (await outlineDashes().first().boundingBox())!.y;
  const startTop = await dashTop();
  for (const height of [820, 700, 940]) {
    await page.setViewportSize({ width: 1420, height });
    await page.waitForTimeout(100);
    expect(Math.abs((await dashTop()) - startTop)).toBeLessThanOrEqual(1);
  }

  // The panel lists every prompt by its first line; the row the pointer opened on is under it.
  await openOutlineOn(4);
  await expect(outlineRows()).toHaveCount(14);
  await expect(outlineRows().nth(4)).toHaveText("outline turn 5 line 1");
  await expect(outlineRows().nth(3)).toHaveText("outline turn 4 line 1");
  await expect(page.locator('.outline-row[data-state="hot"]')).toHaveCount(1);
  await outlineRows().nth(4).hover();
  await expect(outlineRows().nth(4)).toHaveAttribute("data-state", "hot");
  await page.screenshot({ path: "test-results/outline-open.png" });

  // A panel with nothing to scroll hands the wheel to the conversation.
  const transcript = page.locator(".transcript");
  const before = await transcript.evaluate((root) => root.scrollTop);
  await page.mouse.wheel(0, -300);
  await expect.poll(() => transcript.evaluate((root) => root.scrollTop)).toBeLessThan(before);

  // Clicking a row lands on that prompt, even one far above the rendered window.
  const target = outlineRows().nth(1);
  const id = (await target.getAttribute("data-prompt-id"))!;
  await target.click();
  const prompt = page.locator(`[data-item-id="${id}"]`);
  await expect
    .poll(async () => ((await prompt.boundingBox())?.y ?? 9999) - (await transcriptTop()))
    .toBeLessThan(80);
  await expect(prompt).toContainText("outline turn 2 line 1");
  // Jumping away from the end stops following new output.
  await expect(page.getByRole("button", { name: "Jump to latest", exact: true })).toBeVisible();
  await page.mouse.move(900, 500);
  await expect(outline()).toHaveAttribute("data-state", "folded");
  await expect(outlineDashes().nth(1)).toHaveAttribute("aria-current", "location");
  await expect(page.locator(".outline-dash[aria-current]")).toHaveCount(1);
  await page.screenshot({ path: "test-results/outline-rest.png" });
  expect(errors).toEqual([]);
});

test("conversation outline follows the prompt being read as the conversation scrolls", async () => {
  await page.setViewportSize({ width: 1420, height: 940 });
  const prompts = Array.from({ length: 12 }, (_, i) =>
    Array.from({ length: (i % 4) * 10 + 1 }, (_, line) => `reading turn ${i + 1} line ${line}`).join("\n"),
  );
  await outlineSession(prompts);
  await expect(outlineDashes()).toHaveCount(12);
  const transcript = page.locator(".transcript");
  // Scrolling up by hand stops following new output, as a reader does.
  const view = (await transcript.boundingBox())!;
  await page.mouse.move(view.x + view.width / 2, view.y + view.height / 2);
  await page.mouse.wheel(0, -400);
  await expect(page.getByRole("button", { name: "Jump to latest", exact: true })).toBeVisible();
  for (const fraction of [0.15, 0.5, 0.8]) {
    await transcript.evaluate((root, value) => {
      root.scrollTop = (root.scrollHeight - root.clientHeight) * value;
    }, fraction);
    await page.waitForTimeout(300);
    const result = await transcript.evaluate((root) => {
      const line = root.getBoundingClientRect().top + 80;
      const ids = [...document.querySelectorAll<HTMLElement>(".outline-dash")].map((dash) => dash.dataset.promptId!);
      const active = document.querySelector<HTMLElement>('.outline-dash[aria-current="location"]')!.dataset.promptId!;
      const top = (id: string | undefined) =>
        id ? root.querySelector(`[data-item-id="${id}"]`)?.getBoundingClientRect().top : undefined;
      return { own: top(active), next: top(ids[ids.indexOf(active) + 1]), line };
    });
    expect(result.own).toBeDefined();
    expect(result.own!).toBeLessThanOrEqual(result.line + 1);
    if (result.next !== undefined) expect(result.next).toBeGreaterThan(result.line);
  }
  // At the very top the first prompt is being read.
  await transcript.evaluate((root) => (root.scrollTop = 0));
  await expect(outlineDashes().first()).toHaveAttribute("aria-current", "location");
  // Wheel input over the stack scrolls the conversation.
  const dash = (await outlineDashes().nth(2).boundingBox())!;
  await page.mouse.move(dash.x + 4, dash.y);
  await page.mouse.wheel(0, 400);
  await expect.poll(() => transcript.evaluate((root) => root.scrollTop)).toBeGreaterThan(0);
  await page.mouse.move(900, 500);
});

test("conversation outline folds a long session into a short stack and a scrolling panel", async () => {
  test.setTimeout(240000);
  await page.setViewportSize({ width: 1420, height: 940 });
  await outlineSession(Array.from({ length: 60 }, (_, i) => `long session prompt ${i + 1}`));
  // Sixty prompts fold into 24 slices; the slice with the newest prompt is being read.
  await expect(outlineDashes()).toHaveCount(24);
  await expect(outlineDashes().last()).toHaveAttribute("aria-current", "location");
  // Opened, only the rows in view are drawn, and the panel scrolls through the rest.
  await openOutlineOn(12);
  expect(await outlineRows().count()).toBeLessThanOrEqual(26);
  const panel = page.locator(".outline-panel");
  const height = (await panel.boundingBox())!.height;
  expect(height).toBeLessThanOrEqual(16 * 28 + 6 * 2 + 2 + 1);
  const before = await outlineRows().first().getAttribute("data-prompt-id");
  await panel.hover();
  await page.mouse.wheel(0, 400);
  await expect.poll(() => outlineRows().first().getAttribute("data-prompt-id")).not.toBe(before);
  await expect(page.locator('.outline-row[data-state="hot"]')).toContainText("long session prompt");
  await page.mouse.move(900, 500);
  await expect(outline()).toHaveAttribute("data-state", "folded");
});

test("conversation outline keeps text clear of its stack and hides where there is no room", async () => {
  await page.setViewportSize({ width: 1420, height: 940 });
  await outlineSession(["gutter prompt one", "gutter prompt two", "gutter prompt three"]);
  await expect(outlineDashes()).toHaveCount(3);
  const geometry = () =>
    page.evaluate(() => {
      const outline = document.querySelector(".outline");
      const shown = !!outline && getComputedStyle(outline).display !== "none";
      const stack = Math.max(
        ...[...document.querySelectorAll(".outline-mark")].map((mark) => mark.getBoundingClientRect().right),
      );
      const transcript = document.querySelector<HTMLElement>(".transcript")!;
      const box = transcript.getBoundingClientRect();
      const text = box.left + parseFloat(getComputedStyle(transcript).paddingInlineStart);
      const composer = document.querySelector(".composer")!.getBoundingClientRect().left;
      return { shown, gap: text - stack, text, composer, width: box.width };
    });
  for (const width of [1420, 1100, 900]) {
    await page.setViewportSize({ width, height: 940 });
    await page.waitForTimeout(200);
    const value = await geometry();
    expect(value.shown).toBe(true);
    // Text starts at least 12px past the widest dash, and the composer lines up with it.
    expect(value.gap).toBeGreaterThanOrEqual(12);
    expect(Math.abs(value.composer - value.text)).toBeLessThan(1);
    await page.screenshot({ path: `test-results/outline-gutter-${width}.png` });
  }
  // A conversation too narrow for a margin has no room for the stack; its
  // text falls back to the ordinary inset.
  await page.setViewportSize({ width: 700, height: 940 });
  await expect.poll(async () => (await geometry()).width).toBeLessThan(460);
  await expect.poll(async () => (await geometry()).shown).toBe(false);
  expect((await geometry()).text - (await page.locator(".transcript").boundingBox())!.x).toBeLessThanOrEqual(17);
  await page.setViewportSize({ width: 1420, height: 940 });
  await expect.poll(async () => (await geometry()).shown).toBe(true);
  // A single prompt has nowhere to move to, so there is no outline.
  await newSession();
  await send("only one outline prompt");
  await expect(page.locator(".transcript")).toContainText("Echo: only one outline prompt");
  await expect(outline()).toHaveCount(0);
});

test("conversation outline opens nothing past a very short lane and opens again once it grows", async () => {
  await page.setViewportSize({ width: 1420, height: 940 });
  await outlineSession(["short lane one", "short lane two", "short lane three"]);
  await expect(outlineDashes()).toHaveCount(3);
  await openOutlineOn(0);
  await expect(page.locator(".outline-panel")).toBeVisible();
  // A window this short leaves the transcript less room than the panel's smallest size.
  await page.setViewportSize({ width: 1420, height: 230 });
  await expect.poll(() => page.locator(".transcript").evaluate((element) => element.clientHeight)).toBeLessThan(64);
  await expect(outline()).toHaveAttribute("hidden", "");
  await expect(page.locator(".outline-panel")).toHaveCount(0);
  await expect(outlineDashes().first()).toBeHidden();
  await page.screenshot({ path: "test-results/outline-short-lane.png" });
  await page.setViewportSize({ width: 1420, height: 940 });
  await expect(outline()).not.toHaveAttribute("hidden", "");
  await page.mouse.move(700, 600);
  await expect(outline()).toHaveAttribute("data-state", "folded");
  await openOutlineOn(1);
  await expect(page.locator(".outline-panel")).toBeVisible();
  await page.mouse.move(700, 600);
  expect(errors).toEqual([]);
});

test("conversation outline is operable by assistive technology and mirrors in right-to-left languages", async () => {
  await page.setViewportSize({ width: 1420, height: 940 });
  const long = (text: string) =>
    [text, ...Array.from({ length: 40 }, (_, line) => `${text} detail ${line + 1}`)].join("\n");
  await outlineSession([long("accessible outline first"), long("accessible outline second"), long("accessible outline third")]);
  await expect(outlineDashes()).toHaveCount(3);
  await expect(outlineDashes().first()).toHaveAttribute("aria-label", "Jump to prompt 1");
  // Assistive technology activates a dash directly; pointer input goes to the lane.
  await outlineDashes().first().dispatchEvent("click");
  const first = await outlineDashes().first().getAttribute("data-prompt-id");
  await expect
    .poll(async () => ((await page.locator(`[data-item-id="${first}"]`).boundingBox())?.y ?? 9999) - (await transcriptTop()))
    .toBeLessThan(80);
  await send("/language ar");
  await expect(page.locator("html")).toHaveAttribute("dir", "rtl");
  // In Arabic the stack sits at the inline start, the right edge, and the panel opens leftwards.
  const transcript = (await page.locator(".transcript").boundingBox())!;
  const dash = (await outlineDashes().first().boundingBox())!;
  expect(transcript.x + transcript.width - (dash.x + dash.width)).toBeLessThan(40);
  await openOutlineOn(1);
  const panel = (await page.locator(".outline-panel").boundingBox())!;
  expect(panel.x + panel.width).toBeLessThanOrEqual(transcript.x + transcript.width);
  expect(panel.x).toBeLessThan(dash.x);
  // Latin prompts keep their own direction, so they truncate at their end.
  await expect(outlineRows().first().locator(".outline-title")).toHaveAttribute("dir", "auto");
  await page.screenshot({ path: "test-results/outline-rtl.png" });
  await page.mouse.move(400, 500);
  // The send button's name is now Arabic; Enter sends the command either way.
  const input = page.locator("textarea[data-composer-input]");
  await input.fill("/language en");
  await input.press("Enter");
  await expect(page.locator("html")).toHaveAttribute("dir", "ltr");
});

test("deferred input queue edits in the composer, reorders, reconnects and stays paused after Stop", async () => {
  await newSession();
  await send('!tool bash {"command":"printf original-queue-proof"}');
  await expect.poll(async () => (await client.listPermissions()).length).toBeGreaterThan(0);
  const permission = (await client.listPermissions())[0];
  const id = permission.threadId;
  const composer = page.getByRole("textbox", { name: "Message", exact: true });
  const queue = page.getByRole("region", { name: "Queued messages", exact: true });
  const row = (text: string) => queue.getByRole("listitem").filter({ hasText: text });
  // While work runs, the send button says what sending does.
  await composer.fill("documentation later");
  await expect(page.locator(".send-button")).toHaveAttribute("title", "Steer message");
  await composer.press("Alt+Enter");
  await expect(row("documentation later")).toBeVisible();
  await expect(queue.locator("header")).toContainText("Queued");
  await composer.fill("release notes later");
  await composer.press("Alt+Enter");
  await expect.poll(async () => (await client.inputQueue(id)).items.length).toBe(2);
  await expect(queue.locator("header")).toContainText("2");
  expect((await client.listItems(id)).some((item) => item.data.type === "user_message" && item.data.text === "documentation later")).toBe(false);
  // Editing loads the message into the composer and sets the draft aside.
  await composer.fill("preserve my unsent draft");
  await row("documentation later").getByRole("button", { name: "Edit queued message", exact: true }).click();
  await expect(composer).toHaveValue("documentation later");
  await expect(queue.locator("header")).toContainText("Editing — sending saves it in place");
  await expect(page.getByRole("button", { name: "Save queued message", exact: true })).toBeVisible();
  // Esc leaves the edit and never stops the running work.
  await composer.press("Escape");
  await expect(composer).toHaveValue("preserve my unsent draft");
  expect(await client.activeTurn(id)).not.toBeNull();
  await row("documentation later").getByRole("button", { name: "Edit queued message", exact: true }).click();
  await composer.fill("edited documentation");
  await composer.press("Enter");
  await expect(row("edited documentation")).toBeVisible();
  await expect(composer).toHaveValue("preserve my unsent draft");
  expect((await client.inputQueue(id)).items.map((item) => item.text)).toEqual(["edited documentation", "release notes later"]);
  await row("release notes later").hover();
  await row("release notes later").getByRole("button", { name: "Move queued message up", exact: true }).click();
  await expect.poll(async () => (await client.inputQueue(id)).items[0].text).toBe("release notes later");
  await page.reload();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
  await expect(row("release notes later")).toBeVisible();
  await expect(row("edited documentation")).toBeVisible();
  await expect(composer).toHaveValue("preserve my unsent draft");
  // Another client changing the same message stops the save; it never overwrites.
  await row("release notes later").getByRole("button", { name: "Edit queued message", exact: true }).click();
  await composer.fill("my conflicting edit");
  const snapshot = await client.inputQueue(id);
  await client.changeInputQueue(id, { action: "edit", revision: snapshot.revision, id: snapshot.items[0].id, text: "changed by another client" });
  await composer.press("Enter");
  await expect(queue.getByRole("alert")).toContainText("changed somewhere else");
  await expect(composer).toHaveValue("my conflicting edit");
  expect((await client.inputQueue(id)).items[0].text).toBe("changed by another client");
  const changed = await client.inputQueue(id);
  await client.changeInputQueue(id, { action: "remove", revision: changed.revision, id: changed.items[0].id });
  await composer.press("Enter");
  await expect(queue.getByRole("alert")).toContainText("already sent or removed");
  await expect(composer).toHaveValue("my conflicting edit");
  await queue.getByRole("button", { name: "Cancel editing", exact: true }).click();
  await expect(composer).toHaveValue("preserve my unsent draft");
  await composer.fill("remove through the UI");
  await composer.press("Alt+Enter");
  await expect(row("remove through the UI")).toBeVisible();
  await row("remove through the UI").getByRole("button", { name: "Remove queued message", exact: true }).click();
  await expect.poll(async () => (await client.inputQueue(id)).items.length).toBe(1);
  await composer.fill("preserve my unsent draft");
  await client.interruptThreadTree(id);
  await expect.poll(async () => (await client.activeTurn(id))).toBeNull();
  await expect(queue.locator("header")).toContainText("Queue paused — send when ready");
  await expect(page.getByRole("button", { name: "Allow once", exact: true })).toHaveCount(0);
  await page.screenshot({ path: "test-results/deferred-input-queue.png" });
  await row("edited documentation").getByRole("button", { name: "Send now", exact: true }).click();
  await expect.poll(async () => (await client.inputQueue(id)).items.length).toBe(0);
  await expect.poll(async () => (await client.listItems(id)).some((item) => item.data.type === "assistant_message" && item.data.text.includes("edited documentation"))).toBe(true);
  await expect(queue).toHaveCount(0);
  await expect(composer).toHaveValue("preserve my unsent draft");
  await composer.fill("");
});

test("deferred input queue Steer now promotes only the selected prompt, and each queued prompt is its own turn", async () => {
  await newSession();
  await send('!tool bash {"command":"printf queue-steering-proof"}');
  await expect.poll(async () => (await client.listPermissions()).length).toBeGreaterThan(0);
  const permission = (await client.listPermissions())[0];
  const id = permission.threadId;
  const composer = page.getByRole("textbox", { name: "Message", exact: true });
  const queue = page.getByRole("region", { name: "Queued messages", exact: true });
  for (const text of ["keep this for later", "change the active work now", "and this one after"]) {
    await composer.fill(text);
    await composer.press("Alt+Enter");
    await expect(queue).toContainText(text);
  }
  await queue.getByRole("listitem").filter({ hasText: "change the active work now" }).getByRole("button", { name: "Steer now", exact: true }).click();
  await expect.poll(async () => (await client.inputQueue(id)).items.map((item) => item.text)).toEqual(["keep this for later", "and this one after"]);
  expect((await client.listItems(id)).some((item) => item.data.type === "user_message" && item.data.text === "change the active work now")).toBe(true);
  expect((await client.listItems(id)).some((item) => item.data.type === "user_message" && item.data.text === "keep this for later")).toBe(false);
  const first = (await client.activeTurn(id))!;
  await client.replyPermission(permission.permissionId, { decision: "once" });
  await expect.poll(async () => (await client.inputQueue(id)).items.length).toBe(0);
  await expect.poll(async () => (await client.activeTurn(id))).toBeNull();
  const items = await client.listItems(id);
  const users = items.filter((item) => item.data.type === "user_message");
  const messages = users.map((item) => item.data.type === "user_message" ? item.data.text : "");
  expect(messages.slice(-3)).toEqual(["change the active work now", "keep this for later", "and this one after"]);
  // Steering joined the running turn; each queued prompt started its own.
  const turns = users.slice(-3).map((item) => item.turnId);
  expect(turns[0]).toBe(first.id);
  expect(new Set(turns).size).toBe(3);
  await expect(queue).toHaveCount(0);
});

test("Stop answers steering the agent has not read yet and leaves the queue paused", async () => {
  await newSession();
  await send('!tool bash {"command":"printf stop-steering-proof"}');
  await expect.poll(async () => (await client.listPermissions()).length).toBeGreaterThan(0);
  const id = (await client.listPermissions())[0].threadId;
  const composer = page.getByRole("textbox", { name: "Message", exact: true });
  const queue = page.getByRole("region", { name: "Queued messages", exact: true });
  await composer.fill("queued until I say so");
  await composer.press("Alt+Enter");
  await expect(queue).toContainText("queued until I say so");
  await send("actually, just explain the plan");
  await expect.poll(async () => (await client.listItems(id)).some((item) => item.data.type === "user_message" && item.data.text === "actually, just explain the plan")).toBe(true);
  await page.getByRole("button", { name: "Stop response", exact: true }).click();
  // A continuation answers the unread steering instead of dropping it.
  await expect.poll(async () => (await client.listItems(id)).some((item) => item.data.type === "assistant_message" && item.data.text.includes("stopped your previous work"))).toBe(true);
  await expect.poll(async () => (await client.activeTurn(id))).toBeNull();
  const items = await client.listItems(id);
  expect(items.some((item) => item.data.type === "error" && item.data.message.includes("Queued input could not be delivered"))).toBe(false);
  // The continuation does not drain the queue Stop paused.
  await page.waitForTimeout(800);
  expect((await client.inputQueue(id)).items.map((item) => item.text)).toEqual(["queued until I say so"]);
  await expect(queue.locator("header")).toContainText("Queue paused — send when ready");
  // A new message runs first, then the paused queue continues.
  await send("something new first");
  await expect.poll(async () => (await client.inputQueue(id)).items.length).toBe(0);
  await expect.poll(async () => (await client.listItems(id)).some((item) => item.data.type === "assistant_message" && item.data.text.includes("queued until I say so"))).toBe(true);
  await expect.poll(async () => (await client.activeTurn(id))).toBeNull();
  const order = (await client.listItems(id)).filter((item) => item.data.type === "user_message" && !item.data.synthetic).map((item) => item.data.type === "user_message" ? item.data.text : "");
  expect(order.slice(-2)).toEqual(["something new first", "queued until I say so"]);
  await page.screenshot({ path: "test-results/stop-answers-steering.png" });
});

test("shared input settings apply project and global modes to real Desktop chat", async () => {
  const settingsApi = async (input: Record<string, string>) => {
    const result = await client.managementView("input-settings", input);
    return result.data as { delivery: string; scopeDelivery: string | null };
  };
  const globalFile = join(temporary, "state", "config", "haarness.jsonc");
  const projectFile = join(workspace, ".haarness", "haarness.jsonc");
  await writeFile(globalFile, '{\n// global input fixture\n"input":{"delivery":"queue"},"unrelated":7\n}\n');
  await writeFile(projectFile, '{\n// project input fixture\n"input":{"other":true},"unrelated":8\n}\n');
  const originalGlobal = await readFile(globalFile, "utf8");
  try {
    await newSession();
    const composer = page.getByRole("textbox", { name: "Message", exact: true });
    await expect(page.getByRole("button", { name: "Queue message", exact: true })).toHaveCount(0);
    expect((await settingsApi({ action: "get" })).delivery).toBe("queue");
    // Autocomplete advertises the dedicated command before a conversation exists.
    await composer.fill("/input-m");
    await expect(page.locator("#slash-command-0")).toContainText("input-mode");
    await send("/input-mode");
    await expect.poll(async () => (await settingsApi({ action: "get" })).delivery).toBe("steer");
    expect(await readFile(globalFile, "utf8")).toBe(originalGlobal);
    await send("/settings");
    const settings = page.getByRole("dialog", { name: "Settings", exact: true });
    await expect(settings).toBeVisible();
    expect((await settingsApi({ action: "get" })).delivery).toBe("steer"); // /settings only opens the pane.
    const row = settings.locator('[data-setting="Messages sent while working"]');
    const steer = row.getByRole("radio", { name: "Steer", exact: true });
    const queue = row.getByRole("radio", { name: "Queue", exact: true });
    // /input-mode saved this project's own value; the caption says so and
    // names the value its reset falls back to.
    const caption = row.locator(".input-settings-scope");
    await expect(steer).toHaveAttribute("aria-checked", "true");
    await expect(caption).toContainText("This project");
    await expect(caption.getByRole("button", { name: "Use global setting (Queue)", exact: true })).toBeVisible();
    await queue.click();
    await expect.poll(async () => (await settingsApi({ action: "get" })).scopeDelivery).toBe("queue");
    expect(await readFile(globalFile, "utf8")).toBe(originalGlobal);
    // Use global setting removes the project's value; the global one shows.
    await row.getByRole("button", { name: "Use global setting (Queue)", exact: true }).click();
    await expect(queue).toHaveAttribute("aria-checked", "true");
    await expect(caption).toContainText("All projects");
    await expect.poll(async () => (await settingsApi({ action: "get" })).scopeDelivery).toBeNull();
    await expect(row).not.toContainText("This project");
    // Without a project value, the choice is the global setting's.
    await steer.click();
    await expect.poll(async () => (await settingsApi({ action: "get", scope: "global" })).scopeDelivery).toBe("steer");
    expect((await settingsApi({ action: "get" })).scopeDelivery).toBeNull();
    await expect(steer).toHaveAttribute("aria-checked", "true");
    // Override for this project starts the project's own value from the one shown.
    await row.getByRole("button", { name: "Override for this project", exact: true }).click();
    await expect.poll(async () => (await settingsApi({ action: "get" })).scopeDelivery).toBe("steer");
    await expect(row).toContainText("This project");
    // Arrow keys move the choice, which saves at once.
    await steer.focus();
    await page.keyboard.press("ArrowRight");
    await expect(queue).toBeFocused();
    await expect.poll(async () => (await settingsApi({ action: "get" })).scopeDelivery).toBe("queue");
    expect((await settingsApi({ action: "get", scope: "global" })).scopeDelivery).toBe("steer");
    await row.getByRole("button", { name: "Use global setting (Steer)", exact: true }).click();
    await expect.poll(async () => (await settingsApi({ action: "get" })).scopeDelivery).toBeNull();
    expect((await settingsApi({ action: "get" })).delivery).toBe("steer");
    // A reset and a choice right after it change the global setting, never
    // bring the project's value back.
    await row.getByRole("button", { name: "Override for this project", exact: true }).click();
    await row.getByRole("button", { name: "Use global setting (Steer)", exact: true }).click();
    await queue.click();
    await expect.poll(async () => (await settingsApi({ action: "get", scope: "global" })).scopeDelivery).toBe("queue");
    expect((await settingsApi({ action: "get" })).scopeDelivery).toBeNull();
    await expect(caption).toContainText("All projects");
    await steer.click();
    await expect.poll(async () => (await settingsApi({ action: "get", scope: "global" })).scopeDelivery).toBe("steer");
    // An external client changes the same project while General stays open.
    await settingsApi({ action: "set", delivery: "queue" });
    await expect(queue).toHaveAttribute("aria-checked", "true");
    await expect(row).toContainText("This project");
    await page.screenshot({ path: "test-results/input-settings-general.png" });
    await settings.getByRole("button", { name: "Close settings", exact: true }).click();
    await send("/input-mode reset");
    await expect.poll(async () => (await settingsApi({ action: "get" })).delivery).toBe("steer");
    await send('!tool bash {"command":"printf desktop-input-settings-tool"}');
    await expect.poll(async () => (await client.listPermissions()).length).toBeGreaterThan(0);
    const permission = (await client.listPermissions())[0];
    const tid = permission.threadId;
    await send("/input-mode queue");
    await expect.poll(async () => (await settingsApi({ action: "get" })).delivery).toBe("queue");
    await composer.fill("normal Enter waits for current work");
    await composer.press("Enter");
    await expect.poll(async () => (await client.inputQueue(tid)).items.length).toBe(1);
    expect((await client.listItems(tid)).some(i => i.data.type === "user_message" && i.data.text === "normal Enter waits for current work")).toBe(false);
    await send("/input-mode steer");
    await expect.poll(async () => (await settingsApi({ action: "get" })).delivery).toBe("steer");
    expect((await client.inputQueue(tid)).items.length).toBe(1);
    await send("normal Send steers current work");
    await expect.poll(async () => (await client.listItems(tid)).some(i => i.data.type === "user_message" && i.data.text === "normal Send steers current work")).toBe(true);
    await page.screenshot({ path: "test-results/input-settings-active-chat.png" });
    await client.replyPermission(permission.permissionId, { decision: "once" });
    await expect.poll(async () => client.activeTurn(tid)).toBeNull();
    await expect.poll(async () => (await client.inputQueue(tid)).items.length).toBe(0);
    await expect.poll(async () => (await client.listItems(tid)).some(i => i.data.type === "assistant_message" && i.data.text.includes("normal Enter waits for current work"))).toBe(true);
    await send("/input-mode queue");
    await expect.poll(async () => (await settingsApi({ action: "get" })).delivery).toBe("queue");
    await page.reload();
    await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
    await send("idle Queue starts chat immediately after reload");
    await expect.poll(async () => (await client.listItems(tid)).some(i => i.data.type === "assistant_message" && i.data.text.includes("idle Queue starts chat immediately after reload"))).toBe(true);
    expect(await readFile(projectFile, "utf8")).toContain("// project input fixture");
    expect(await readFile(globalFile, "utf8")).toContain("// global input fixture");
    expect(errors).toEqual([]);
  } finally {
    await settingsApi({ action: "reset" });
    await settingsApi({ action: "reset", scope: "global" });
    await rm(projectFile, { force: true });
    await rm(globalFile, { force: true });
  }
});

test("display languages default to English and switch persistently across all shared locales", async () => {
  const { languages, translate } = await import("../src/localization");
  await expect(page.locator("html")).toHaveAttribute("lang", "en");
  await newSession();
  await send("/language");
  const settings = page.locator(".settings");
  const language = settings.locator('[data-setting="Language"] [role="combobox"]');
  await expect(language).toContainText("English");
  for (const { code, label, direction } of languages) {
    await selectValue(language, code);
    await expect(page.locator("html")).toHaveAttribute("lang", code);
    await expect(page.locator("html")).toHaveAttribute("dir", direction);
    await expect(language).toContainText(label);
    await expect(settings.locator("h2")).toContainText(translate(code, "Settings"));
    await expect(settings.locator("h1")).toHaveText(translate(code, "General"));
    const menuLabels = await app.evaluate(({ Menu }) => Menu.getApplicationMenu()?.items.map((item) => item.label));
    expect(menuLabels).toContain(translate(code, "File"));
    expect(JSON.parse(await readFile(join(temporary, "desktop", "desktop.json"), "utf8")).preferences.language).toBe(code);
    if (["ja", "ar", "zh-TW"].includes(code))
      await page.screenshot({ path: `test-results/language-${code}.png` });
  }
  await selectValue(language, "ja");
  await page.reload();
  await expect(page.locator("html")).toHaveAttribute("lang", "ja");
  // The command itself stays stable in every interface language.
  if (await settings.isVisible()) await settings.locator("header button").last().click();
  const composer = () => page.getByRole("textbox", { name: translate("ja", "Message"), exact: true });
  await composer().fill("/language invalid");
  await composer().press("Enter");
  await expect(page.locator("html")).toHaveAttribute("lang", "ja");
  await composer().fill("/language en");
  await composer().press("Enter");
  await expect(page.locator("html")).toHaveAttribute("lang", "en");
  await send("language-chat-proof");
  await expect(page.locator(".message.assistant").last()).toContainText("language-chat-proof");
  await send("/language zh-Hant");
  await expect(page.locator("html")).toHaveAttribute("lang", "zh-TW");
  await page.getByRole("textbox", { name: translate("zh-TW", "Message"), exact: true }).fill("/language en");
  await page.getByRole("textbox", { name: translate("zh-TW", "Message"), exact: true }).press("Enter");
  await expect(page.locator("html")).toHaveAttribute("lang", "en");
});

test("the window opens in its saved theme, palette, text size and language, never the defaults first", async () => {
  const before = await page.evaluate(() => window.haarness.boot().then((value) => value.preferences));
  await page.evaluate(() => window.haarness.preferences({ theme: "light", palette: "ocean", fontSize: 18, language: "ar" }));
  await expect(page.locator("html")).toHaveAttribute("dir", "rtl");
  // The window itself paints the saved theme's page colour before its page draws anything.
  const { themeColors } = await import("../src/themes");
  expect((await app.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows()[0].getBackgroundColor())).toLowerCase())
    .toBe(themeColors("ocean", "light").page);
  // From this page's next document on (and only this one), every look the document has once the app has drawn anything.
  const cdp = await page.context().newCDPSession(page);
  await cdp.send("Page.enable");
  const { identifier } = await cdp.send("Page.addScriptToEvaluateOnNewDocument", {
    source: `(() => {
      const seen = [];
      window.appearance = seen;
      // The document has no root element yet when this runs.
      const observer = new MutationObserver(() => {
        const root = document.documentElement;
        if (!document.getElementById("root")?.childElementCount) return;
        const look = [root.lang, root.dir, root.dataset.theme, root.dataset.palette, root.style.getPropertyValue("--font-size")].join(" ");
        if (seen.at(-1) !== look) seen.push(look);
      });
      observer.observe(document, { attributes: true, childList: true, subtree: true });
      setTimeout(() => observer.disconnect(), 5000);
    })()`,
  });
  try {
    await page.reload();
    await expect(page.locator(".application")).toBeVisible();
    await expect.poll(() => page.evaluate(() => (window as unknown as { appearance: string[] }).appearance.length)).toBeGreaterThan(0);
    await page.waitForTimeout(1000);
    expect(await page.evaluate(() => (window as unknown as { appearance: string[] }).appearance)).toEqual(["ar rtl light ocean 18px"]);
  } finally {
    await cdp.send("Page.removeScriptToEvaluateOnNewDocument", { identifier });
    await cdp.detach();
  }

  // Before the app applies the theme, the page lets that window colour show, not its stylesheet's dark defaults.
  const empty = await page.evaluate(() => {
    const root = document.documentElement;
    const saved = { theme: root.dataset.theme, palette: root.dataset.palette, style: root.getAttribute("style") };
    delete root.dataset.theme;
    delete root.dataset.palette;
    root.removeAttribute("style");
    (document.getElementById("root") as HTMLElement).style.visibility = "hidden";
    const look = { body: getComputedStyle(document.body).backgroundColor, scheme: getComputedStyle(root).colorScheme };
    (window as unknown as { restore: () => void }).restore = () => {
      root.dataset.theme = saved.theme!;
      root.dataset.palette = saved.palette!;
      root.setAttribute("style", saved.style ?? "");
      (document.getElementById("root") as HTMLElement).style.visibility = "";
    };
    return look;
  });
  expect(empty).toEqual({ body: "rgba(0, 0, 0, 0)", scheme: "normal" });
  await page.screenshot({ path: test.info().outputPath("first-frame-light.png") });
  await page.evaluate(() => (window as unknown as { restore: () => void }).restore());

  // A main process busy elsewhere (a native dialog in another window) does not leave the window empty.
  await app.evaluate(({ ipcMain }) => {
    const handlers = (ipcMain as unknown as { _invokeHandlers: Map<string, (...args: unknown[]) => unknown> })._invokeHandlers;
    const original = handlers.get("haarness:boot")!;
    (globalThis as unknown as { bootHandler: unknown }).bootHandler = original;
    handlers.set("haarness:boot", async (...args: unknown[]) => {
      await new Promise((resolve) => setTimeout(resolve, 2500));
      return original(...args);
    });
  });
  try {
    await page.reload({ waitUntil: "domcontentloaded" });
    await expect(page.locator(".application")).toBeVisible({ timeout: 1500 });
  } finally {
    await app.evaluate(({ ipcMain }) => {
      const handlers = (ipcMain as unknown as { _invokeHandlers: Map<string, unknown> })._invokeHandlers;
      handlers.set("haarness:boot", (globalThis as unknown as { bootHandler: unknown }).bootHandler);
    });
  }
  await expect(page.locator("html")).toHaveAttribute("lang", "ar");
  await page.evaluate(
    (value) => window.haarness.preferences({ theme: value.theme, palette: value.palette, fontSize: value.fontSize, language: value.language }),
    before,
  );
  await expect(page.locator("html")).toHaveAttribute("lang", before.language);
});

test("incremental Markdown retains live selection and completed blocks", async () => {
  await newSession();
  await send("::mdslow");
  const live = page.getByTestId("streaming-reply");
  await expect(live.locator("strong")).toHaveText("Live markdown");
  await live.locator("strong").evaluate((element) => {
    const range = document.createRange();
    range.selectNodeContents(element);
    window.getSelection()!.removeAllRanges();
    window.getSelection()!.addRange(range);
    // Read as the next block lands, in the page: the turn ends half a second
    // later and its saved reply replaces the live one, which a check made
    // from the test afterwards could see on a loaded machine.
    const reply = element.closest('[data-testid="streaming-reply"]')!;
    const read = () => ({
      landedAfterSelection: true,
      sameNode: reply.querySelector("strong") === element,
      selected: window.getSelection()?.toString(),
    });
    (window as any).__markdownEvidence = new Promise((resolve) => {
      if (reply.querySelector("li")) return resolve({ ...read(), landedAfterSelection: false });
      const observer = new MutationObserver(() => {
        if (reply.querySelector("li")?.textContent !== "streams while working") return;
        observer.disconnect();
        resolve(read());
      });
      observer.observe(reply, { subtree: true, childList: true, characterData: true });
    });
  });
  // The list lives in the live reply for half a second at most: wait for the
  // page's own reading of it rather than for a poll to catch it there.
  const evidence = await page.evaluate(() => (window as any).__markdownEvidence);
  await test.info().attach("live-selection", { body: JSON.stringify(evidence), contentType: "application/json" });
  expect(evidence.landedAfterSelection).toBe(true);
  if (process.env.HAARNESS_MARKDOWN_BASELINE !== "1") {
    expect(evidence.sameNode).toBe(true);
    expect(evidence.selected).toBe("Live markdown");
  }
  await expect(live).toHaveCount(0);
  await expect(page.locator(".transcript article.assistant").last().locator("strong")).toHaveText("Live markdown");
  expect(errors).toEqual([]);
});

test("text selected in a streaming reply stays selected, backward as made, when the saved reply takes its place, and copies", async () => {
  await newSession();
  await send("::mdslow");
  const live = page.getByTestId("streaming-reply");
  await expect(live.locator("strong")).toHaveText("Live markdown");
  // "markdown", dragged from its end back to its start.
  const before = await live.locator("strong").evaluate((element) => {
    const text = element.firstChild as Text;
    window.getSelection()!.setBaseAndExtent(text, 13, text, 5);
    return document.activeElement?.outerHTML.slice(0, 80);
  });
  await app.evaluate(({ clipboard }) => clipboard.writeText("clipboard before copy"));
  await expect(live).toHaveCount(0);
  const saved = page.locator(".transcript article.assistant").last();
  await expect(saved.locator("li")).toHaveText("streams while working");
  await expect
    .poll(() =>
      saved.evaluate((element) => {
        const selection = window.getSelection()!;
        return {
          text: selection.toString(),
          inSaved: element.contains(selection.anchorNode) && element.contains(selection.focusNode),
          anchor: selection.anchorOffset,
          focus: selection.focusOffset,
          active: document.activeElement?.outerHTML.slice(0, 80),
        };
      }),
    )
    .toEqual({ text: "markdown", inSaved: true, anchor: 13, focus: 5, active: before });
  await page.keyboard.press(process.platform === "darwin" ? "Meta+C" : "Control+C");
  await expect
    .poll(() => app.evaluate(({ clipboard }) => clipboard.readText()))
    .toBe("markdown");
  expect(errors).toEqual([]);
});

test("incremental Markdown streams a long real provider response, resolves references, and survives reload", async () => {
  let output: import("node:http").ServerResponse | undefined;
  const provider = createServer(async (request, response) => {
    if (request.method === "GET") {
      response.setHeader("Content-Type", "application/json");
      response.end(JSON.stringify({ data: [{ id: "markdown-model" }] }));
      return;
    }
    const chunks: Buffer[] = [];
    for await (const chunk of request) chunks.push(Buffer.from(chunk));
    const body = JSON.parse(Buffer.concat(chunks).toString());
    response.writeHead(200, { "Content-Type": "text/event-stream" });
    if (JSON.stringify(body.messages).includes("Name this conversation.")) {
      response.end('data: {"choices":[{"index":0,"delta":{"content":"Markdown streaming fixture"},"finish_reason":null}]}\n\ndata: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}\n\ndata: [DONE]\n\n');
      return;
    }
    output = response;
  });
  await new Promise<void>(resolve => provider.listen(0, "127.0.0.1", resolve));
  const address = provider.address();
  if (!address || typeof address === "string") throw Error("Missing provider address");
  const directory = join(temporary, "markdown-project");
  extraWorkspaces.push(directory);
  await mkdir(join(directory, ".haarness"), { recursive: true });
  await writeFile(join(directory, ".haarness/config.toml"), `[defaults]\nprovider="markdown-fixture"\nmodel="markdown-model"\n[providers.markdown-fixture]\nprotocol="openai-chat-completions"\nbase_url="http://127.0.0.1:${address.port}/v1"\nrequires_auth=false\ntools=false\n`);
  const delta = (text: string) => output!.write("data: " + JSON.stringify({ choices: [{ index: 0, delta: { content: text }, finish_reason: null }] }) + "\n\n");
  try {
    await app.evaluate(({ dialog }, directory) => {
      const original = dialog.showOpenDialog;
      dialog.showOpenDialog = async () => { dialog.showOpenDialog = original; return { canceled: false, filePaths: [directory] }; };
    }, directory);
    await page.getByRole("button", { name: "Open another project", exact: true }).click();
    await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
    await send("Exercise incremental Markdown rendering.");
    await expect.poll(() => !!output).toBe(true);
    const initial = "# Stable heading\n\nSelected **stable text** stays in place.\n\n" +
      Array.from({ length: 500 }, (_, i) => `Paragraph ${i}: **completed prose** and inline \`code\` remain unchanged while later blocks stream.\n\n`).join("") +
      "## Streaming tail\n\nTail start";
    delta(initial);
    const live = page.getByTestId("streaming-reply");
    await expect(live.locator("h1")).toHaveText("Stable heading");
    await expect(live.locator("p").last()).toHaveText("Tail start");
    if (process.env.HAARNESS_MARKDOWN_BASELINE !== "1")
      expect(page.workers().some(worker => worker.url().includes("markdown.worker"))).toBe(true);
    await live.evaluate((element) => {
      const first = element.querySelector("strong")!;
      const range = document.createRange(); range.selectNodeContents(first);
      window.getSelection()!.removeAllRanges(); window.getSelection()!.addRange(range);
      const probe = { first, removed: 0, tasks: [] as number[], frames: [] as number[], last: performance.now(), frame: 0, observer: undefined as MutationObserver | undefined, performance: undefined as PerformanceObserver | undefined };
      probe.observer = new MutationObserver(records => { for (const record of records) probe.removed += record.removedNodes.length; });
      probe.observer.observe(element, { subtree: true, childList: true });
      probe.performance = new PerformanceObserver(list => probe.tasks.push(...list.getEntries().map(entry => entry.duration)));
      probe.performance.observe({ type: "longtask", buffered: false });
      const tick = (now: number) => { probe.frames.push(now - probe.last); probe.last = now; probe.frame = requestAnimationFrame(tick); };
      probe.frame = requestAnimationFrame(tick);
      (window as any).__markdownProbe = probe;
    });
    for (let index = 0; index < 40; index++) {
      delta(` step-${index}`);
      await expect(live.locator("p").last()).toContainText(`step-${index}`);
    }
    const evidence = await live.evaluate((element) => {
      const probe = (window as any).__markdownProbe;
      probe.observer.disconnect(); probe.performance.disconnect(); cancelAnimationFrame(probe.frame);
      return { sameNode: probe.first === element.querySelector("strong"), selected: window.getSelection()?.toString(), removedNodes: probe.removed, longTasks: probe.tasks, frames: probe.frames.length, maxFrameMs: Math.max(...probe.frames) };
    });
    console.log("Markdown streaming evidence:", JSON.stringify(evidence));
    await test.info().attach("markdown-streaming-metrics", { body: JSON.stringify(evidence), contentType: "application/json" });
    if (process.env.HAARNESS_MARKDOWN_BASELINE !== "1") {
      expect(evidence.sameNode).toBe(true);
      expect(evidence.selected).toBe("stable text");
      expect(evidence.removedNodes).toBe(0);
    }
    delta("\n\n[late reference][target]\n\n```typescript\nconst safe = '<tag>';\n");
    await expect(live.locator("pre code")).toContainText("const safe");
    await expect(live.getByRole("button", { name: "Copy code" })).toHaveCount(0);
    delta("```\n\n| Left | Right |\n| --- | --- |\n| one | two |\n\n[target]: https://example.test/resolved\n\n<img src=x onerror=\"window.badMarkdown=true\"><script>window.badMarkdown=true</script>\n\nEND_MARKDOWN_PROOF");
    await expect(live.locator('a[href="https://example.test/resolved"]')).toHaveText("late reference");
    await expect(live.locator("table td")).toHaveCount(2);
    await expect(live.locator("script, [onerror]")).toHaveCount(0);
    expect(await page.evaluate(() => (window as any).badMarkdown)).toBeUndefined();
    await page.screenshot({ path: test.info().outputPath("markdown-streaming.png") });
    output!.end('data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}\n\ndata: [DONE]\n\n');
    await expect(live).toHaveCount(0);
    const answer = page.locator(".transcript article.assistant").last();
    await expect(answer).toContainText("END_MARKDOWN_PROOF");
    const copy = answer.getByRole("button", { name: "Copy code", exact: true });
    await copy.click();
    await expect.poll(() => app.evaluate(async ({ clipboard }) => (await clipboard.readText()) === "const safe = '<tag>';\n")).toBe(true);
    await page.reload();
    await expect(answer).toContainText("END_MARKDOWN_PROOF");
    await expect(answer.locator("table td")).toHaveCount(2);
    await expect(answer.getByRole("button", { name: "Copy code", exact: true })).toHaveCount(1);
    await page.screenshot({ path: test.info().outputPath("markdown-completed.png") });
    expect(errors).toEqual([]);
  } finally {
    output?.end();
    provider.closeAllConnections();
    await new Promise<void>(resolve => provider.close(() => resolve()));
  }
});

test("incremental Markdown recovers from worker failure without blanking the answer", async () => {
  let output: import("node:http").ServerResponse | undefined;
  const provider = createServer(async (request, response) => {
    if (request.method === "GET") {
      response.setHeader("Content-Type", "application/json");
      response.end(JSON.stringify({ data: [{ id: "fallback-model" }] }));
      return;
    }
    const chunks: Buffer[] = [];
    for await (const chunk of request) chunks.push(Buffer.from(chunk));
    const body = JSON.parse(Buffer.concat(chunks).toString());
    response.writeHead(200, { "Content-Type": "text/event-stream" });
    if (JSON.stringify(body.messages).includes("Name this conversation.")) {
      response.end('data: {"choices":[{"index":0,"delta":{"content":"Markdown fallback fixture"},"finish_reason":null}]}\n\ndata: [DONE]\n\n');
      return;
    }
    output = response;
  });
  await new Promise<void>(resolve => provider.listen(0, "127.0.0.1", resolve));
  const address = provider.address();
  if (!address || typeof address === "string") throw Error("Missing provider address");
  const directory = join(temporary, "markdown-fallback-project");
  extraWorkspaces.push(directory);
  await mkdir(join(directory, ".haarness"), { recursive: true });
  await writeFile(join(directory, ".haarness/config.toml"), `[defaults]
provider="markdown-fallback"
model="fallback-model"
[providers.markdown-fallback]
protocol="openai-chat-completions"
base_url="http://127.0.0.1:${address.port}/v1"
requires_auth=false
tools=false
`);
  const delta = (text: string) => output!.write("data: " + JSON.stringify({ choices: [{ index: 0, delta: { content: text }, finish_reason: null }] }) + "\n\n");
  try {
    // A fresh renderer resets the worker manager. Hold the real provider stream
    // until each assertion completes, rather than racing a 500 ms live state.
    await page.reload();
    await app.evaluate(({ dialog }, directory) => {
      const original = dialog.showOpenDialog;
      dialog.showOpenDialog = async () => { dialog.showOpenDialog = original; return { canceled: false, filePaths: [directory] }; };
    }, directory);
    await page.getByRole("button", { name: "Open another project", exact: true }).click();
    await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
    await newSession();
    await page.evaluate(() => {
      const original = Worker.prototype.postMessage;
      (window as any).__workerFailureInjected = false;
      Worker.prototype.postMessage = function() {
        Worker.prototype.postMessage = original;
        (window as any).__workerFailureInjected = true;
        throw new Error("Fixture worker transport failure");
      };
    });
    await send("Exercise Markdown recovery.");
    await expect.poll(() => !!output).toBe(true);
    delta("**Live markdown**");
    const live = page.getByTestId("streaming-reply");
    await expect(live.locator("strong")).toHaveText("Live markdown");
    expect(await page.evaluate(() => (window as any).__workerFailureInjected)).toBe(true);
    delta("\n\n- streams while working");
    await expect(live.locator("li")).toHaveText("streams while working");
    output!.end('data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}\n\ndata: [DONE]\n\n');
    await expect(live).toHaveCount(0);
    const answer = page.locator(".transcript article.assistant").last();
    await expect(answer.locator("strong")).toHaveText("Live markdown");
    await expect(answer.locator("li")).toHaveText("streams while working");
    expect(errors).toEqual([]);
  } finally {
    output?.end();
    provider.closeAllConnections();
    await new Promise<void>(resolve => provider.close(() => resolve()));
  }
});

async function newCanvasSession() {
  // Other scenarios can leave a different provider/project selected.
  await app.evaluate(({ dialog }, directory) => {
    const original = dialog.showOpenDialog;
    dialog.showOpenDialog = async () => { dialog.showOpenDialog = original; return { canceled: false, filePaths: [directory] }; };
  }, workspace);
  await page.getByRole("button", { name: "Open another project", exact: true }).click();
  await expect(page.locator(".titlebar")).toContainText("workspace");
  await showChat();
  await newSession();
}

test("canvas preview runs isolated scripts, refreshes versions, compares and restores through real tool events", async () => {
  await newCanvasSession();
  const first = '<!doctype html><html><body><h1>Canvas first</h1><button onclick="document.getElementById(\'count\').textContent=\'clicked\'">Increment</button><p id="count">ready</p><script>document.body.dataset.executed="yes"</script></body></html>';
  const second = first.replace('Canvas first', 'Canvas second');
  await send('!tool write ' + JSON.stringify({ path: 'canvas.html', content: first }));
  await expect.poll(() => readFile(join(workspace, 'canvas.html'), 'utf8').catch(() => '')).toBe(first);
  await settled();
  await send('!tool preview {"filePath":"canvas.html","title":"Canvas demo"}');
  const banner = page.locator('.preview-banner').last();
  await expect(banner).toContainText('Canvas demo');
  await expect(banner).toContainText('Current');
  const preview = page.locator('.file-preview .html-preview');
  await expect(preview).toBeVisible();
  const content = () => page.frameLocator('.file-preview .html-preview iframe').frameLocator('iframe').frameLocator('iframe');
  await expect(content().getByRole('heading', { name: 'Canvas first' })).toBeVisible();
  await expect(content().locator('body')).toHaveAttribute('data-executed', 'yes');
  await content().getByRole('button', { name: 'Increment' }).click();
  await expect(content().locator('#count')).toHaveText('clicked');
  expect(await content().locator('body').evaluate(() => ({ bridge: typeof (window as unknown as {haarness?: unknown}).haarness, parent: (() => { try { return !!parent.document; } catch { return false; } })() }))).toEqual({ bridge: 'undefined', parent: false });
  await page.locator('.file-actions').getByRole('button', { name: 'Code', exact: true }).click();
  await expect(page.locator('.file-preview .cm-content')).toContainText('Canvas first');
  await page.locator('.file-actions').getByRole('button', { name: 'Preview', exact: true }).click();
  await banner.getByRole('button', { name: 'Preview', exact: true }).click();
  await expect(banner.locator('.preview-inline .html-preview')).toBeVisible();
  await settled();
  await send('!tool edit ' + JSON.stringify({ path: 'canvas.html', old: 'Canvas first', new: 'Canvas second' }));
  await expect.poll(() => readFile(join(workspace, 'canvas.html'), 'utf8')).toBe(second);
  await settled();
  await send('!tool preview {"filePath":"canvas.html","title":"Canvas updated"}');
  await expect(page.locator('.preview-banner')).toHaveCount(2);
  await expect(content().getByRole('heading', { name: 'Canvas second' })).toBeVisible();
  const earlier = page.locator('.preview-banner').first();
  await expect(earlier).toContainText('Earlier version');
  await earlier.getByRole('button', { name: 'Compare', exact: true }).click();
  await expect(earlier.locator('.preview-removed')).toContainText('Canvas first');
  await expect(earlier.locator('.preview-added')).toContainText('Canvas second');
  await earlier.getByRole('button', { name: 'Open preview', exact: true }).click();
  await expect(page.locator('.preview-version-strip')).toBeVisible();
  await expect(content().getByRole('heading', { name: 'Canvas first' })).toBeVisible();
  await page.getByRole('button', { name: 'Show current', exact: true }).click();
  await expect(content().getByRole('heading', { name: 'Canvas second' })).toBeVisible();
  await earlier.getByRole('button', { name: 'Restore', exact: true }).click();
  await page.getByRole('dialog').getByRole('button', { name: /Confirm|Restore/, exact: false }).click();
  await expect.poll(() => readFile(join(workspace, 'canvas.html'), 'utf8')).toBe(first);
  await expect(content().getByRole('heading', { name: 'Canvas first' })).toBeVisible();
  await earlier.getByRole('button', { name: 'Undo restore', exact: true }).click();
  await page.getByRole('dialog').getByRole('button', { name: /Confirm|Restore/, exact: false }).click();
  await expect.poll(() => readFile(join(workspace, 'canvas.html'), 'utf8')).toBe(second);
  await writeFile(join(workspace, 'canvas.html'), first.replace('Canvas first', 'External change'));
  await page.locator('.preview-banner').last().getByRole('button', { name: 'Open preview', exact: true }).click();
  await expect(content().getByRole('heading', { name: 'Canvas second' })).toBeVisible();
  await expect(page.locator('.preview-banner').last()).toContainText('Earlier version');
  await page.getByRole('button', { name: 'Show current', exact: true }).click();
  await expect(content().getByRole('heading', { name: 'External change' })).toBeVisible();
  await page.screenshot({ path: test.info().outputPath('canvas-preview.png') });
});

test("canvas preview blocks network and navigation without affecting chat or normal file editing", async () => {
  await newCanvasSession();
  const requests: string[] = [];
  const receiver = createServer((request, response) => { requests.push(request.url ?? ''); response.end('unexpected'); });
  await new Promise<void>(resolve => receiver.listen(0, '127.0.0.1', resolve));
  const address = receiver.address();
  if (!address || typeof address === 'string') throw Error('Missing receiver');
  const url = `http://127.0.0.1:${address.port}`;
  try {
    // The page's own navigations (a script and a meta refresh) start when the
    // link is clicked, not at load: a blocked navigation can replace the page,
    // and on a loaded machine one started at load could do that before the
    // heading and link are checked.
    const navigate = `setTimeout(()=>{location.href='${url}/navigation'},300);var m=document.createElement('meta');m.httpEquiv='refresh';m.content='1;url=${url}/refresh';document.head.appendChild(m)`;
    const html = `<!doctype html><h1>Contained preview</h1><a href="${url}/link" onclick="${navigate}">Leave</a><img src="${url}/image"><script>fetch('${url}/fetch').catch(()=>{});</script>`;
    await writeFile(join(workspace, 'contained.html'), html);
    await send('!tool preview {"filePath":"contained.html"}');
    await expect(page.locator('.preview-banner')).toBeVisible();
    const content = page.frameLocator('.file-preview .html-preview iframe').frameLocator('iframe').frameLocator('iframe');
    await expect(content.getByRole('heading')).toHaveText('Contained preview');
    await content.getByRole('link', { name: 'Leave' }).click();
    await expect(page.locator('.file-preview .preview-notice')).toBeVisible();
    // Allow both the script and meta refresh timers to attempt their navigation.
    await page.waitForTimeout(1600);
    expect(requests).toEqual([]);
    // Chromium may replace the blocked frame with its local error document; never reload hostile scripts automatically.
    await expect(page.locator('.file-preview .preview-notice')).toBeVisible();
    await send('chat still works after sandbox attempts');
    await expect(page.getByRole('region', { name: 'Chat', exact: true })).toContainText('chat still works after sandbox attempts');
  } finally { await new Promise<void>(resolve => receiver.close(() => resolve())); }
});

test("canvas preview preserves unsaved code, renders markdown on request and never reopens from history", async () => {
  await newCanvasSession();
  await writeFile(join(workspace, 'draft-preview.html'), '<h1>Saved canvas</h1>');
  await send('!tool preview {"filePath":"draft-preview.html"}');
  await expect(page.locator('.preview-banner')).toBeVisible();
  await page.locator('.file-actions').getByRole('button', { name: 'Code', exact: true }).click();
  const editor = page.locator('.file-preview .cm-content');
  await editor.fill('<h1>Unsaved canvas</h1>');
  await settled();
  await send('!tool preview {"filePath":"draft-preview.html"}');
  await expect(page.locator('.preview-banner')).toHaveCount(2);
  await expect(editor).toContainText('Unsaved canvas');
  expect(await readFile(join(workspace, 'draft-preview.html'), 'utf8')).toContain('Saved canvas');
  await page.locator('.file-actions').getByRole('button', { name: 'Save', exact: true }).click();
  await expect.poll(() => readFile(join(workspace, 'draft-preview.html'), 'utf8')).toContain('Unsaved canvas');
  await expect(editor).toBeVisible();
  await writeFile(join(workspace, 'report-preview.md'), '# Rendered report\n\nPreview markdown content.');
  await settled();
  await send('!tool preview {"filePath":"report-preview.md"}');
  await expect(page.locator('.file-preview .document-preview')).toContainText('Rendered report');
  await expect(page.locator('.preview-banner')).toHaveCount(3);
  await page.getByRole('button', { name: 'Hide side panel', exact: true }).click();
  await expect.poll(() => page.evaluate(() => window.haarness.storage('layout:v2'))).toMatchObject({ dock: null });
  await page.reload();
  await expect(page.locator('.preview-banner')).toHaveCount(3);
  await expect(page.locator('.side-panel')).toBeHidden();
});

test("canvas preview loads a public npm library through the supported CDN", async () => {
  test.skip(process.env.HAARNESS_LIVE_PREVIEW_CDN !== '1', 'Explicit network integration check');
  await newCanvasSession();
  await writeFile(join(workspace, 'cdn-preview.html'), '<!doctype html><h1 id="result">Loading library</h1><script src="https://cdn.jsdelivr.net/npm/dayjs@1.11.13/dayjs.min.js"></script><script>document.getElementById("result").textContent=dayjs("2026-10-03").format("YYYY/MM/DD")</script>');
  await send('!tool preview {"filePath":"cdn-preview.html"}');
  const content = page.frameLocator('.file-preview .html-preview iframe').frameLocator('iframe').frameLocator('iframe');
  await expect(content.getByRole('heading')).toHaveText('2026/10/03', { timeout: 30000 });
});
