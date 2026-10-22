import { Haarness } from "@haarness/sdk";
import { test, expect, _electron as electron, type ElectronApplication, type Page } from "@playwright/test";
import { mkdir, mkdtemp, realpath, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";

// The /btw side chat in real Electron against a project daemon on the echo
// provider, which answers a side question with "Echo: <question>" and names
// the side chat's question before it, so follow-ups prove they carried their
// thread. Only the native folder picker is given a deterministic answer.
// HAARNESS_SIDE_CHAT_SCREENSHOTS=<folder> keeps the light and dark shots there.
const binary = resolve(process.env.HAARNESS_BIN ?? "../../target/debug/haarness");
const shots = process.env.HAARNESS_SIDE_CHAT_SCREENSHOTS;
let app: ElectronApplication;
let page: Page;
let temporary: string;
let workspace: string;
let env: Record<string, string>;
let client: Haarness;
let errors: string[] = [];

const panel = () => page.locator(".side-chat-panel");
const column = () => page.locator(".side-chat-column");
const sidePanel = () => page.getByRole("complementary", { name: "Project side panel", exact: true });
const chatPane = () => page.locator(".chat-pane");
const box = () => panel().getByRole("textbox", { name: "Ask a side question", exact: true });
const threadTabs = () => panel().getByRole("tab");
const composer = () => page.getByRole("textbox", { name: "Message", exact: true });
const mod = process.platform === "darwin" ? "Meta" : "Control";

test.beforeAll(async () => {
  temporary = await realpath(await mkdtemp(join(tmpdir(), "haarness-side-chat-")));
  workspace = join(temporary, "workspace");
  await mkdir(join(workspace, ".haarness"), { recursive: true });
  await writeFile(join(workspace, ".haarness", "config.toml"), '[defaults]\nprovider="echo"\nmodel="echo-1"\n');
  env = {
    ...process.env,
    HAARNESS_BIN: binary,
    HAARNESS_HOME: join(temporary, "home"),
    HAARNESS_DATA_DIR: join(temporary, "data"),
    HAARNESS_DESKTOP_DATA: join(temporary, "desktop"),
    HAARNESS_TEST_PROVIDER: "1",
    HAARNESS_DESKTOP_TEST: "1",
  } as Record<string, string>;
  delete env.ELECTRON_RUN_AS_NODE;
  delete env.ELECTRON_RENDERER_URL;
  app = await electron.launch({ args: ["."], env, timeout: 60000 });
  page = await app.firstWindow();
  page.on("pageerror", (error) => errors.push(error.message));
  await app.evaluate(({ dialog, BrowserWindow }, directory) => {
    dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [directory] });
    BrowserWindow.getAllWindows()[0]?.setSize(1440, 900);
  }, workspace);
  await page.getByRole("region", { name: "Project home" }).getByRole("button", { name: "Open project", exact: true }).click();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 60000 });
  const connection = JSON.parse(execFileSync(binary, ["--workspace", workspace, "--trust-project", "desktop"], {
    env, input: JSON.stringify({ action: "connect" }), encoding: "utf8", timeout: 60000,
  }));
  client = new Haarness({ baseUrl: connection.url, token: connection.token });
});

test.afterAll(async () => {
  test.setTimeout(180000);
  client?.close();
  if (app) {
    const child = app.process();
    const exited = new Promise<void>((resolve) => {
      if (child.exitCode !== null) resolve();
      else child.once("exit", () => resolve());
    });
    const within = (ms: number) => Promise.race([exited, new Promise<void>((resolve) => setTimeout(resolve, ms))]);
    await Promise.race([
      app.evaluate(({ BrowserWindow }) => { for (const window of BrowserWindow.getAllWindows()) window.destroy(); }).catch(() => {}),
      within(10000),
    ]);
    void app.close().catch(() => {});
    await within(15000);
    if (child.exitCode === null) child.kill("SIGTERM");
    await within(5000);
    if (child.exitCode === null) child.kill("SIGKILL");
  }
  try { execFileSync(binary, ["--workspace", workspace, "--trust-project", "serve", "--stop"], { env, stdio: "ignore", timeout: 60000 }); } catch {}
  if (temporary) await rm(temporary, { recursive: true, force: true, maxRetries: 10 }).catch(() => {});
});

test.beforeEach(() => { errors = []; });

async function send(text: string) {
  await composer().fill(text);
  await page.getByRole("button", { name: "Send message", exact: true }).click();
}

async function ask(text: string) {
  await box().fill(text);
  await box().press("Enter");
}

async function shoot(name: string) {
  if (!shots) return;
  await mkdir(shots, { recursive: true });
  for (const theme of ["light", "dark"] as const) {
    await page.evaluate((value) => window.haarness.preferences({ theme: value }), theme);
    await expect(page.locator("html")).toHaveAttribute("data-theme", theme);
    await page.mouse.move(0, 0);
    await page.waitForTimeout(150);
    await page.screenshot({ path: join(shots, `${theme}-${name}.png`) });
    await panel().screenshot({ path: join(shots, `${theme}-${name}-panel.png`) });
  }
  await page.evaluate(() => window.haarness.preferences({ theme: "light" }));
}

/** The chat whose first message was `text`, and its transcript rows. */
async function chatRows(text: string) {
  for (const thread of await client.listThreads()) {
    const items = await client.listItems(thread.id);
    if (items.some((item) => item.data.type === "user_message" && item.data.text === text))
      return { id: thread.id, rows: items.length };
  }
  throw new Error("No chat began with " + text);
}

test("/btw opens a side chat beside the chat that streams answers and keeps follow-ups in a thread", async () => {
  test.setTimeout(240000);
  await page.getByRole("button", { name: "New chat", exact: true }).click();
  await send("Plan the release notes");
  await expect(page.locator(".transcript")).toContainText("Echo: Plan the release notes");
  const before = await chatRows("Plan the release notes");

  // /btw in the composer asks in a column of its own beside the chat, and the keyboard follows.
  await send("/btw what changed in the plan?");
  await expect(panel()).toBeVisible();
  await expect(chatPane()).toBeVisible();
  await expect(sidePanel()).toBeHidden();
  await expect(panel()).toContainText("Echo: what changed in the plan?");
  await expect(threadTabs()).toHaveText(["what changed in the plan?"]);
  await expect(box()).toBeFocused();
  await expect(box()).toHaveAttribute("placeholder", "Ask a follow-up");
  await expect(composer()).toHaveValue("");
  await expect(page.locator(".transcript")).not.toContainText("what changed in the plan?");

  // A follow-up carries the thread.
  await ask("and why?");
  await expect(panel()).toContainText("Echo: and why? (after: what changed in the plan?)");
  await expect(panel().locator(".side-chat-turn")).toHaveCount(2);
  await shoot("2-thread");

  // Answers stream: the first piece shows while Stop stands in for Send.
  await ask("::mdslow");
  await expect(panel().getByText("Live markdown")).toBeVisible();
  await expect(panel().getByRole("button", { name: "Stop", exact: true })).toBeVisible();
  await expect(panel()).toContainText("streams while working");
  await expect(panel().getByRole("button", { name: "Send", exact: true })).toBeDisabled();

  // Stop, Retry, and Esc in the box stopping an answer.
  await ask("!linger 9000 take your time");
  await expect(panel().getByText("Thinking…")).toBeVisible();
  await shoot("3-thinking");
  await panel().getByRole("button", { name: "Stop", exact: true }).click();
  await expect(panel().locator(".side-chat-status").last()).toContainText("Stopped");
  await panel().getByRole("button", { name: "Retry", exact: true }).click();
  await expect(panel().getByText("Thinking…")).toBeVisible();
  await box().press("Escape");
  await expect(panel().locator(".side-chat-status").last()).toContainText("Stopped");
  await expect(panel().locator(".side-chat-turn")).toHaveCount(4);
  // With nothing being written, Esc goes back to the chat's message box.
  await box().press("Escape");
  await expect(composer()).toBeFocused();

  // A finished answer offers Copy, a stopped one Retry instead.
  await panel().locator(".side-chat-turn").first().hover();
  await expect(panel().locator(".side-chat-turn").first().getByRole("button", { name: "Copy message", exact: true })).toBeVisible();
  await expect(panel().locator(".side-chat-turn").last().getByRole("button", { name: "Copy message", exact: true })).toHaveCount(0);

  // The side panel opens beside it: conversation, side chat and Files all on screen.
  await page.getByRole("button", { name: "Show side panel", exact: true }).click();
  await sidePanel().getByRole("button", { name: "Open Files", exact: true }).click();
  await expect(sidePanel()).toBeVisible();
  await expect(column()).toBeVisible();
  await expect(chatPane()).toBeVisible();
  const [chatBox, sideBox, dockBox] = await Promise.all([chatPane(), column(), sidePanel()].map((item) => item.boundingBox()));
  expect(chatBox!.x + chatBox!.width).toBeLessThanOrEqual(sideBox!.x + 1);
  expect(sideBox!.x + sideBox!.width).toBeLessThanOrEqual(dockBox!.x + 1);
  expect(chatBox!.width).toBeGreaterThanOrEqual(479);
  await shoot("4-beside-files");

  // Its width follows the divider and is kept.
  const divider = page.getByRole("separator", { name: "Resize side chat", exact: true });
  await divider.focus();
  const width = Number(await divider.getAttribute("aria-valuenow"));
  await divider.press("ArrowRight");
  await expect(divider).toHaveAttribute("aria-valuenow", String(width - 20));

  // Where all three do not fit, the side chat and the side panel share one
  // place, and whichever was asked for last is in front: Files, then the side chat.
  await app.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows()[0]?.setSize(1250, 860));
  await expect(sidePanel()).toBeVisible();
  await expect(column()).toBeHidden();
  await composer().click();
  await page.keyboard.press(`${mod}+Shift+J`);
  await expect(column()).toBeVisible();
  await expect(sidePanel()).toBeHidden();
  await expect(chatPane()).toBeVisible();
  await expect(box()).toBeFocused();
  await shoot("5-instead-of-files");
  await page.getByRole("button", { name: "Show side panel", exact: true }).click();
  await expect(sidePanel()).toBeVisible();
  await expect(column()).toBeHidden();
  await page.keyboard.press(`${mod}+Shift+J`);
  await expect(column()).toBeVisible();
  await expect(sidePanel()).toBeHidden();
  // In a narrow window it takes the whole workbench.
  await app.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows()[0]?.setSize(1000, 800));
  await expect(page.locator(".workbench")).toHaveClass(/side-over/);
  await expect(column()).toBeVisible();
  await app.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows()[0]?.setSize(1440, 900));
  await expect(page.locator(".workbench")).not.toHaveClass(/side-over/);
  await expect(sidePanel()).toBeVisible();
  // The side panel goes, the side chat stays.
  await page.getByRole("button", { name: "Hide side panel", exact: true }).click();
  await expect(sidePanel()).toBeHidden();
  await expect(column()).toBeVisible();

  // The far-end × puts the side chat away, back to the message box.
  await panel().getByRole("button", { name: "Hide side chat", exact: true }).click();
  await expect(column()).toBeHidden();
  await expect(composer()).toBeFocused();
  await send("/btw");
  await expect(column()).toBeVisible();
  await expect(box()).toBeFocused();

  // The chat itself never changed.
  expect((await client.listItems(before.id)).length).toBe(before.rows);
  expect(errors).toEqual([]);
});

test("side chats keep their threads per chat, across a reload, and come and go from the keyboard", async () => {
  test.setTimeout(240000);
  // Another thread from +, named after its first question.
  await panel().getByRole("button", { name: "New side chat", exact: true }).click();
  await expect(threadTabs()).toHaveCount(2);
  await expect(panel().getByRole("heading", { name: "Ask a side question" })).toBeVisible();
  await expect(box()).toBeFocused();
  await shoot("1-empty");
  await ask("Summarize the last answer in one sentence");
  await expect(panel()).toContainText("Echo: Summarize the last answer in one sentence");
  await expect(threadTabs().nth(1)).toHaveText("Summarize the last answer in on…");

  // A /btw while that thread is in front asks there; the first thread is untouched.
  await composer().click();
  await send("/btw second question here");
  await expect(panel()).toContainText("Echo: second question here (after: Summarize the last answer in one sentence)");

  // Threads and their answers survive a reload; a draft too.
  await box().fill("a draft for later");
  await page.waitForTimeout(1000);
  await page.reload();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 60000 });
  await expect(panel()).toBeVisible();
  await expect(threadTabs()).toHaveCount(2);
  await expect(panel()).toContainText("Echo: second question here");
  await expect(box()).toHaveValue("a draft for later");
  await threadTabs().first().click();
  await expect(panel()).toContainText("Echo: and why? (after: what changed in the plan?)");
  await expect(panel().locator(".side-chat-status").last()).toContainText("Stopped");

  // Another chat has side chats of its own.
  await page.getByRole("button", { name: "New chat tab", exact: true }).click();
  await send("A different chat");
  await expect(page.locator(".transcript")).toContainText("Echo: A different chat");
  await composer().click();
  await page.keyboard.press(`${mod}+Shift+J`);
  await expect(box()).toBeFocused();
  await expect(threadTabs()).toHaveCount(1);
  await expect(panel().getByRole("heading", { name: "Ask a side question" })).toBeVisible();
  // The same key from the side chat puts it away and goes back to the message box.
  await page.keyboard.press(`${mod}+Shift+J`);
  await expect(panel()).toBeHidden();
  await expect(composer()).toBeFocused();

  // The session menu opens it too; the new-chat screen has nothing to ask about.
  await page.getByRole("button", { name: "Chat actions", exact: true }).click();
  await page.getByRole("menuitem", { name: "Open side chat", exact: true }).click();
  await expect(column()).toBeVisible();
  await page.getByRole("button", { name: "New chat", exact: true }).click();
  await expect(column()).toBeHidden();

  // Back on the first chat, its threads are there; the palette starts another.
  await page.getByRole("region", { name: "Project workspace", exact: true }).getByRole("button", { name: /Plan the release notes/ }).first().click();
  await expect(page.locator(".transcript")).toContainText("Echo: Plan the release notes");
  await page.getByRole("button", { name: "Search", exact: true }).click();
  const search = page.getByRole("combobox", { name: "Search Haarness", exact: true });
  await search.fill("New side chat");
  await expect(page.getByRole("option", { name: /New side chat/ })).toBeVisible();
  await search.press("Enter");
  await expect(panel()).toBeVisible();
  await expect(threadTabs()).toHaveCount(3);
  await expect(box()).toBeFocused();

  // ⌘W in the side chat closes its thread in front, not the window.
  await box().press(`${mod}+W`);
  await expect(threadTabs()).toHaveCount(2);
  expect(await app.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows().length)).toBe(1);
  // Closing a tab discards its thread; closing the last puts the panel away.
  for (let count = 2; count > 0; count--) {
    await panel().getByRole("button", { name: /^Close / }).last().click();
    if (count > 1) await expect(threadTabs()).toHaveCount(count - 1);
  }
  await expect(panel()).toBeHidden();
  expect(errors).toEqual([]);
});

test("a /btw on a chat not started yet says how to begin, and leaves the draft", async () => {
  await page.getByRole("button", { name: "New chat", exact: true }).click();
  await composer().fill("/btw anything?");
  await composer().press("Enter");
  await expect(page.getByText("Send a first message, then ask side questions with /btw.")).toBeVisible();
  await expect(composer()).toHaveValue("/btw anything?");
  await expect(panel()).toBeHidden();
  expect(errors).toEqual([]);
});

test("answers keep streaming across chats and page loads, and the side chat shares its place sensibly", async () => {
  test.setTimeout(300000);
  await page.getByRole("button", { name: "New chat", exact: true }).click();
  await send("Another place");
  await expect(page.locator(".transcript")).toContainText("Echo: Another place");
  await page.getByRole("button", { name: "New chat", exact: true }).click();
  await send("Shared place");
  await expect(page.locator(".transcript")).toContainText("Echo: Shared place");

  // An answer keeps streaming while another chat is shown and while frames
  // in the page load (a preview, an MCP app): only a reload ends it.
  await send("/btw !linger 4000 across chats");
  await expect(panel().getByText("Thinking…")).toBeVisible();
  const chats = page.getByRole("region", { name: "Project workspace", exact: true });
  await chats.getByRole("button", { name: /Another place/ }).first().click();
  await expect(page.locator(".transcript")).toContainText("Echo: Another place");
  await page.evaluate(() => {
    const frame = document.createElement("iframe");
    frame.id = "side-chat-probe";
    frame.style.display = "none";
    document.body.append(frame);
    frame.srcdoc = "<p>first</p>";
    setTimeout(() => (frame.srcdoc = "<p>second</p>"), 300);
  });
  await chats.getByRole("button", { name: /Shared place/ }).first().click();
  await expect(panel()).toContainText("Echo: across chats", { timeout: 30000 });
  await expect(panel().locator(".side-chat-status")).toHaveCount(0);
  await page.evaluate(() => document.getElementById("side-chat-probe")?.remove());

  // A window too narrow for the side chat beside the side panel: they share a place.
  await app.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows()[0]?.setSize(1250, 860));
  await expect(column()).toBeVisible();
  // The agent's preview waits behind the side chat; the one the person opens comes forward.
  const page_ = '<!doctype html><html><body><h1>Side preview</h1></body></html>';
  await send("!tool write " + JSON.stringify({ path: "side.html", content: page_ }));
  await expect(page.locator(".transcript")).toContainText("Tool write");
  await send('!tool preview {"filePath":"side.html","title":"Side preview"}');
  await ask("!linger 2500 while it previews");
  const banner = page.locator(".preview-banner").last();
  await expect(banner).toContainText("Side preview", { timeout: 30000 });
  await expect(column()).toBeVisible();
  await expect(sidePanel()).toBeHidden();
  await expect(panel()).toContainText("Echo: while it previews", { timeout: 30000 });
  await banner.getByRole("button", { name: "Open preview", exact: true }).click();
  await expect(sidePanel()).toBeVisible();
  await expect(column()).toBeHidden();

  // After a reload, the one in front stays in front.
  await composer().click();
  await page.keyboard.press(`${mod}+Shift+J`);
  await expect(column()).toBeVisible();
  await page.waitForTimeout(500);
  await page.reload();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 60000 });
  await expect(column()).toBeVisible();
  await expect(sidePanel()).toBeHidden();

  // Not behind an open dialog.
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  await expect(page.locator(".settings-dialog")).toBeVisible();
  await page.keyboard.press(`${mod}+Shift+J`);
  await expect(column()).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.locator(".settings-dialog")).toBeHidden();

  // In a narrow window it covers the workspace, and Esc in its idle box puts it away.
  await app.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows()[0]?.setSize(1000, 800));
  await expect(page.locator(".workbench")).toHaveClass(/side-over/);
  await box().focus();
  await box().press("Escape");
  await expect(column()).toBeHidden();
  // The side panel it covered comes back, and expanded it still takes the whole workspace.
  await expect(sidePanel()).toBeVisible();
  await page.getByRole("button", { name: "Expand side panel", exact: true }).click();
  const workbench = await page.locator(".workbench").boundingBox();
  const expanded = await sidePanel().boundingBox();
  expect(expanded!.width).toBeGreaterThan(workbench!.width - 4);
  await page.getByRole("button", { name: "Restore side panel", exact: true }).click();
  await page.getByRole("button", { name: "Hide side panel", exact: true }).click();
  await app.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows()[0]?.setSize(1440, 900));
  expect(errors).toEqual([]);
});
