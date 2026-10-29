import { test, expect, _electron as electron, type ElectronApplication, type Locator, type Page } from "@playwright/test";
import { Haarness } from "@haarness/sdk";
import { mkdtemp, mkdir, writeFile, realpath } from "node:fs/promises";
import { execFileSync } from "node:child_process";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { stopChatHome } from "./fixtures/chat-home";
import { isolatedEnv } from "./fixtures/isolated-env";

// The agent's shell commands in the background, as a person at Desktop
// sees and handles them: the row's badge, time and live output, Stop and
// Send to background, the title bar's count and Still running, the
// sidebar's ring, the end that reaches the chat as a hidden message, and a
// server that has none of it. The echo provider runs every command for
// real. Pictures are kept in HAARNESS_SCREENSHOTS when it is set.
let app: ElectronApplication, page: Page, root: string, project: string;
let env: Record<string, string>;
let client: Haarness;
const binary = resolve(process.env.HAARNESS_BIN ?? "../../target/debug/haarness");
// A server from before commands could go to the background.
const older = process.env.HAARNESS_OLDER_BIN ? resolve(process.env.HAARNESS_OLDER_BIN) : undefined;
const shots = process.env.HAARNESS_SCREENSHOTS;
const errors: string[] = [];
test.describe.configure({ mode: "serial" });

async function api(method: string, ...args: unknown[]): Promise<any> {
  return page.evaluate(
    ({ connection, method, args }) => (window as any).haarness.api(connection, method, args),
    { connection: "local:" + project, method, args },
  );
}
async function send(text: string) {
  const box = page.getByRole("textbox", { name: "Message", exact: true });
  await box.fill(" " + text);
  await page.getByRole("button", { name: /Send message|Send guidance/ }).click();
}
const bash = (command: string, background = false) =>
  "!tool bash " + JSON.stringify(background ? { command, run_in_background: true } : { command });
async function idle() {
  await expect.poll(async () => (await api("activeTurns")).length, { timeout: 30000 }).toBe(0);
}
async function newChat() {
  await page.getByRole("button", { name: "New chat", exact: true }).click();
  await expect(page.getByRole("textbox", { name: "Message", exact: true })).toBeVisible();
}
/** The chat the last message made: each test starts one. */
async function shownChat(): Promise<string> {
  const threads: any[] = await api("listThreads", {});
  return threads
    .filter((thread) => !thread.isSubagent)
    .sort((a, b) => b.createdAt.localeCompare(a.createdAt))[0].id;
}
async function commands(thread: string): Promise<any[]> {
  return api("listShellCommands", thread);
}
/** The transcript's row of the command whose line holds `text`. */
const row = (text: string) => page.locator(".transcript details.tool-call").filter({ hasText: text }).last();
/** The controls beside that row (its message holds both). */
const step = (text: string) =>
  page.locator(".transcript .message.step").filter({ has: page.locator("details.tool-call").filter({ hasText: text }) }).last();
const pill = () => page.locator(".background-pill");
async function pictures(name: string, target: Locator | Page) {
  for (const theme of ["light", "dark"] as const) {
    await page.evaluate((theme) => (window as any).haarness.preferences({ theme }), theme);
    await expect(page.locator("html")).toHaveAttribute("data-theme", theme);
    await page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done))));
    // The theme's colours ease in.
    await page.waitForTimeout(300);
    await target.screenshot({ path: shots ? join(shots, `${name}-${theme}.png`) : test.info().outputPath(`${name}-${theme}.png`) });
  }
  await page.evaluate(() => (window as any).haarness.preferences({ theme: "light" }));
}
/** Every text `.tool-state` showed in the transcript from now on. */
async function recordStates() {
  await page.evaluate(() => {
    const seen = ((window as any).__states = [] as string[]);
    const note = () => {
      for (const element of document.querySelectorAll(".transcript .tool-state")) {
        const text = element.textContent ?? "";
        if (seen.at(-1) !== text) seen.push(text);
      }
    };
    new MutationObserver(note).observe(document.body, { subtree: true, childList: true, characterData: true });
  });
}

async function launch(bin: string) {
  env = isolatedEnv();
  Object.assign(env, {
    HAARNESS_BIN: bin,
    HAARNESS_HOME: join(root, "home"),
    HAARNESS_DATA_DIR: join(root, "data"),
    HAARNESS_DESKTOP_DATA: join(root, "desktop"),
    HAARNESS_TEST_PROVIDER: "1",
    HAARNESS_DESKTOP_TEST: "1",
  });
  delete env.ELECTRON_RUN_AS_NODE;
  delete env.ELECTRON_RENDERER_URL;
  app = await electron.launch({ args: ["."], env, timeout: 60000 });
  page = await app.firstWindow();
  page.setDefaultTimeout(15000);
  page.on("pageerror", (error) => errors.push(error.message));
  await app.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows()[0].setSize(1280, 860));
  await app.evaluate(({ dialog }, path) => {
    dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [path] });
  }, project);
  await page.getByRole("button", { name: "Open another project", exact: true }).click();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 60000 });
  await newChat();
}
async function close(bin: string) {
  client?.close();
  client = undefined as unknown as Haarness;
  if (app) {
    await app.evaluate(({ BrowserWindow }) => {
      for (const window of BrowserWindow.getAllWindows()) window.destroy();
    }).catch(() => {});
    await app.close().catch(() => {});
  }
  try {
    execFileSync(bin, ["--workspace", project, "serve", "--stop"], { env, stdio: "ignore", timeout: 30000 });
  } catch {}
  stopChatHome(bin, env);
}

test.describe("a server with commands in the background", () => {
  test.beforeAll(async () => {
    test.setTimeout(150000);
    root = await realpath(await mkdtemp(join(process.env.HAARNESS_E2E_DIR ?? tmpdir(), "haarness-background-commands-")));
    project = join(root, "project");
    await mkdir(join(project, ".haarness"), { recursive: true });
    // A long foreground command moves after 8 s here (a minute by
    // default), and a stop steps from interrupt to kill in half a second.
    await writeFile(
      join(project, ".haarness/config.toml"),
      '[defaults]\nprovider="echo"\nmodel="echo-1"\n[permissions]\nbash="allow"\n[lsp]\nenabled=false\n' +
        "[execution]\nshell_move_after_ms=8000\nshell_stop_grace_ms=500\n",
    );
    if (shots) await mkdir(shots, { recursive: true });
    await launch(binary);
    // Another client on the same server, as a script or the phone would be.
    const endpoint = JSON.parse(
      execFileSync(binary, ["--workspace", project, "--trust-project", "desktop"], {
        env,
        input: JSON.stringify({ action: "connect" }),
        encoding: "utf8",
        timeout: 120000,
      }),
    );
    client = new Haarness({ baseUrl: endpoint.url, token: endpoint.token });
  });
  test.afterAll(async () => close(binary));

  test("a command in the background shows its badge, time and live output, and stops from its row", async () => {
    test.setTimeout(120000);
    // It ignores the interrupt and the terminate, so it reads Stopping…
    // until the kill.
    const line = "trap '' INT TERM; printf 'ready\\n'; i=0; while [ $i -lt 300 ]; do i=$((i+1)); printf 'tick %s\\n' $i; sleep 1; done";
    await send(bash(line, true));
    await idle();
    const thread = await shownChat();
    const command = row("trap");
    await expect(command.locator(".tool-chip", { hasText: "In background" })).toBeVisible();
    await expect(command).toHaveAttribute("data-status", "running");
    await expect(command.locator(".tool-state")).toHaveText(/^\d+s$/);
    const first = await command.locator(".tool-state").textContent();
    await expect(command.locator(".tool-state")).not.toHaveText(first!, { timeout: 5000 });
    // The reply is done: the title bar counts it and the sidebar rings it.
    await expect(pill()).toHaveText("1 in background");
    const sidebarRow = page.locator(".sidebar-lists .session-row").filter({ hasText: /trap/ }).first();
    await expect(sidebarRow.locator(".session-status")).toHaveClass(/\bbackground\b/);
    await expect(sidebarRow.locator(".session-status")).not.toHaveClass(/\brunning\b/);
    // Opened, it follows what the command prints; the runtime's note to the
    // model never shows.
    await command.locator("summary").click();
    await expect(command.locator(".tool-output")).toContainText("ready");
    await expect(command.locator(".tool-output")).toContainText("tick 4", { timeout: 10000 });
    await expect(command).not.toContainText("runs in the background");
    await pictures("background-row", step("trap"));
    await pictures("background-titlebar", page.locator(".titlebar"));
    await pictures("background-sidebar-ring", page.locator(".sidebar"));
    // Still running lists it, its output a click away.
    await pill().click();
    const panel = page.getByRole("region", { name: "Still running" });
    await expect(panel.locator("details.tool-call")).toHaveCount(1);
    await expect(pill()).toHaveAttribute("aria-pressed", "true");
    await panel.locator("details.tool-call summary").click();
    await expect(panel.locator(".tool-output")).toContainText("tick");
    await pictures("still-running", page);
    // Stop: Stopping… until it has been killed, then Stopped, its output kept.
    await recordStates();
    const before = (await command.locator(".tool-output").textContent())!;
    await step("trap").getByRole("button", { name: "Stop this command" }).click();
    await expect(command.locator(".tool-state")).toHaveText("Stopped", { timeout: 15000 });
    expect(await page.evaluate(() => (window as any).__states)).toContain("Stopping…");
    expect((await command.locator(".tool-output").textContent())!.length).toBeGreaterThanOrEqual(before.length);
    await expect(command.locator(".tool-output")).toContainText("tick");
    await expect(step("trap").getByRole("button", { name: "Stop this command" })).toHaveCount(0);
    await expect(pill()).toHaveCount(0);
    await expect(sidebarRow.locator(".session-status")).not.toHaveClass(/\bbackground\b/);
    // Still running keeps it a while, greyed, and has nothing else to show.
    await expect(panel.locator(".background-item.ended")).toHaveCount(1);
    // The agent is told in a hidden message, which starts a reply of its own.
    await expect.poll(async () => (await commands(thread))[0]?.state).toBe("stopped");
    await expect
      .poll(async () => ((await api("listItems", thread)) as any[]).filter((item) => item.data.type === "assistant_message").length, { timeout: 30000 })
      .toBe(2);
    await idle();
    const items: any[] = await api("listItems", thread);
    expect(items.some((item) => item.data.type === "user_message" && item.data.synthetic && item.data.text.startsWith("<command "))).toBe(true);
    // Only the person's own message is a bubble; the agent's reply to the
    // notice follows as any reply does.
    await expect(page.locator(".transcript .message.user")).toHaveCount(1);
    await expect(page.locator(".transcript .message.assistant")).toHaveCount(2);
    await pictures("background-stopped", step("trap"));
    // The checkpoint rows of the command never show as rows of their own.
    await expect(page.locator(".transcript .message.step")).toHaveCount(1);
  });

  test("two commands count up and down; one exits 2 and another stopped through the SDK reads Stopped", async () => {
    test.setTimeout(120000);
    await newChat();
    await send(bash("printf 'checking\\n'; sleep 5; exit 2", true));
    await idle();
    const thread = await shownChat();
    await expect(pill()).toHaveText("1 in background");
    await send(bash("printf 'serving\\n'; sleep 120", true));
    await idle();
    await expect(pill()).toHaveText("2 in background");
    // The first ends with an exit code after its badge, and the count goes down.
    const failed = row("exit 2");
    await expect(failed.locator(".tool-exit")).toHaveText("exit 2", { timeout: 15000 });
    await expect(failed).toHaveAttribute("data-status", "failed");
    await expect(pill()).toHaveText("1 in background");
    await pictures("background-exit-2", step("exit 2"));
    // Another client stops the second: this window says so too.
    const serving = (await commands(thread)).find((command) => command.command.includes("serving"))!;
    await client.stopShellCommand(thread, serving.id);
    await expect(row("serving").locator(".tool-state")).toHaveText("Stopped", { timeout: 15000 });
    await expect(pill()).toHaveCount(0);
    await idle();
  });

  test("a foreground command goes to the background from its row and the reply ends without a notice", async () => {
    test.setTimeout(120000);
    await newChat();
    await send(bash("printf 'serving on 4321\\n'; sleep 120"));
    const command = row("serving on 4321");
    await expect(command).toHaveAttribute("data-status", "running");
    // The server reports it about two seconds in; then it offers both.
    const move = step("serving on 4321").getByRole("button", { name: "Send to background" });
    await expect(move).toBeVisible({ timeout: 6000 });
    await expect(step("serving on 4321").getByRole("button", { name: "Stop this command" })).toBeVisible();
    // Opened while it runs, it shows its output rather than waiting for it.
    await command.locator("summary").click();
    await expect(command.locator(".tool-output")).toContainText("serving on 4321");
    await pictures("foreground-row", step("serving on 4321"));
    await command.locator("summary").click();
    await move.click();
    await idle();
    const thread = await shownChat();
    await expect(command.locator(".tool-chip", { hasText: "In background" })).toBeVisible();
    await expect(move).toHaveCount(0);
    const info = (await commands(thread))[0];
    expect([info.background, info.movedBy, info.state]).toEqual([true, "person", "running"]);
    // The reply ended; nothing was told to the chat while the command runs.
    const items: any[] = await api("listItems", thread);
    expect(items.some((item) => item.data.type === "user_message" && item.data.synthetic)).toBe(false);
    await expect(page.locator(".transcript .message.user")).toHaveCount(1);
    // A late Stop of the reply (resume, with no reply running) leaves it running.
    await api("interruptThreadTree", thread, { resume: true });
    await page.waitForTimeout(1500);
    expect((await commands(thread))[0].state).toBe("running");
    await expect(command).toHaveAttribute("data-status", "running");
    await step("serving on 4321").getByRole("button", { name: "Stop this command" }).click();
    await expect(command.locator(".tool-state")).toHaveText("Stopped", { timeout: 15000 });
    await idle();
  });

  test("the agent's reading and stopping of a command name the command, never its id", async () => {
    test.setTimeout(60000);
    await newChat();
    await send(bash("printf 'watching files\\n'; sleep 120", true));
    await expect(row("watching files").locator(".tool-chip")).toHaveText("In background");
    await idle();
    const thread = await shownChat();
    const id = (await commands(thread))[0].id as string;
    await send("!tool bash_output " + JSON.stringify({ id }));
    await expect(page.locator(".transcript details.tool-call")).toHaveCount(2);
    await idle();
    await send("!tool bash_stop " + JSON.stringify({ id }));
    await expect(page.locator(".transcript details.tool-call")).toHaveCount(3);
    await idle();
    const rows = page.locator(".transcript details.tool-call > summary");
    await expect(rows.nth(1)).toHaveText(/^Checked a command\s*printf 'watching files\\n'; sleep 120$/);
    await expect(rows.nth(2)).toHaveText(/^Stopped a command\s*printf 'watching files\\n'; sleep 120$/);
    for (const text of await rows.allTextContents()) expect(text).not.toContain(id);
    await expect(page.locator(".transcript details.tool-call").first().locator(".tool-state")).toHaveText("Stopped");
    await pictures("agent-command-steps", page.locator(".transcript"));
  });

  test("a command a batch started in the background shows under its batch row, stops there and is listed", async () => {
    test.setTimeout(90000);
    await newChat();
    await send(
      "!tool batch " +
        JSON.stringify({
          tool_calls: [
            { tool: "list", parameters: { path: "." } },
            { tool: "bash", parameters: { command: "printf 'batch step\\n'; sleep 120", run_in_background: true } },
          ],
        }),
    );
    const batchRow = page.locator(".transcript details.tool-batch").last();
    await expect(batchRow.locator(":scope > summary .tool-chip")).toHaveText("In background");
    await idle();
    await expect(pill()).toHaveText("1 in background");
    await batchRow.locator(":scope > summary").click();
    const stepRow = batchRow.locator("details.tool-child").filter({ hasText: "batch step" });
    await expect(stepRow.locator(".tool-chip")).toHaveText("In background");
    await expect(stepRow.locator(".tool-state")).toHaveText(/^\d+s$/);
    await stepRow.locator("summary").click();
    await expect(stepRow.locator(".tool-output")).toContainText("batch step");
    await pictures("batch-step", page.locator(".transcript .message.step").filter({ has: page.locator("details.tool-batch") }).last());
    if ((await pill().getAttribute("aria-pressed")) !== "true") await pill().click();
    const panel = page.getByRole("region", { name: "Still running" });
    await expect(panel.locator("details.tool-call").filter({ hasText: "batch step" })).toHaveCount(1);
    await batchRow.locator(".tool-step").filter({ hasText: "batch step" }).getByRole("button", { name: "Stop this command" }).click();
    await expect(stepRow.locator(".tool-state")).toHaveText("Stopped", { timeout: 15000 });
    await expect(batchRow.locator(":scope > summary .tool-chip")).toHaveCount(0);
    await expect(pill()).toHaveCount(0);
    await idle();
  });

  test("a fork's copy of a running command is not live, and steps name a command the server forgot", async () => {
    test.setTimeout(120000);
    await newChat();
    await send(bash("printf 'original server\\n'; sleep 120", true));
    await expect(row("original server").locator(".tool-chip")).toHaveText("In background");
    await idle();
    const original = await shownChat();
    const running = (await commands(original))[0];
    // Forked while it runs: the fork has the row, not the command.
    const fork = await api("forkThread", original, { title: "Fork of the server chat" });
    await page.locator(".sidebar-lists").getByRole("button", { name: "Fork of the server chat", exact: true }).click();
    const copy = row("original server");
    await expect(copy.locator(".tool-chip")).toHaveText("In background");
    await expect.poll(async () => (await commands(fork.id)).length).toBe(0);
    await page.waitForTimeout(1500);
    await expect(copy).not.toHaveAttribute("data-status", "running");
    await expect(copy.locator(".tool-state")).toHaveCount(0);
    await expect(step("original server").getByRole("button", { name: "Stop this command" })).toHaveCount(0);
    await expect(pill()).toHaveCount(0);
    await pictures("fork-copy", step("original server"));
    // The original still runs, and its row says so.
    const title = ((await api("listThreads", {})) as any[]).find((thread) => thread.id === original).title;
    await page.locator(".sidebar-lists").getByTitle(title, { exact: true }).first().click();
    await expect(row("original server")).toHaveAttribute("data-status", "running");
    await expect(pill()).toHaveText("1 in background");
    // The agent reads and stops it; then the server is asked afresh (a
    // reload) from a fork of all that, which the server never ran.
    await send("!tool bash_output " + JSON.stringify({ id: running.id }));
    await expect(page.locator(".transcript details.tool-call")).toHaveCount(2);
    await idle();
    await send("!tool bash_stop " + JSON.stringify({ id: running.id }));
    await expect(page.locator(".transcript details.tool-call")).toHaveCount(3);
    await idle();
    await api("forkThread", original, { title: "Fork after the stop" });
    await page.locator(".sidebar-lists").getByRole("button", { name: "Fork after the stop", exact: true }).click();
    await expect(page.locator(".transcript details.tool-call")).toHaveCount(3);
    await page.reload();
    await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 60000 });
    const rows = page.locator(".transcript details.tool-call > summary");
    await expect(rows).toHaveCount(3);
    await expect(rows.nth(1)).toHaveText(/^Checked a command\s*printf 'original server\\n'; sleep 120$/);
    await expect(rows.nth(2)).toHaveText(/^Stopped a command\s*printf 'original server\\n'; sleep 120$/);
    for (const text of await rows.allTextContents()) expect(text).not.toContain(running.id);
  });

  test("File → Send to background moves a running foreground subagent and the reply carries on", async () => {
    test.setTimeout(120000);
    await newChat();
    await send(
      "!tool task " +
        JSON.stringify({ description: "Survey the routes", agent: "general", prompt: bash("printf 'surveying\\n'; sleep 14") }),
    );
    const task = page.locator(".transcript .agent-task-row").last();
    const send2 = task.getByRole("button", { name: /^Send to background/ });
    await expect(send2).toBeVisible({ timeout: 15000 });
    await pictures("subagent-send", task);
    await app.evaluate(({ BrowserWindow, Menu }) => {
      const file = Menu.getApplicationMenu()!.items.find((item) => item.label === "File")!;
      const item = file.submenu!.items.find((entry) => entry.label === "Send to background")!;
      item.click(undefined, BrowserWindow.getAllWindows()[0]);
    });
    await expect(task.locator(".agent-task")).toHaveAttribute("data-status", "background");
    await expect(send2).toHaveCount(0);
    // The reply ends while the subagent goes on.
    const parent = await shownChat();
    await expect
      .poll(async () => ((await api("activeTurns")) as any[]).filter((turn) => turn.threadId === parent).length)
      .toBe(0);
    expect(((await api("activeTurns")) as any[]).length).toBe(1);
    await expect(pill()).toHaveText("1 in background");
    // Still running stayed open from before; the pill says so.
    await expect(pill()).toHaveAttribute("aria-pressed", "true");
    const panel = page.getByRole("region", { name: "Still running" });
    await expect(panel.locator(".agent-task")).toHaveCount(1);
    await pictures("still-running-subagent", page.locator(".side-panel"));
    // It finishes on its own: the count goes and it stays listed, greyed.
    await expect(pill()).toHaveCount(0, { timeout: 40000 });
    await expect(panel.locator(".background-item.ended .agent-task")).toHaveCount(1);
    await idle();
  });

  test("a long foreground command moves to the background by itself and says when", async () => {
    test.setTimeout(120000);
    await newChat();
    await send(bash("printf 'building\\n'; sleep 30"));
    const command = row("building");
    const badge = command.locator(".tool-chip", { hasText: "In background" });
    await expect(badge).toBeVisible({ timeout: 20000 });
    await expect(badge).toHaveAttribute("title", /^Sent to the background after (8|9)s$/);
    await idle();
    await expect(pill()).toHaveText("1 in background");
    // Arabic, right to left, and a narrow window.
    await page.evaluate(() => (window as any).haarness.preferences({ language: "ar" }));
    await expect(page.locator("html")).toHaveAttribute("dir", "rtl");
    await expect(command.locator(".tool-chip")).toHaveText("في الخلفية");
    await pictures("background-rtl", page);
    await page.evaluate(() => (window as any).haarness.preferences({ language: "en" }));
    await app.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows()[0].setSize(760, 760));
    await page.waitForTimeout(400);
    await pictures("background-narrow", page);
    await app.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows()[0].setSize(1280, 860));
    await step("building").getByRole("button", { name: "Stop this command" }).click();
    await expect(command.locator(".tool-state")).toHaveText("Stopped", { timeout: 15000 });
    await idle();
    expect(errors).toEqual([]);
  });
});

test.describe("a server from before commands in the background", () => {
  test.skip(!older, "HAARNESS_OLDER_BIN names no older server");
  test.beforeAll(async () => {
    test.setTimeout(150000);
    root = await realpath(await mkdtemp(join(process.env.HAARNESS_E2E_DIR ?? tmpdir(), "haarness-background-older-")));
    project = join(root, "project");
    await mkdir(join(project, ".haarness"), { recursive: true });
    await writeFile(
      join(project, ".haarness/config.toml"),
      '[defaults]\nprovider="echo"\nmodel="echo-1"\n[permissions]\nbash="allow"\n[lsp]\nenabled=false\n',
    );
    await launch(older!);
  });
  test.afterAll(async () => close(older!));

  test("its command rows are as they always were", async () => {
    test.setTimeout(60000);
    const info = await api("info");
    expect(info.capabilities).not.toContain("background-commands.v1");
    await send(bash("printf 'old server\\n'; sleep 6"));
    const command = row("old server");
    await expect(command).toHaveAttribute("data-status", "running");
    await command.locator("summary").click();
    await page.waitForTimeout(3000);
    await expect(command.locator(".tool-empty")).toHaveText("Waiting for output…");
    await expect(step("old server").locator(".tool-controls")).toHaveCount(0);
    await expect(pill()).toHaveCount(0);
    await idle();
    await expect(command).toHaveAttribute("data-status", "done");
    await expect(command.locator(".tool-output")).toContainText("old server");
    await expect(command.locator(".tool-chip")).toHaveCount(0);
    expect(errors).toEqual([]);
  });
});
