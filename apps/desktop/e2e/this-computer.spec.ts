import { Haarness, type Thread } from "@haarness/sdk";
import {
  test,
  expect,
  _electron as electron,
  type ElectronApplication,
  type Page,
} from "@playwright/test";
import { mkdtemp, mkdir, readFile, realpath, rename, rm, stat, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";
import { chatHomeFolder, stopChatHome } from "./fixtures/chat-home";

// Chats without a project: on this computer a first launch is a ready chat,
// each such chat works in a folder of its own under one chat home, and one
// daemon serves them all. Real Electron, IPC, daemons and the echo provider.

const binary = resolve(process.env.HAARNESS_BIN ?? "../../target/debug/haarness");
let app: ElectronApplication | undefined;
let page: Page;
let temporary: string;
let env: Record<string, string>;
let project: string;
let client: Haarness | undefined;

test.beforeEach(async () => {
  test.setTimeout(300000);
  temporary = await realpath(await mkdtemp(join(tmpdir(), "haarness-this-computer-")));
  project = join(temporary, "pocket-project");
  await mkdir(project);
  await mkdir(join(temporary, "desktop"));
  await mkdir(join(temporary, "home"));
  env = Object.fromEntries(
    Object.entries(process.env).filter((entry): entry is [string, string] => entry[1] !== undefined),
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
  delete env.ELECTRON_RENDERER_URL;
});

test.afterEach(async () => {
  client?.close();
  client = undefined;
  await app?.close().catch(() => {});
  app = undefined;
  try {
    execFileSync(binary, ["--workspace", project, "serve", "--stop"], { env, stdio: "ignore", timeout: 30000 });
  } catch {}
  stopChatHome(binary, env);
  await rm(temporary, { recursive: true, force: true });
});

async function launch() {
  app = await electron.launch({ args: ["."], env, timeout: 240000 });
  page = await app.firstWindow();
  // Notes whether the full-window loading mark is ever seen: it waits a
  // third of a second before it fades in, so a mark gone sooner never shows.
  await page.evaluate(() => {
    const look = () => {
      const mark = document.querySelector(".chat-loading");
      if (mark && Number(getComputedStyle(mark).opacity) > 0.05) (window as any).loadingMarkSeen = true;
      requestAnimationFrame(look);
    };
    look();
  });
  await app.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows()[0].setContentSize(1280, 820));
}
const loadingMarkSeen = () => page.evaluate(() => !!(window as any).loadingMarkSeen);

const ready = () =>
  expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 240000 });
const composer = () => page.getByRole("textbox", { name: "Message", exact: true });
const section = () => page.getByRole("region", { name: "This computer", exact: true });
const rows = () => section().locator(".session-row > button[aria-current]");
const newChatFolder = () => page.getByRole("button", { name: /^Folder for new chat: / });
const chatFolder = () => page.getByRole("button", { name: /^Chat folder: / });

/** The chat home's own server, as Desktop connected it (one daemon). */
function homeClient() {
  const home = join(temporary, "desktop", "Chat folders");
  const endpoint = JSON.parse(
    execFileSync(binary, ["--workspace", home, "--chat-home", "desktop"], {
      env,
      input: JSON.stringify({ action: "connect" }),
      encoding: "utf8",
      timeout: 240000,
    }),
  );
  return new Haarness({ baseUrl: endpoint.url, token: endpoint.token });
}

/** The servers running for the chat home (a daemon per workspace). */
function homeDaemons() {
  const home = join(temporary, "desktop", "Chat folders");
  return execFileSync("ps", ["-axww", "-o", "command="], { encoding: "utf8" })
    .split("\n")
    .filter((line) => line.includes(home) && /\bserve\b/.test(line) && !/--stop/.test(line)).length;
}

/** A local date as the chat folders name their day. */
function today() {
  const now = new Date();
  const pad = (value: number) => String(value).padStart(2, "0");
  return `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}`;
}

async function send(text: string) {
  await composer().fill(text);
  await composer().press("Enter");
}

async function settled(thread: string) {
  await expect.poll(async () => (await client!.activeTurns()).some((turn) => turn.threadId === thread), { timeout: 60000 }).toBe(false);
}

test("chats without a project: a ready first launch, a folder of their own on one daemon, a project when chosen, kept across a relaunch", async () => {
  await launch();
  // A first launch is a ready message box that needs no folder, there while
  // the chat home's daemon starts, with no loading screen.
  await expect(composer()).toBeEnabled();
  await expect(newChatFolder()).toHaveAccessibleName("Folder for new chat: No folder");
  await expect(page.locator(".conversation-empty h2")).toHaveText("What should we work on?");
  // A message sent meanwhile (the daemon usually still starting) goes once
  // the chat home is open.
  await send('!tool bash {"command":"pwd"}');
  await ready();
  expect(await loadingMarkSeen()).toBe(false);
  await expect(page.getByRole("region", { name: "Project home" })).toHaveCount(0);
  await expect(page.getByLabel("Chat starters")).toHaveCount(0);
  // This computer alone: nothing to switch to.
  await expect(page.getByRole("button", { name: "Switch server", exact: true })).toHaveCount(0);
  await expect(section().getByRole("button", { name: "New chat in This computer", exact: true })).toBeVisible();

  // The first message gives the chat its folder, named by what it says, and
  // its tools run there.
  const home = await realpath(chatHomeFolder(join(temporary, "desktop")));
  await expect(rows()).toHaveCount(1);
  const first = join(home, today(), "tool-bash-command-pwd");
  await expect.poll(() => stat(first).then((value) => value.isDirectory()).catch(() => false)).toBe(true);
  client = homeClient();
  const threads = async () => (await client!.listThreads({ archived: true })) as (Thread & { directory?: string })[];
  const one = (await threads()).find((thread) => thread.directory === first);
  expect(one, "the chat reports its folder").toBeTruthy();
  await settled(one!.id);
  const ran = (await client.listItems(one!.id)).find((item) => item.data.type === "tool_result");
  expect(JSON.stringify(ran?.data)).toContain(first);
  await expect(chatFolder()).toHaveAccessibleName("Chat folder: tool-bash-command-pwd");
  await expect(page.locator(".titlebar-caption")).toHaveText("This computer");

  // A file it writes lands in that folder, and the Files panel shows it there.
  await send('!tool write {"path":"notes.txt","content":"written without a project"}');
  await expect.poll(() => readFile(join(first, "notes.txt"), "utf8").catch(() => "")).toBe("written without a project");
  await settled(one!.id);
  await page.getByRole("button", { name: "Show side panel", exact: true }).click();
  await page.getByRole("button", { name: "Open Files", exact: true }).click();
  const tree = page.locator(".file-tree");
  await expect(tree.getByRole("button", { name: "notes.txt", exact: true })).toBeVisible();
  // Only its own folder: not the day above it, nor another chat's.
  await expect(tree.getByRole("button", { name: today(), exact: true })).toHaveCount(0);
  await page.screenshot({ path: test.info().outputPath("chat-folder-files.png") });

  // A reply's relative image and link name the chat's own folder: the image
  // shows, the link opens there, and nothing outside it is read.
  await writeFile(
    join(first, "chart.png"),
    Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aQx8AAAAASUVORK5CYII=", "base64"),
  );
  // A link in the folder that leads out of it is not followed either.
  await writeFile(join(home, today(), "elsewhere.png"), await readFile(join(first, "chart.png")));
  await symlink(join(home, today(), "elsewhere.png"), join(first, "sneaky.png"));
  await send("See ![the chart](chart.png) and [the notes](notes.txt), not ![theirs](../other/chart.png) or ![sneaky](sneaky.png)");
  await settled(one!.id);
  const reply = page.locator(".transcript .markdown").filter({ hasText: "and the notes" }).last();
  await expect(reply.locator("img.project-image-loaded")).toHaveAttribute("data-project-path", "chart.png");
  await expect(reply.locator('.project-image[data-project-path="sneaky.png"]')).toBeVisible();
  await expect(reply.locator("img.project-image-loaded")).toHaveCount(1);
  await page.getByRole("button", { name: "Hide side panel", exact: true }).click();
  await reply.getByRole("link", { name: "the notes", exact: true }).click();
  await expect(page.locator(".file-tabs").getByRole("button", { name: "notes.txt", exact: true })).toBeVisible();

  // This computer's + starts another chat, in a folder of its own.
  await section().getByRole("button", { name: "New chat in This computer", exact: true }).click();
  await expect(newChatFolder()).toHaveAccessibleName("Folder for new chat: No folder");
  await expect(composer()).toBeFocused();
  // Before its first message it has no folder, and the Files panel says so.
  await expect(page.locator(".file-tree-note")).toBeVisible();
  await send("Second chat about Lisbon");
  await expect(rows()).toHaveCount(2);
  const second = join(home, today(), "second-chat-about-lisbon");
  await expect.poll(() => stat(second).then((value) => value.isDirectory()).catch(() => false)).toBe(true);
  expect(second).not.toBe(first);
  await expect(page.locator(".file-tree-note")).toHaveCount(0);
  // Both are served by one daemon.
  expect(homeDaemons()).toBe(1);
  const two = (await threads()).find((thread) => thread.directory === second)!;
  expect(two).toBeTruthy();
  await settled(two.id);
  // The keyboard reaches its rows as any chat rows: Focus sidebar lands on
  // the open chat's, and the arrows walk them.
  await page.keyboard.press("Control+Shift+L");
  await expect(page.locator(":focus")).toHaveAttribute("aria-current", "true");
  expect(await page.evaluate(() => !!document.activeElement?.closest(".computer-chats"))).toBe(true);
  await page.keyboard.press("ArrowDown");
  await expect(page.locator(":focus")).toHaveAttribute("aria-current", "false");
  expect(await page.evaluate(() => !!document.activeElement?.closest(".computer-chats"))).toBe(true);

  // A workflow in a chat's own folder is one of that chat's commands, and
  // only that chat's.
  await mkdir(join(first, ".haarness/workflows"), { recursive: true });
  await writeFile(
    join(first, ".haarness/workflows/folderflow.ts"),
    "export const meta = { name: 'folderflow', description: 'Only in this chat', phases: ['one'] };\nphase('one');\nreturn 1;\n",
  );
  await rows().filter({ hasText: "pwd" }).click();
  await composer().fill("/folderflow");
  await expect(page.locator("#slash-commands")).toContainText("/folderflow");
  await composer().fill("");
  await rows().filter({ hasText: "Lisbon" }).click();
  await composer().fill("/folderflow");
  await expect(page.locator("#slash-commands")).toHaveCount(0);
  await composer().fill("");
  // Unsaved edits in one chat's files wait for it while another chat is
  // open, with no question asked.
  await rows().filter({ hasText: "pwd" }).click();
  const show = page.getByRole("button", { name: "Show side panel", exact: true });
  if (await show.isVisible()) await show.click();
  await tree.getByRole("button", { name: "notes.txt", exact: true }).click();
  const editor = page.locator(".file-preview .cm-content");
  await editor.click();
  await page.keyboard.press("End");
  await page.keyboard.type(" and an unsaved line");
  const tab = page.locator(".file-tabs").getByRole("button", { name: /^notes\.txt/ });
  await expect(tab).toHaveText("notes.txt •");
  await rows().filter({ hasText: "Lisbon" }).click();
  await expect(page.locator(".file-tabs")).toHaveCount(0);
  await expect(tree.getByRole("button", { name: "notes.txt", exact: true })).toHaveCount(0);
  await rows().filter({ hasText: "pwd" }).click();
  await expect(tab).toHaveText("notes.txt •");
  await expect(editor).toContainText("written without a project and an unsaved line");
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect.poll(() => readFile(join(first, "notes.txt"), "utf8")).toBe("written without a project and an unsaved line");
  await page.getByRole("button", { name: "Hide side panel", exact: true }).click();
  // Finding files finds the chat's own, however many its neighbours have,
  // named from its folder: `@`, the Files panel and Search.
  for (let index = 0; index < 60; index++) await writeFile(join(first, `match-other-${index}.txt`), "theirs\n");
  await writeFile(join(second, "match-mine.txt"), "mine\n");
  await rows().filter({ hasText: "Lisbon" }).click();
  await composer().fill("@match");
  const mentions = page.locator("#prompt-mentions").getByRole("option");
  await expect(mentions.filter({ hasText: "match-mine.txt" })).toHaveCount(1);
  await expect(mentions.filter({ hasText: "match-other" })).toHaveCount(0);
  await mentions.filter({ hasText: "match-mine.txt" }).click();
  await expect(page.getByLabel("Attached context", { exact: true })).toContainText("match-mine.txt");
  await expect(page.getByLabel("Attached context", { exact: true })).not.toContainText(today());
  // Typed paths still complete from this computer's home folder.
  await writeFile(join(temporary, "home", "home-note.txt"), "x");
  await composer().fill("@~/");
  await expect(page.locator("#prompt-mentions").getByRole("option").filter({ hasText: "home-note.txt" })).toHaveCount(1);
  await composer().fill("");
  if (await page.getByRole("button", { name: "Show side panel", exact: true }).isVisible())
    await page.getByRole("button", { name: "Show side panel", exact: true }).click();
  await page.getByRole("textbox", { name: "Find files" }).fill("match");
  await expect(page.locator(".file-tree .file-entry")).toHaveText(["match-mine.txt"]);
  await page.getByRole("textbox", { name: "Find files" }).fill("");
  await page.getByRole("button", { name: "Hide side panel", exact: true }).click();
  await page.getByRole("button", { name: "Search", exact: true }).click();
  await page.getByRole("combobox", { name: "Search Haarness", exact: true }).fill("match-mine");
  await expect(page.getByRole("option").filter({ hasText: "match-mine.txt" })).toHaveCount(1);
  await page.keyboard.press("Escape");
  await rows().filter({ hasText: "pwd" }).click();
  // A chat whose folder a link stands in for (here, to the first chat's)
  // offers no workflows of its own, and says nothing about it.
  await rows().filter({ hasText: "pwd" }).click();
  await rename(second, second + ".moved");
  await symlink(first, second);
  try {
    await rows().filter({ hasText: "Lisbon" }).click();
    await composer().fill("/");
    await expect(page.locator("#slash-commands")).toBeVisible();
    await composer().fill("/folderflow");
    await expect(page.locator("#slash-commands")).toHaveCount(0);
    await expect(page.getByRole("alert")).toHaveCount(0);
    await composer().fill("");
  } finally {
    await rm(second);
    await rename(second + ".moved", second);
  }

  // A chat's own folder chosen as a folder opens that chat, never a project
  // inside the chat home.
  await app!.evaluate(({ dialog }, directory) => {
    dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [directory] });
  }, first);
  await page.getByRole("button", { name: "Open another project", exact: true }).click();
  await ready();
  await expect(chatFolder()).toHaveAccessibleName("Chat folder: tool-bash-command-pwd");
  await expect(page.locator(".project-list .project-heading")).toHaveCount(0);
  expect((await page.evaluate(() => window.haarness.boot())).recent).toHaveLength(1);
  // Choosing a project in the chip sends there instead, and makes no folder.
  await app!.evaluate(({ dialog }, directory) => {
    dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [directory] });
  }, project);
  await page.getByRole("button", { name: "New chat", exact: true }).click();
  await expect(newChatFolder()).toHaveAccessibleName("Folder for new chat: No folder");
  // Typed first, then a folder chosen: the text goes along.
  await composer().fill("Take this along");
  await newChatFolder().click();
  const picker = page.getByRole("dialog", { name: "Choose a folder", exact: true });
  await expect(picker.getByRole("option", { name: "No folder" })).toHaveAttribute("aria-selected", "true");
  await picker.getByRole("button", { name: "Open folder…", exact: true }).click();
  await ready();
  await expect(newChatFolder()).toHaveAccessibleName("Folder for new chat: pocket-project");
  await expect(composer()).toHaveValue("Take this along");
  await expect(page.locator(".conversation-empty h2")).toHaveText("What should we work on in pocket-project?");
  await send("Project message please");
  await expect(page.locator(".transcript")).toContainText("Echo: Project message please", { timeout: 60000 });
  await expect(page.locator(".project-list")).toContainText("Echo: Project message please");
  await expect.poll(() => stat(join(home, today(), "project-message-please")).then(() => true, () => false)).toBe(false);
  await expect(rows()).toHaveCount(2);
  await expect(chatFolder()).toHaveAccessibleName("Chat folder: pocket-project");
  // The chip goes back to No folder, which opens This computer's new chat.
  await page.getByRole("button", { name: "New chat", exact: true }).click();
  await newChatFolder().click();
  await expect(picker.getByRole("option")).toHaveText(["No folder", "pocket-project"]);
  await page.evaluate(() => ((window as any).loadingMarkSeen = false));
  await picker.getByRole("option", { name: "No folder" }).click();
  await ready();
  await expect(newChatFolder()).toHaveAccessibleName("Folder for new chat: No folder");
  // What was taken along is not left behind as well.
  await expect(composer()).toHaveValue("");
  // Going back to This computer shows its message box at once too.
  expect(await loadingMarkSeen()).toBe(false);

  // The chat home is never a project.
  await expect(page.locator(".project-list .project-heading")).toHaveText(["pocket-project"]);
  const recent = (await page.evaluate(() => window.haarness.boot())).recent;
  expect(recent.filter((item) => item.chatHome).map((item) => item.workspace)).toEqual([home]);
  expect(recent.find((item) => item.chatHome)?.project).toBeUndefined();
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  const settings = page.getByRole("dialog", { name: "Settings", exact: true });
  await settings.getByRole("button", { name: "Projects", exact: true }).click();
  await expect(settings).toContainText("pocket-project");
  await expect(settings).not.toContainText("Chat folders");
  await page.getByRole("button", { name: "Close settings", exact: true }).click();

  // Search finds these chats, labelled This computer and their folder.
  await page.getByRole("button", { name: "Search", exact: true }).click();
  await page.getByRole("combobox", { name: "Search Haarness", exact: true }).fill("Lisbon");
  const found = page.getByRole("option").filter({ hasText: "second-chat-about-lisbon" });
  await expect(found).toContainText("This computer");
  await found.click();
  await expect(page.locator(".transcript")).toContainText("Second chat about Lisbon");
  await expect(chatFolder()).toHaveAccessibleName("Chat folder: second-chat-about-lisbon");

  // Archiving one keeps its folder; it leaves This computer.
  const titled = section().locator(".session-row").filter({ hasText: /Lisbon/ });
  await titled.getByRole("button", { name: /^Archive chat / }).click();
  await expect(rows()).toHaveCount(1);
  expect((await stat(second)).isDirectory()).toBe(true);
  await expect.poll(async () => (await client!.getThread(two.id)).archived).toBe(true);

  // A relaunch keeps This computer's chats.
  client.close();
  client = undefined;
  await app!.close();
  await launch();
  await ready();
  await expect(rows()).toHaveCount(1);
  await expect(page.locator(".project-list .project-heading")).toHaveText(["pocket-project"]);
  await rows().first().click();
  await expect(page.locator(".transcript")).toContainText("written without a project");
  await expect(chatFolder()).toHaveAccessibleName("Chat folder: tool-bash-command-pwd");
  client = homeClient();
  expect(homeDaemons()).toBe(1);
});

/** Desktop's binary, with the chat home's `desktop` calls held back while
 * `slow` exists and refused while `fail` does: a daemon slow to start, or
 * one that cannot. */
async function wrappedBinary() {
  const flags = { slow: join(temporary, "slow-home"), fail: join(temporary, "fail-home") };
  const wrapper = join(temporary, "haarness-wrapper");
  await writeFile(
    wrapper,
    `#!/bin/sh
case " $* " in
  *" --chat-home "*" desktop "*|*" --chat-home desktop"*)
    if [ -e "${flags.slow}" ]; then sleep 5; fi
    if [ -e "${flags.fail}" ]; then echo "The chat home could not start (test)" >&2; exit 3; fi
    ;;
esac
exec "${binary}" "$@"
`,
    { mode: 0o755 },
  );
  env.HAARNESS_BIN = wrapper;
  return flags;
}

test("opening This computer from a project chat shows its new chat at once, and a message sent meanwhile goes when it is open", async () => {
  const flags = await wrappedBinary();
  // A project remembered from before, and no chat home yet.
  await writeFile(
    join(temporary, "desktop", "desktop.json"),
    JSON.stringify({
      preferences: {},
      credentials: {},
      recent: [{ id: "local:" + project, label: "pocket-project", workspace: project, remote: false, trust: true }],
      values: { "active-project": "local:" + project, "active-server": "local", "server-project:local": "local:" + project },
    }),
  );
  // A Git project: its new chat offers the checkout beside the folder chip.
  for (const args of [["init", "-q", "-b", "main"], ["-c", "user.name=Fixture", "-c", "user.email=fixture@example.test", "commit", "-q", "--allow-empty", "-m", "Fixture"]])
    execFileSync("git", args, { cwd: project, stdio: "ignore" });
  await launch();
  await ready();
  await expect(newChatFolder()).toHaveAccessibleName("Folder for new chat: pocket-project");
  // The composer's row keeps to its width in a small window with the side
  // panel open, in English and in Arabic: its chips give way to their icons.
  await app!.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows()[0].setContentSize(900, 640));
  await page.getByRole("button", { name: "Show side panel", exact: true }).click();
  // Every control of the row lies inside it, none cut off or under the next.
  const fits = () =>
    page.locator(".composer-leading").evaluate((row) => {
      const box = row.getBoundingClientRect();
      return [...row.children].every((child) => {
        const part = child.getBoundingClientRect();
        return part.left >= box.left - 1 && part.right <= box.right + 1;
      });
    });
  await expect(page.getByRole("button", { name: "Choose working location", exact: true })).toBeVisible();
  await expect.poll(fits).toBe(true);
  await page.evaluate(() => window.haarness.preferences({ language: "ar" }));
  await expect(page.locator("html")).toHaveAttribute("dir", "rtl");
  await expect.poll(fits).toBe(true);
  // The heading naming the project follows the language too.
  await expect(page.locator(".conversation-empty h2")).toHaveText("على ماذا نعمل في pocket-project؟");
  await page.evaluate(() => window.haarness.preferences({ language: "en" }));
  await expect(page.locator(".conversation-empty h2")).toHaveText("What should we work on in pocket-project?");
  await page.getByRole("button", { name: "Hide side panel", exact: true }).click();
  await app!.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows()[0].setContentSize(1280, 820));
  await send("A project chat first");
  await expect(page.locator(".transcript")).toContainText("Echo: A project chat first", { timeout: 60000 });
  // This computer's daemon is slow to start this time. (Opening the project
  // showed the loading mark, as opening a project does.)
  await page.evaluate(() => ((window as any).loadingMarkSeen = false));
  await writeFile(flags.slow, "");
  await section().getByRole("button", { name: "New chat in This computer", exact: true }).click();
  // At once: its new chat, not the project's chat under its name.
  await expect(page.locator(".conversation-empty h2")).toHaveText("What should we work on?");
  await expect(newChatFolder()).toHaveAccessibleName("Folder for new chat: No folder");
  await expect(page.locator(".titlebar-caption")).toHaveText("This computer");
  await expect(page.locator(".transcript")).not.toContainText("A project chat first");
  await expect(composer()).toBeEnabled();
  await send("Sent while it opens");
  await expect(page.getByRole("button", { name: /Send message/ })).toHaveAttribute("aria-busy", "true");
  await rm(flags.slow);
  await expect(rows()).toHaveCount(1, { timeout: 60000 });
  await expect(page.locator(".transcript")).toContainText("Echo: Sent while it opens", { timeout: 60000 });
  expect(await loadingMarkSeen()).toBe(false);
  await expect(page.locator(".project-list")).toContainText("Echo: A project chat first");
});

test("This computer that cannot open says so in its own words, and Retry opens it with what was typed", async () => {
  const flags = await wrappedBinary();
  await writeFile(flags.slow, "");
  await writeFile(flags.fail, "");
  await launch();
  await expect(composer()).toBeEnabled();
  await composer().fill("Typed before it failed");
  const home = page.getByRole("region", { name: "Project home" });
  await expect(home.getByRole("heading")).toHaveText("Couldn’t open This computer", { timeout: 60000 });
  await expect(home.getByRole("alert")).toContainText("could not start");
  await expect(home).not.toContainText("Couldn’t open project");
  await expect(home.getByRole("button", { name: "Open project", exact: true })).toBeVisible();
  await rm(flags.fail);
  await rm(flags.slow);
  await home.getByRole("button", { name: "Retry", exact: true }).click();
  await ready();
  await expect(newChatFolder()).toHaveAccessibleName("Folder for new chat: No folder");
  await expect(composer()).toHaveValue("Typed before it failed");
  await expect(page.getByRole("alert")).toHaveCount(0);
});

test("This computer's daemon starts only when it is used: a launch on a project shows its chats from before without it", async () => {
  await launch();
  await expect(composer()).toBeEnabled();
  await send("A chat from before");
  await expect(rows()).toHaveCount(1, { timeout: 60000 });
  await expect(page.locator(".transcript")).toContainText("Echo: A chat from before", { timeout: 60000 });
  // The project is the place left open, so the next launch opens it.
  await app!.evaluate(({ dialog }, directory) => {
    dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [directory] });
  }, project);
  await page.getByRole("button", { name: "Open another project", exact: true }).click();
  await ready();
  await app!.close();
  stopChatHome(binary, env);
  await expect.poll(homeDaemons).toBe(0);
  await launch();
  await ready();
  await expect(page.locator(".titlebar-caption")).toHaveText("pocket-project");
  // Its chats show as they were left, and nothing started for them.
  await expect(rows()).toHaveCount(1);
  await expect(rows().first()).toContainText("A chat from before");
  await page.waitForTimeout(2000);
  expect(homeDaemons()).toBe(0);
  // Opening one of them starts it.
  await rows().first().click();
  await expect(page.locator(".transcript")).toContainText("Echo: A chat from before", { timeout: 60000 });
  expect(homeDaemons()).toBe(1);
});
