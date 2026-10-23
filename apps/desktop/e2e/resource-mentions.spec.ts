import { test, expect, _electron as electron, type ElectronApplication, type Page, type Locator } from "@playwright/test";
import { Haarness } from "@haarness/sdk";
import { mkdtemp, mkdir, writeFile, readFile, realpath, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";

// `@` mentions of MCP resources: the composer's `@` list asks the project's MCP servers for their resources the
// first time it opens and reuses the answer, lists them after agents and files with their server (leaving out a
// server the rules take away), and a pick is a chip that is read by the server when the message is sent, into a
// block of the sent message (images as attachments, one that cannot be read or that a rule keeps from the chat
// named in it). Two servers expose the same resources, so each URI is two mentions. Undo and a queued message's
// edit bring a resource back as a chip holding what was read, sent again without reading. A remote project lists
// and reads them through the same API.
test.describe.configure({ mode: "serial" });

let app: ElectronApplication, page: Page, client: Haarness;
let temporary: string, workspace: string, binary: string, env: Record<string, string>;
let thread = "", reads = "";
const errors: string[] = [];
const fixtures = resolve("../../crates/haarness-mcp/tests/fixtures");
const shots = process.env.HAARNESS_SCREENSHOTS;

test.beforeAll(async () => {
  test.setTimeout(240000);
  temporary = await realpath(await mkdtemp(join(process.env.HAARNESS_E2E_DIR ?? tmpdir(), "haarness-resource-mentions-")));
  workspace = join(temporary, "workspace");
  await mkdir(join(workspace, ".haarness"), { recursive: true });
  await writeFile(join(workspace, "settings.md"), "A file whose name the `@set` query also finds.\n");
  reads = join(temporary, "requests.log");
  const server = { command: "python3", args: [join(fixtures, "resources.py")], env: { REQUEST_LOG: reads } };
  await writeFile(
    join(workspace, ".haarness/haarness.json"),
    JSON.stringify(
      {
        model: "echo/echo-1",
        // `closed` lends the model none of its tools, so none of its resources either; one wiki page is kept from it.
        permission: { mcp_resource_read: { "wiki:docs://logo": "deny" }, "mcp__closed__*": "deny" },
        mcp: { docs: server, wiki: server, closed: server },
      },
      null,
      2,
    ),
  );
  // A Git project keeps snapshots, which Undo needs.
  execFileSync("git", ["init", "-q", "-b", "main"], { cwd: workspace });
  binary = resolve(process.env.HAARNESS_BIN ?? "../../target/debug/haarness");
  env = {
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
  await page.setViewportSize({ width: 1400, height: 950 });
  // Every API call the window makes, by method, from the first one.
  await app.evaluate(({ ipcMain }) => {
    const handlers = (ipcMain as unknown as { _invokeHandlers: Map<string, (...args: any[]) => unknown> })._invokeHandlers;
    const original = handlers.get("haarness:api")!;
    (globalThis as any).apiCalls = [];
    ipcMain.removeHandler("haarness:api");
    // `filesDelay` holds file searches back, as a large project's would be.
    (globalThis as any).filesDelay = 0;
    ipcMain.handle("haarness:api", async (event, raw) => {
      (globalThis as any).apiCalls.push(raw.method);
      const delay = raw.method === "searchFiles" ? (globalThis as any).filesDelay : 0;
      if (delay) await new Promise((done) => setTimeout(done, delay));
      return original(event, raw);
    });
  });
  await app.evaluate(({ dialog }, directory) => {
    dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [directory] });
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
  await expect
    .poll(async () => Object.fromEntries((await client.mcpServers()).map((server) => [server.name, server.status])), { timeout: 30000 })
    .toEqual({ docs: "connected", wiki: "connected", closed: "connected" });
  thread = (await client.createThread({ title: "Resource mentions", provider: "echo", model: "echo-1" })).id;
  await page.locator(".sessions").getByRole("button", { name: "Resource mentions", exact: true }).click();
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
  if (temporary) await rm(temporary, { recursive: true, force: true }).catch(() => {});
});

const input = () => page.getByRole("textbox", { name: "Message", exact: true });
const list = () => page.getByRole("listbox", { name: "Files, agents and resources", exact: true });
const rows = () => list().locator('[role="option"][data-kind="resource"]');
const row = (name: string, server: string) =>
  rows()
    .filter({ has: page.locator("strong", { hasText: new RegExp(`^${name}$`) }) })
    .filter({ has: page.locator(".mention-kind", { hasText: new RegExp(`^${server}$`) }) });
const chips = () => page.getByLabel("Attached context", { exact: true }).locator(".prompt-context-item");
const listings = () =>
  app.evaluate(() => ((globalThis as any).apiCalls as string[]).filter((method) => method === "mcpResources").length);
/** How many resources the MCP servers were asked to read. */
const readsSoFar = async () =>
  (await readFile(reads, "utf8").catch(() => "")).split("\n").filter((line) => line === "resources/read").length;
async function settled() {
  await expect.poll(async () => (await client.activeTurns()).length, { timeout: 30000 }).toBe(0);
}
const sent = async () =>
  (await client.listItems(thread)).flatMap((item) => (item.data.type === "user_message" ? [item.data] : []));
/** Light and dark pictures of `target` (or of the window around several), kept when HAARNESS_SCREENSHOTS names a folder. */
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

test("the @ list asks for resources the first time it opens, then lists them after files with their server", async () => {
  // Opening the project and a chat asked no MCP server for its resources.
  await page.waitForTimeout(500);
  expect(await listings()).toBe(0);
  await input().fill("@");
  // `closed` is left out: the rules take all its tools, and so its resources, away.
  await expect(rows()).toHaveCount(4);
  await expect(rows().locator(".mention-kind", { hasText: "closed" })).toHaveCount(0);
  // One listing per connected server, asked at once.
  expect(await listings()).toBe(3);
  await expect(row("setup", "docs")).toContainText("Setup guide");
  await expect(row("setup", "docs").locator(".mention-kind")).toHaveText("docs");
  await expect(row("logo", "wiki").locator("small").first()).toHaveText("docs://logo");
  // Resources come after the agents and files.
  const kinds = await list().locator('[role="option"]').evaluateAll((options) => options.map((option) => option.getAttribute("data-kind")));
  expect(kinds.indexOf("resource")).toBeGreaterThan(Math.max(kinds.lastIndexOf("file"), kinds.lastIndexOf("agent")));
  expect(kinds.slice(kinds.indexOf("resource")).every((kind) => kind === "resource")).toBe(true);
  // Their text has the size of the other rows'.
  const sizes = await list()
    .locator('[role="option"]')
    .evaluateAll((options) => [...new Set(options.map((option) => getComputedStyle(option.querySelector("strong")!).fontSize))]);
  expect(sizes).toHaveLength(1);
  // Typing filters them by name.
  await input().fill("@set");
  await expect(rows()).toHaveCount(2);
  await expect(list().locator('[role="option"][data-kind="file"]').filter({ hasText: "settings.md" })).toHaveCount(1);
  await pictures(page.locator(".mention-popover"), "resource-rows");
  // A second `@`, in this chat or another, within the minute asks nothing.
  await page.keyboard.press("Escape");
  await input().fill("");
  await input().fill("@");
  await expect(rows()).toHaveCount(4);
  const other = await client.createThread({ title: "Other chat", provider: "echo", model: "echo-1" });
  await page.locator(".sessions").getByRole("button", { name: "Other chat", exact: true }).click();
  await input().fill("@lo");
  await expect(rows()).toHaveCount(2);
  expect(await listings()).toBe(3);
  await input().fill("");
  await client.updateThread(other.id, { archived: true });
  await page.locator(".sessions").getByRole("button", { name: "Resource mentions", exact: true }).click();
});

test("resources wait below the files a slow search is still finding, so a quick Enter never picks one", async () => {
  await app.evaluate(() => ((globalThis as any).filesDelay = 2500));
  await input().fill("@setting");
  await expect(page.locator(".mention-popover [role=status]")).toHaveText("Searching files…");
  // No resource fills the files' place meanwhile, and Enter picks nothing.
  await expect(rows()).toHaveCount(0);
  await pictures(page.locator(".mention-popover"), "resource-rows-while-files-load");
  await input().fill("@set");
  await expect(page.locator(".mention-popover [role=status]")).toHaveText("Searching files…");
  await expect(rows()).toHaveCount(0);
  await input().press("Enter");
  await expect(chips()).toHaveCount(0);
  await expect(input()).toHaveValue("@set");
  // The files come first, the resources below them, the first file highlighted.
  await expect(rows()).toHaveCount(2);
  const kinds = await list().locator('[role="option"]').evaluateAll((options) => options.map((option) => option.getAttribute("data-kind")));
  expect(kinds.indexOf("file")).toBeGreaterThanOrEqual(0);
  expect(kinds.indexOf("resource")).toBeGreaterThan(kinds.lastIndexOf("file"));
  await expect(list().locator('[role="option"][aria-selected="true"]')).toHaveAttribute("data-kind", kinds[0]!);
  await app.evaluate(() => ((globalThis as any).filesDelay = 0));
  await page.keyboard.press("Escape");
  await input().fill("");
});

test("a picked resource is a chip that the server reads when the message is sent", async () => {
  await input().fill("Compare @set");
  await row("setup", "docs").click();
  await expect(input()).toHaveValue("Compare ");
  // The keyboard reaches the other server's copy of the same URI.
  await input().press("End");
  await input().pressSequentially("@set");
  await expect(rows()).toHaveCount(2);
  const index = await list()
    .locator('[role="option"]')
    .evaluateAll((options) => options.findIndex((option) => option.getAttribute("data-kind") === "resource" && option.textContent!.includes("wiki")));
  for (let step = 0; step < index; step++) await input().press("ArrowDown");
  await expect(list().locator('[role="option"].highlighted')).toContainText("wiki");
  await input().press("Enter");
  await expect(chips()).toHaveCount(2);
  // The same name from two servers says which server each is.
  await expect(chips().locator("summary")).toHaveText(["@setup · docs", "@setup · wiki"]);
  await expect(chips().locator("summary").nth(0)).toHaveAttribute("title", "docs://setup · docs");
  await expect(chips().locator("summary").nth(1)).toHaveAttribute("title", "docs://setup · wiki");
  await chips().nth(0).locator("summary").click();
  const preview = chips().nth(0).locator(".prompt-context-preview");
  await expect(preview).toContainText("MCP resource · docs");
  await expect(preview).toContainText("docs://setup");
  await expect(preview).toContainText("Read from docs when the message is sent.");
  await pictures([preview, page.locator(".composer")], "resource-chip");
  await page.keyboard.press("Escape");
  await expect(preview).toBeHidden();
  // The draft keeps them across a reload.
  await page.reload();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 60000 });
  await expect(chips()).toHaveCount(2);
  await expect(input()).toHaveValue("Compare ");
  await input().press("Enter");
  await expect(chips()).toHaveCount(0);
  await settled();
  const [message] = await sent();
  expect(message.text).toBe(
    [
      "Compare",
      "<details>\n<summary>MCP resource: setup · docs:docs://setup</summary>\n\n```\n# Setup\nRun the fixture.\n```\n\n</details>",
      "<details>\n<summary>MCP resource: setup · wiki:docs://setup</summary>\n\n```\n# Setup\nRun the fixture.\n```\n\n</details>",
    ].join("\n\n"),
  );
  // The bubble folds each one under its name and address, like a paste.
  const bubble = page.locator(".transcript .message.user").last();
  const read = bubble.locator(".sent-resource");
  await expect(read.locator(".sent-resource-label > span")).toHaveText(["@setup", "@setup"]);
  await expect(read.locator(".sent-resource-label > small")).toHaveText(["docs:docs://setup", "wiki:docs://setup"]);
  await expect(bubble.locator(".sent-paste-text")).toHaveCount(0);
  await expect(bubble).not.toContainText("Run the fixture.");
  await read.first().getByRole("button").click();
  await expect(read.first().locator(".sent-paste-text")).toHaveText("# Setup\nRun the fixture.");
  // The model read what the server did: the echo provider repeats it.
  await expect(page.locator(".transcript .message.assistant").last()).toContainText("Run the fixture.");
  await pictures(bubble, "resource-message");
  // Prompt history brings the chips back, and sending them reads them again.
  await input().focus();
  await input().press("ArrowUp");
  await expect(input()).toHaveValue("Compare ");
  await expect(chips()).toHaveCount(2);
  for (const _ of [0, 1]) await chips().first().getByRole("button").click();
  await input().fill("");
  await expect(chips()).toHaveCount(0);
});

test("an image resource is attached, and one its server cannot read or a rule keeps is named in the message", async () => {
  await input().fill("Look @logo");
  await row("logo", "docs").click();
  await input().pressSequentially("@set");
  await row("setup", "wiki").click();
  await input().pressSequentially("@logo");
  await row("logo", "wiki").click();
  await expect(chips()).toHaveCount(3);
  // The wiki server is gone by the time the message is sent.
  await client.disconnectMcp("wiki");
  await input().press("Enter");
  await expect(chips()).toHaveCount(0);
  await settled();
  const message = (await sent()).at(-1)!;
  expect(message.attachments).toHaveLength(1);
  expect(message.attachments![0]).toMatch(/^data:image\/png/);
  expect(message.text).toContain("<summary>MCP resource: logo · docs:docs://logo</summary>\n\n```\n[image/png attachment: docs://logo]\n```");
  expect(message.text).toContain("<summary>MCP resource: setup · wiki:docs://setup (not read)</summary>\n\n```\nMCP server wiki is not connected\n```");
  expect(message.text).toContain("<summary>MCP resource: logo · wiki:docs://logo (not read)</summary>\n\n```\na rule keeps this resource from the model\n```");
  const bubble = page.locator(".transcript .message.user").last();
  const unread = bubble.locator(".sent-resource.failed");
  await expect(unread).toHaveCount(2);
  await expect(unread.locator(".sent-resource-unread").first()).toContainText("@setup");
  await expect(unread.locator(".sent-resource-unread").first()).toContainText("not read");
  // The reason alone: the row already names the resource.
  await expect(unread.locator(".sent-resource-reason")).toHaveText([
    "MCP server wiki is not connected",
    "a rule keeps this resource from the model",
  ]);
  await expect(page.locator(".permission")).toHaveCount(0);
  await expect(bubble.locator(".sent-resource:not(.failed) .sent-resource-label > span")).toHaveText("@logo");
  // A quiet line in the message, never a dialog or a banner.
  await expect(page.getByRole("alertdialog")).toHaveCount(0);
  await expect(page.locator(".error-banner")).toHaveCount(0);
  await pictures(bubble, "resource-not-read");
  await client.reconnectMcp("wiki");
});

test("Undo brings a resource back as a chip holding what was read, sent again without reading it", async () => {
  await settled();
  await input().fill("Undo me @set");
  await row("setup", "docs").click();
  // One a rule keeps from the chat comes back to be read again.
  await input().pressSequentially("@logo");
  await row("logo", "wiki").click();
  await input().press("Enter");
  await expect.poll(async () => (await sent()).at(-1)?.text.startsWith("Undo me")).toBe(true);
  await settled();
  const count = (await sent()).length;
  const sentBefore = (await sent()).at(-1)!.text;
  const before = await readsSoFar();
  await page.getByRole("button", { name: "Chat actions", exact: true }).click();
  await page.getByRole("menuitem", { name: "Undo last turn", exact: true }).click();
  await expect(input()).toHaveValue("Undo me");
  await expect(chips()).toHaveCount(2);
  await expect(chips().locator("summary")).toHaveText(["@setup", "@logo"]);
  await chips().first().locator("summary").click();
  await expect(chips().first().locator(".prompt-context-preview pre")).toHaveText("# Setup\nRun the fixture.");
  await expect(chips().first().locator(".prompt-context-preview")).toContainText("Sent again as it was read.");
  await pictures([chips().first().locator(".prompt-context-preview"), page.locator(".composer")], "resource-chip-undone");
  await page.keyboard.press("Escape");
  await chips().last().locator("summary").click();
  await expect(chips().last().locator(".prompt-context-preview")).toContainText("Read from wiki when the message is sent.");
  await page.keyboard.press("Escape");
  await input().press("Enter");
  // The undone message is gone, and the one sent in its place is the same.
  await expect.poll(async () => (await sent()).length).toBe(count);
  await expect.poll(async () => (await sent()).at(-1)?.text).toBe(sentBefore);
  await settled();
  expect(await readsSoFar()).toBe(before);
});

test("a queued message's edit keeps its resource as a chip and offers no new ones", async () => {
  await settled();
  await input().fill("!linger 15000 slow work");
  await input().press("Enter");
  await expect(page.locator(".composer")).toHaveClass(/running/);
  await input().fill("Queued note @set");
  await row("setup", "docs").click();
  await input().press("Alt+Enter");
  const queued = page.locator(".input-queue-row").first();
  await expect(queued.locator(".input-queue-text")).toHaveText("Queued note · @setup");
  await queued.getByRole("button", { name: "Edit queued message" }).click();
  await expect(input()).toHaveValue("Queued note");
  await expect(chips()).toHaveCount(1);
  await expect(chips().locator("summary")).toHaveText("@setup");
  // Saving keeps text only, so no new resource is offered meanwhile.
  await input().fill("Queued note, edited @set");
  await expect(list().or(page.getByRole("listbox", { name: "Files and agents", exact: true }))).toBeVisible();
  await expect(page.locator('.mention-options [role="option"][data-kind="resource"]')).toHaveCount(0);
  await page.keyboard.press("Escape");
  await input().fill("Queued note, edited");
  await page.getByRole("button", { name: "Save queued message", exact: true }).click();
  await expect(queued.locator(".input-queue-text")).toHaveText("Queued note, edited · @setup");
  const pending = (await client.inputQueue(thread)).items[0];
  expect(pending.text).toContain("<summary>MCP resource: setup · docs:docs://setup</summary>\n\n```\n# Setup\nRun the fixture.\n```");
  await page.getByRole("button", { name: "Stop response", exact: true }).click().catch(() => {});
  await settled();
});

test("a remote project lists and reads resources through the same API", async () => {
  const endpoint = JSON.parse(
    execFileSync(binary, ["--workspace", workspace, "--trust-project", "desktop"], {
      env,
      input: JSON.stringify({ action: "connect" }),
      encoding: "utf8",
      timeout: 30000,
    }),
  );
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  const settings = page.getByRole("dialog", { name: "Settings", exact: true });
  await settings.getByRole("button", { name: "Servers", exact: true }).click();
  await settings.getByRole("button", { name: "Add remote server", exact: true }).click();
  const form = page.getByRole("dialog", { name: "Connect to server", exact: true });
  await form.getByLabel("Server URL", { exact: true }).fill(endpoint.url);
  await form.getByLabel("Name", { exact: true }).fill("Resource remote");
  await form.getByRole("button", { name: "Connect", exact: true }).click();
  await expect(form).toHaveCount(0);
  if (await settings.isVisible()) await settings.getByRole("button", { name: "Close settings", exact: true }).click();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
  await expect(page.getByRole("button", { name: "Switch server", exact: true })).toContainText("Resource remote");
  await page.getByRole("button", { name: "New chat", exact: true }).first().click();
  await input().fill("Remote @set");
  await row("setup", "docs").click();
  await expect(chips()).toHaveCount(1);
  await input().press("Enter");
  await expect(page.locator(".transcript .message.assistant").last()).toContainText("Run the fixture.");
  await expect(page.locator(".transcript .message.user").last().locator(".sent-resource-label > small")).toHaveText(
    "docs:docs://setup",
  );
  expect(errors).toEqual([]);
});
