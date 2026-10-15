import { test, expect, _electron as electron, type ElectronApplication, type Page } from "@playwright/test";
import { Haarness } from "@haarness/sdk";
import { mkdtemp, mkdir, writeFile, realpath, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";

// Links a model writes, in a reply or a Markdown file, and haarness:// links
// from outside: a file link opens in the Files panel or not at all, nothing
// reaches the system's file opener, and no link trusts a folder by itself.
test.describe.configure({ mode: "serial" });

let app: ElectronApplication, page: Page, client: Haarness;
let temporary: string, workspace: string, other: string;
const errors: string[] = [];

type Box = { message: string; detail?: string; buttons?: string[]; cancelId?: number };
type Calls = { shell: unknown[][]; boxes: Box[]; answer: number };

test.beforeAll(async () => {
  test.setTimeout(240000);
  temporary = await realpath(await mkdtemp(join(tmpdir(), "haarness-links-")));
  workspace = join(temporary, "workspace");
  other = join(temporary, "linked project");
  await mkdir(join(workspace, ".haarness"), { recursive: true });
  await mkdir(join(workspace, "src"));
  await mkdir(join(workspace, "docs"));
  await mkdir(other);
  await writeFile(join(workspace, ".haarness/config.toml"), '[defaults]\nprovider="echo"\nmodel="echo-1"\n');
  await writeFile(
    join(workspace, "src/a.ts"),
    Array.from({ length: 200 }, (_, index) => `export const line${index + 1} = ${index + 1};`).join("\n") + "\n",
  );
  await writeFile(join(workspace, "run.command"), "#!/bin/sh\necho linked script ran\n", { mode: 0o755 });
  await writeFile(
    join(workspace, "diagram.svg"),
    '<svg xmlns="http://www.w3.org/2000/svg" width="40" height="40"><rect width="40" height="40" fill="teal"/></svg>\n',
  );
  await writeFile(
    join(workspace, "docs/guide.md"),
    "# Guide\n\nRead [the source](../src/a.ts#L120) or [elsewhere](../../outside.md).\n",
  );
  const binary = resolve(process.env.HAARNESS_BIN ?? "../../target/debug/haarness");
  const env: Record<string, string> = {
    ...(process.env as Record<string, string>),
    HAARNESS_BIN: binary,
    HAARNESS_HOME: join(temporary, "home"),
    HAARNESS_DATA_DIR: join(temporary, "data"),
    HAARNESS_DESKTOP_DATA: join(temporary, "desktop"),
    HAARNESS_TEST_PROVIDER: "1",
    HAARNESS_DESKTOP_TEST: "1",
  };
  delete env.ELECTRON_RUN_AS_NODE;
  delete env.ELECTRON_RENDERER_URL;
  app = await electron.launch({ args: ["."], env, timeout: 60000 });
  page = await app.firstWindow();
  page.on("pageerror", (error) => errors.push(error.message));
  await page.setViewportSize({ width: 1500, height: 950 });
  // Record what would reach the system instead of opening anything.
  await app.evaluate(({ dialog, shell }, directory) => {
    const calls: Calls = { shell: [], boxes: [], answer: 0 };
    (globalThis as unknown as { calls: Calls }).calls = calls;
    dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [directory] });
    dialog.showMessageBox = (async (...args: unknown[]) => {
      calls.boxes.push(args.at(-1) as Box);
      return { response: calls.answer, checkboxChecked: false };
    }) as typeof dialog.showMessageBox;
    shell.openExternal = async (...args: unknown[]) => void calls.shell.push(["openExternal", ...args]);
    shell.openPath = async (...args: unknown[]) => {
      calls.shell.push(["openPath", ...args]);
      return "";
    };
    shell.showItemInFolder = (...args: unknown[]) => void calls.shell.push(["showItemInFolder", ...args]);
  }, workspace);
  await page.getByRole("region", { name: "Project home" }).getByRole("button", { name: "Open project", exact: true }).click();
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
});

test.afterAll(async () => {
  client?.close();
  await app?.close();
  if (temporary) await rm(temporary, { recursive: true, force: true }).catch(() => {});
});

const calls = () => app.evaluate(() => (globalThis as unknown as { calls: Calls }).calls);
const toast = () => page.locator(".notifications .toast");

test("file links in a reply open project files in the Files panel; others say why they do not open", async ({}, info) => {
  test.setTimeout(180000);
  const thread = await client.createThread({ title: "Link fixture", provider: "echo", model: "echo-1" });
  await page.locator(".sessions").getByRole("button", { name: "Link fixture", exact: true }).click();
  const links = [
    "[source](src/a.ts:150)",
    "[script](run.command)",
    "[diagram](diagram.svg)",
    "[guide](docs/guide.md)",
    "[calculator](file:///Applications/Calculator.app)",
    "[share](//server/share/x.md)",
    "[unc](\\\\\\\\server\\\\share)",
    "[smb](smb://server/share)",
    "[script url](javascript:alert(1))",
    "[missing](src/missing.ts)",
    "[above](../outside.txt)",
    `[absolute](file://${encodeURI(workspace)}/src/a.ts#L7)`,
    "[mail](mailto:me@example.test?subject=Hi&attach=/etc/passwd)",
  ];
  const turn = await client.startTurn(thread.id, { requestId: crypto.randomUUID(), text: links.join(" ") });
  await client.waitTurn(turn.id);
  const reply = page.locator(".message.assistant .markdown").last();
  await expect(reply).toContainText("Echo:");
  const link = (name: string) => reply.getByRole("link", { name, exact: true });
  const editor = page.locator(".file-preview .cm-content");
  const activeTab = page.locator(".file-tabs .active");

  await link("source").click();
  await expect(activeTab).toContainText("a.ts");
  await expect(page.locator(".file-preview .cm-activeLine")).toHaveText("export const line150 = 150;");
  await expect(page.locator(".file-preview .cm-activeLine")).toBeInViewport();
  await link("absolute").click();
  await expect(page.locator(".file-preview .cm-activeLine")).toHaveText("export const line7 = 7;");

  // A script opens as text, and an SVG only in the sandboxed preview.
  await link("script").click();
  await expect(activeTab).toContainText("run.command");
  await expect(editor).toContainText("echo linked script ran");
  await link("diagram").click();
  await expect(activeTab).toContainText("diagram.svg");
  await expect(page.locator(".file-preview .html-preview iframe")).toBeVisible();

  for (const [name, notice] of [
    ["calculator", "Only files in this project open from a link."],
    ["above", "Only files in this project open from a link."],
    ["share", "Links to network locations do not open."],
    ["unc", "Links to network locations do not open."],
    ["smb", "Links to network locations do not open."],
    ["script url", "This kind of link does not open in Haarness."],
    ["missing", "src/missing.ts was not found in this project."],
  ] as const) {
    await link(name).click();
    await expect(toast(), name).toHaveText(notice);
    await expect(toast()).toHaveAttribute("data-tone", "blocked");
  }
  await expect(page.locator(".error-banner")).toHaveCount(0);
  await toast().screenshot({ path: info.outputPath("link-notice-light.png") });
  await page.evaluate(() => window.haarness.preferences({ theme: "dark" }));
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await link("share").click();
  await expect(toast()).toHaveText("Links to network locations do not open.");
  await page.screenshot({ path: info.outputPath("link-notice-dark.png") });
  await page.evaluate(() => window.haarness.preferences({ theme: "light" }));

  // A mail link still leaves for the mail app, without a file to attach.
  await link("mail").click();
  await expect.poll(async () => (await calls()).shell).toEqual([
    ["openExternal", "mailto:me@example.test?subject=Hi"],
  ]);

  // Relative links in a previewed Markdown file start from its folder.
  await link("guide").click();
  await expect(activeTab).toContainText("guide.md");
  await page.locator(".file-actions").getByRole("button", { name: "Preview", exact: true }).click();
  const document = page.locator(".file-preview .document-preview .markdown");
  await document.getByRole("link", { name: "the source", exact: true }).click();
  await expect(activeTab).toContainText("a.ts");
  await expect(page.locator(".file-preview .cm-activeLine")).toHaveText("export const line120 = 120;");
  await page.locator(".file-tabs").getByRole("button", { name: "guide.md", exact: true }).click();
  await document.getByRole("link", { name: "elsewhere", exact: true }).click();
  await expect(toast()).toHaveText("Only files in this project open from a link.");

  // Nothing but the mail link reached the system.
  expect((await calls()).shell).toEqual([["openExternal", "mailto:me@example.test?subject=Hi"]]);
  expect(errors).toEqual([]);
});

test("a haarness:// link opens a saved folder directly and confirms any other, which opens untrusted", async ({}, info) => {
  test.setTimeout(180000);
  const deepLink = (url: string) =>
    app.evaluate(({ BrowserWindow }, url) => {
      BrowserWindow.getAllWindows()[0]!.webContents.send("haarness:event", {
        kind: "menu",
        data: { action: "deep-link", url },
      });
    }, url);
  const projects = () => page.evaluate(() => window.haarness.projects());
  const linked = "haarness://open?workspace=" + encodeURIComponent(` ${other} `);

  // Declined: nothing opens and nothing is saved.
  await app.evaluate(() => void ((globalThis as unknown as { calls: Calls }).calls.answer = 1));
  await deepLink(linked);
  await expect.poll(async () => (await calls()).boxes.length).toBe(1);
  const box = (await calls()).boxes[0]!;
  expect(box.message).toBe("Open “linked project” from a link?");
  expect(box.detail).toContain(other);
  expect(box.detail).toContain("MCP servers, hooks and extensions in this folder do not run");
  expect(box.buttons).toEqual(["Open folder", "Cancel"]);
  expect(box.cancelId).toBe(1);
  await page.waitForTimeout(500);
  expect((await projects()).some((item) => item.workspace === other)).toBe(false);

  // Accepted: the folder opens with its project configuration off.
  await app.evaluate(() => void ((globalThis as unknown as { calls: Calls }).calls.answer = 0));
  await deepLink(linked);
  await expect.poll(async () => (await calls()).boxes.length).toBe(2);
  await expect
    .poll(async () => (await projects()).find((item) => item.workspace === other)?.trust)
    .toBe(false);
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 90000 });
  await page.screenshot({ path: info.outputPath("deep-link-untrusted.png") });

  // A folder already in the sidebar opens as saved, without asking, at the linked chat.
  const thread = (await client.listThreads()).find((item) => item.title === "Link fixture")!;
  await deepLink(`haarness://open?workspace=${encodeURIComponent(workspace)}&session=%20${thread.id}%20`);
  await expect(page.locator(".session-bar")).toContainText("Link fixture", { timeout: 90000 });
  expect((await calls()).boxes.length).toBe(2);
  expect((await projects()).find((item) => item.workspace === workspace)?.trust).toBe(true);

  // A network folder is refused before anything looks at it.
  await deepLink("haarness://open?workspace=" + encodeURIComponent("//server/share"));
  await expect(toast()).toHaveText("This Haarness link could not be opened.");
  expect((await calls()).boxes.length).toBe(2);
  expect(errors).toEqual([]);
});
