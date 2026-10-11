import { Haarness } from "@haarness/sdk";
import {
  test,
  expect,
  _electron as electron,
  type ElectronApplication,
  type Page,
} from "@playwright/test";
import {
  mkdtemp,
  mkdir,
  writeFile,
  readFile,
  rm,
  realpath,
  access,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";

const binary = resolve(
  process.env.HAARNESS_BIN ?? "../../target/release/haarness",
);
let app: ElectronApplication | undefined;
let page: Page;
let temporary: string;
let env: Record<string, string>;
let workspace: string | undefined;
let client: Haarness | undefined;

test.beforeEach(async () => {
  test.setTimeout(300000);
  temporary = await realpath(
    await mkdtemp(join(tmpdir(), "haarness-project-required-")),
  );
  workspace = undefined;
  await mkdir(join(temporary, "desktop"));
  await mkdir(join(temporary, "home"));
  env = Object.fromEntries(
    Object.entries(process.env).filter(
      (entry): entry is [string, string] => entry[1] !== undefined,
    ),
  );
  Object.assign(env, {
    HAARNESS_BIN: binary,
    HAARNESS_HOME: join(temporary, "haarness"),
    HAARNESS_DATA_DIR: join(temporary, "state"),
    HAARNESS_DESKTOP_DATA: join(temporary, "desktop"),
    HAARNESS_TEST_PROVIDER: "1",
    HAARNESS_DESKTOP_TEST: "1",
    HOME: join(temporary, "home"),
  });
  delete env.ELECTRON_RUN_AS_NODE;
});

async function launch() {
  const executablePath = process.env.HAARNESS_DESKTOP_EXECUTABLE;
  const launchEnv = { ...env };
  if (executablePath) delete launchEnv.HAARNESS_BIN;
  app = await electron.launch({
    timeout: 240000,
    ...(executablePath ? { executablePath, args: [] } : { args: ["."] }),
    env: launchEnv,
  });
  page = await app.firstWindow();
}

test.afterEach(async () => {
  client?.close();
  client = undefined;
  await app?.close().catch(() => {});
  app = undefined;
  if (workspace) {
    execFileSync(binary, ["--workspace", workspace, "serve", "--stop"], {
      env,
      stdio: "ignore",
      timeout: 30000,
    });
  }
  await rm(temporary, { recursive: true, force: true });
});

test("a fresh desktop shows project home, and canceling never creates a general chat", async () => {
  await launch();
  const home = page.getByRole("region", { name: "Project home" });
  await expect(
    home.getByRole("heading", { name: "Open a project to get started" }),
  ).toBeVisible();
  await expect(
    page.getByRole("textbox", { name: "Message", exact: true }),
  ).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: "Send message", exact: true }),
  ).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: "Attach files", exact: true }),
  ).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: "Open files", exact: true }),
  ).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: "Toggle terminal", exact: true }),
  ).toHaveCount(0);
  await expect(page.getByLabel("Chat starters")).toHaveCount(0);
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  await expect(
    page.getByRole("dialog", { name: "Settings", exact: true }),
  ).toBeVisible();
  await page
    .getByRole("button", { name: "Close settings", exact: true })
    .click();
  await app!.evaluate(({ dialog }) => {
    (globalThis as any).projectPickerCalls = 0;
    dialog.showOpenDialog = async () => {
      (globalThis as any).projectPickerCalls++;
      return { canceled: true, filePaths: [] };
    };
  });
  await page.getByRole("button", { name: "New chat", exact: true }).click();
  await expect
    .poll(() => app!.evaluate(() => (globalThis as any).projectPickerCalls))
    .toBe(1);
  await expect(
    home.getByRole("button", { name: "Open project", exact: true }),
  ).toBeVisible();
  await home.getByRole("button", { name: "Open project", exact: true }).click();
  await expect
    .poll(() => app!.evaluate(() => (globalThis as any).projectPickerCalls))
    .toBe(2);
  await expect(home.getByRole("heading")).toHaveText(
    "Open a project to get started",
  );
  // Exercise the native menu's renderer dispatch without requiring OS focus.
  await app!.evaluate(({ BrowserWindow }) => {
    BrowserWindow.getAllWindows()[0].webContents.send("haarness:event", {
      kind: "menu", data: "new",
    });
  });
  await expect.poll(() => app!.evaluate(() => (globalThis as any).projectPickerCalls)).toBe(3);
  const rejected = await page.evaluate(async () => {
    try {
      await window.haarness.connect({ projectless: true } as any);
      return "accepted";
    } catch (error) {
      return String(error);
    }
  });
  expect(rejected).toContain("projectless");
  expect((await page.evaluate(() => window.haarness.boot())).recent).toEqual(
    [],
  );
  await expect(access(join(temporary, "desktop", "chats"))).rejects.toThrow();
  await page.reload();
  await expect(home.getByRole("heading")).toHaveText(
    "Open a project to get started",
  );
  await expect(
    page.getByRole("textbox", { name: "Message", exact: true }),
  ).toHaveCount(0);
  await page.screenshot({ path: "test-results/project-required-empty.png" });
  await page.setViewportSize({ width: 720, height: 600 });
  await expect(
    home.getByRole("button", { name: "Open project", exact: true }),
  ).toBeInViewport();
  expect(
    await home.evaluate(
      (element) => element.scrollWidth <= element.clientWidth,
    ),
  ).toBe(true);
  await page.screenshot({ path: "test-results/project-home-narrow.png" });
});

async function chooseFolder(path: string) {
  await app!.evaluate(({ dialog }, directory) => {
    dialog.showOpenDialog = async () => ({
      canceled: false,
      filePaths: [directory],
    });
  }, path);
}

async function ready() {
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
    { timeout: 240000 },
  );
  await expect(
    page.getByRole("textbox", { name: "Message", exact: true }),
  ).toBeEnabled();
  await expect(page.getByRole("region", { name: "Project home" })).toHaveCount(
    0,
  );
}

async function forgetSelection() {
  await page.evaluate(async () => {
    await window.haarness.storage("active-project", null);
    await window.haarness.storage("server-project:local", null);
  });
  await page.reload();
  await expect(
    page
      .getByRole("region", { name: "Project home" })
      .getByRole("heading", { name: "Choose a project", exact: true }),
  ).toBeVisible();
}

test("opening from home enables chat, remembers explicit selection, and never selects a saved project implicitly", async () => {
  workspace = join(temporary, "chosen-project");
  await mkdir(workspace);
  await launch();
  await chooseFolder(workspace);
  await page
    .getByRole("region", { name: "Project home" })
    .getByRole("button", { name: "Open project", exact: true })
    .click();
  await ready();
  const composer = page.getByRole("textbox", { name: "Message", exact: true });
  await expect(page.locator(".conversation-empty h2")).toHaveText(
    "What should we work on in chosen-project?",
  );
  await page.getByRole("button", { name: "Explore code", exact: true }).click();
  await expect(composer).toHaveValue(
    "Explain the architecture of this project",
  );
  await composer.fill("project-home-proof");
  await page.getByRole("button", { name: "Send message", exact: true }).click();
  await expect(page.locator(".transcript")).toContainText(
    "Echo: project-home-proof",
    { timeout: 30000 },
  );
  await composer.fill("Remember this draft");
  await page.reload();
  await ready();
  await expect(page.locator(".transcript")).toContainText(
    "Echo: project-home-proof",
  );
  await expect(composer).toHaveValue("Remember this draft");
  await forgetSelection();
  const home = page.getByRole("region", { name: "Project home" });
  await expect(home).toContainText(workspace);
  await expect(composer).toHaveCount(0);
  await page.screenshot({ path: "test-results/project-home-recent.png" });
  await home.getByRole("button", { name: /chosen-project/ }).click();
  await ready();
  await expect(composer).toHaveValue("Remember this draft");
  await forgetSelection();
  await app!.evaluate(({ dialog }) => {
    dialog.showOpenDialog = async () => ({ canceled: true, filePaths: [] });
  });
  await page.getByRole("button", { name: "New chat", exact: true }).click();
  await expect(
    home.getByRole("heading", { name: "Choose a project", exact: true }),
  ).toBeVisible();
  await expect(composer).toHaveCount(0);
  await home.getByRole("button", { name: /chosen-project/ }).click();
  await ready();
});

test("a missing remembered project shows recovery instead of opening another saved project", async () => {
  workspace = join(temporary, "available-project");
  await mkdir(workspace);
  const missing = join(temporary, "missing-project");
  const id = "local:" + missing;
  await writeFile(
    join(temporary, "desktop", "desktop.json"),
    JSON.stringify({
      preferences: {},
      credentials: {},
      recent: [
        {
          id: "local:" + workspace,
          label: "available-project",
          workspace,
          remote: false,
          trust: false,
        },
        {
          id,
          label: "missing-project",
          workspace: missing,
          remote: false,
          trust: false,
        },
      ],
      values: {
        "active-project": id,
        "active-server": "local",
        "server-project:local": id,
      },
    }),
  );
  await launch();
  const home = page.getByRole("region", { name: "Project home" });
  await expect(
    home.getByRole("heading", { name: "Couldn’t open project" }),
  ).toBeVisible({ timeout: 240000 });
  await expect(composer()).toHaveCount(0);
  await expect(home.getByRole("alert")).not.toBeEmpty();
  await page.screenshot({ path: "test-results/project-home-unavailable.png" });
  await home.getByRole("button", { name: "Retry", exact: true }).click();
  await expect(
    home.getByRole("heading", { name: "Couldn’t open project" }),
  ).toBeVisible();
  await chooseFolder(workspace);
  await home
    .getByRole("button", { name: "Choose another project", exact: true })
    .click();
  await ready();
  await expect(page.locator(".conversation-empty h2")).toHaveText(
    "What should we work on in available-project?",
  );
});

function composer() {
  return page.getByRole("textbox", { name: "Message", exact: true });
}

test("legacy general chats reopen as a regular project with history and drafts intact", async () => {
  workspace = join(temporary, "desktop", "chats");
  await mkdir(workspace);
  const endpoint = JSON.parse(
    execFileSync(binary, ["--workspace", workspace, "desktop"], {
      env,
      input: JSON.stringify({ action: "connect" }),
      encoding: "utf8",
      timeout: 240000,
    }),
  );
  client = new Haarness({ baseUrl: endpoint.url, token: endpoint.token });
  const thread = await client.createThread({
    title: "Retained conversation",
    provider: "echo",
    model: "echo-1",
  });
  const turn = await client.startTurn(thread.id, {
    requestId: crypto.randomUUID(),
    text: "legacy-project-history-proof",
  });
  await client.waitTurn(turn.id);
  const id = "local:" + workspace;
  await writeFile(
    join(temporary, "desktop", "desktop.json"),
    JSON.stringify({
      preferences: {},
      recent: [
        {
          id,
          label: "Chats",
          workspace,
          remote: false,
          trust: false,
          projectless: true,
        },
      ],
      values: {
        "active-project": id,
        ["session:" + id]: thread.id,
        ["draft:" + id + ":"]: "preserved project draft",
        ["draft:" + id + ":" + thread.id]: "preserved conversation draft",
      },
      credentials: {},
    }),
  );
  await launch();
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
  );
  await expect(page.locator(".transcript")).toContainText(
    "Echo: legacy-project-history-proof",
  );
  await expect(
    page
      .locator(".project-list")
      .getByRole("button", { name: "Retained conversation", exact: true }),
  ).toBeVisible();
  const composer = page.getByRole("textbox", { name: "Message", exact: true });
  await expect(composer).toHaveValue("preserved conversation draft");
  const recent = (
    await page.evaluate(() => window.haarness.boot())
  ).recent.find((item) => item.id === id)!;
  expect(recent.workspace).toBe(workspace);
  expect(recent.trust).toBe(false);
  expect(recent.project?.root).toBe(workspace);
  expect("projectless" in recent).toBe(false);
  const stored = JSON.parse(
    await readFile(join(temporary, "desktop", "desktop.json"), "utf8"),
  );
  expect(stored.recent.some((item: any) => "projectless" in item)).toBe(false);
  await page.getByRole("button", { name: "New chat", exact: true }).click();
  await expect(composer).toHaveValue("preserved project draft");
  await expect(page.locator(".composer-project")).toContainText(
    recent.project!.name,
  );
  await expect(page.locator(".composer-project")).not.toContainText(
    "No project",
  );
  await expect(
    page.getByRole("button", { name: "Explore code", exact: true }),
  ).toBeVisible();
  await page.reload();
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
  );
  await expect(composer).toHaveValue("preserved project draft");
  await page
    .locator(".project-list")
    .getByRole("button", { name: "Retained conversation", exact: true })
    .click();
  await expect(page.locator(".transcript")).toContainText(
    "Echo: legacy-project-history-proof",
  );
  await expect(composer).toHaveValue("preserved conversation draft");
  await page.screenshot({ path: "test-results/project-required-migrated.png" });
});
