import { Haarness } from "@haarness/sdk";
import { test, expect, _electron as electron, type ElectronApplication, type Page } from "@playwright/test";
import { mkdir, mkdtemp, realpath, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";
import { randomUUID } from "node:crypto";

// Real Electron, IPC, disk preferences and a project daemon on the echo
// provider. Only the native folder picker is given a deterministic answer.
const binary = resolve(process.env.HAARNESS_BIN ?? "../../target/debug/haarness");
let app: ElectronApplication;
let page: Page;
let temporary: string;
let workspace: string;
let env: Record<string, string>;
let client: Haarness;
let errors: string[] = [];
/** Projects opened after the first, whose daemons the file stops. */
const others: { directory: string; client: Haarness }[] = [];

const sidebar = () => page.getByRole("region", { name: "Project workspace", exact: true });
const tabs = () => page.locator(".workspace-tabs [role=tab]");

test.beforeAll(async () => {
  temporary = await realpath(await mkdtemp(join(tmpdir(), "haarness-chat-tabs-")));
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
  page.on("pageerror", error => errors.push(error.message));
  await app.evaluate(({ dialog }, directory) => {
    dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [directory] });
  }, workspace);
  await page.getByRole("region", { name: "Project home" }).getByRole("button", { name: "Open project", exact: true }).click();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 60000 });
  const connection = JSON.parse(execFileSync(binary, ["--workspace", workspace, "--trust-project", "desktop"], {
    env, input: JSON.stringify({ action: "connect" }), encoding: "utf8", timeout: 60000,
  }));
  client = new Haarness({ baseUrl: connection.url, token: connection.token });
});

test.afterAll(async () => {
  // A loaded machine can take minutes to close Electron; stop waiting after a
  // bounded time so neither Electron nor the project daemon outlives the file.
  test.setTimeout(180000);
  client?.close();
  for (const other of others) other.client.close();
  if (app) {
    const child = app.process();
    const exited = new Promise<void>(resolve => {
      if (child.exitCode !== null) resolve();
      else child.once("exit", () => resolve());
    });
    const within = (ms: number) => Promise.race([exited, new Promise<void>(resolve => setTimeout(resolve, ms))]);
    // Destroying the windows skips the close and quit handling a user's
    // close goes through, as desktop.spec.ts and sidebar.spec.ts do.
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
  for (const directory of [workspace, ...others.map(other => other.directory)])
    try { execFileSync(binary, ["--workspace", directory, "--trust-project", "serve", "--stop"], { env, stdio: "ignore", timeout: 60000 }); } catch {}
  if (temporary) await rm(temporary, { recursive: true, force: true, maxRetries: 10 }).catch(() => {});
});

test.beforeEach(() => { errors = []; });

async function send(text: string) {
  await page.getByRole("textbox", { name: "Message", exact: true }).fill(text);
  await page.getByRole("button", { name: "Send message", exact: true }).click();
  await expect(page.locator(".transcript")).toContainText(`Echo: ${text}`);
}

/** The generated title of the newest chat whose first message was `text`. */
async function generatedTitle(text: string) {
  let title = "";
  await expect.poll(async () => {
    const threads = await client.listThreads();
    for (const thread of threads) {
      const items = await client.listItems(thread.id);
      const first = items.find(item => item.data.type === "user_message");
      if (first?.data.type === "user_message" && first.data.text === text && !/^New (chat|thread)$/.test(thread.title)) {
        title = thread.title;
        return true;
      }
    }
    return false;
  }, { timeout: 30000 }).toBe(true);
  return title;
}

test("a new chat's generated title reaches its tab without reselecting the chat", async () => {
  // The only open chat names itself in the session bar.
  await page.getByRole("button", { name: "New chat", exact: true }).click();
  await send("first tab title check");
  const first = await generatedTitle("first tab title check");
  await expect(sidebar().getByRole("button", { name: first, exact: true })).toBeVisible();
  await expect(page.locator(".session-bar-name")).toHaveText(first);
  // With another chat open, the new one's tab carries its title.
  await page.getByRole("button", { name: "New chat tab", exact: true }).click();
  await send("second tab title check");
  const second = await generatedTitle("second tab title check");
  await expect(sidebar().getByRole("button", { name: second, exact: true })).toBeVisible();
  await expect(tabs()).toHaveText([first, second]);
  await expect(page.getByRole("tab", { name: second, exact: true })).toHaveAttribute("aria-selected", "true");
  expect(errors).toEqual([]);
});

async function reload() {
  await page.reload();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 60000 });
}

test("reloading the window keeps every open chat tab and the active one", async () => {
  const names = ["Reload one", "Reload two", "Reload three"];
  const ids: Record<string, string> = {};
  // A double-click keeps each chat in a tab of its own (a plain click only
  // previews it).
  for (const title of names) {
    ids[title] = (await client.createThread({ title, provider: "echo", model: "echo-1" })).id;
    await sidebar().getByRole("button", { name: title, exact: true }).dblclick();
  }
  const before = await tabs().allTextContents();
  expect(before.slice(-3)).toEqual(names);
  await page.getByRole("tab", { name: "Reload two", exact: true }).click();
  await reload();
  await expect(tabs()).toHaveText(before);
  await expect(page.getByRole("tab", { name: "Reload two", exact: true })).toHaveAttribute("aria-selected", "true");
  // A closed tab stays closed, and an archived chat's tab is not reopened.
  await page.locator(".workspace-tab")
    .filter({ has: page.getByRole("tab", { name: "Reload one", exact: true }) })
    .getByRole("button", { name: "Close chat tab", exact: true })
    .click();
  await expect(page.getByRole("tab", { name: "Reload one", exact: true })).toHaveCount(0);
  await client.updateThread(ids["Reload three"], { archived: true });
  await reload();
  await expect(tabs()).toHaveText(before.filter(name => name !== "Reload one" && name !== "Reload three"));
  await expect(page.getByRole("tab", { name: "Reload two", exact: true })).toHaveAttribute("aria-selected", "true");
  expect(errors).toEqual([]);
});

test("archiving a chat closes its background tab, while the chat on screen stays open to restore", async () => {
  const names = ["Archive kept", "Archive from sidebar", "Archive elsewhere"];
  const ids: Record<string, string> = {};
  for (const title of names) {
    ids[title] = (await client.createThread({ title, provider: "echo", model: "echo-1" })).id;
    await sidebar().getByRole("button", { name: title, exact: true }).dblclick();
  }
  await page.getByRole("tab", { name: "Archive kept", exact: true }).click();
  const tab = (name: string) => page.getByRole("tab", { name, exact: true });
  // From the sidebar row's own archive button: the row and the tab both go.
  await sidebar().getByRole("button", { name: "Archive chat Archive from sidebar", exact: true }).click();
  await expect(sidebar().getByRole("button", { name: "Archive from sidebar", exact: true })).toHaveCount(0);
  await expect(tab("Archive from sidebar")).toHaveCount(0);
  // From another client: the open window follows.
  await client.updateThread(ids["Archive elsewhere"], { archived: true });
  await expect(tab("Archive elsewhere")).toHaveCount(0);
  await expect(tab("Archive kept")).toHaveAttribute("aria-selected", "true");
  // The chat on screen stays, says it is archived, and restores.
  await page.getByRole("button", { name: "Chat actions", exact: true }).click();
  await page.getByRole("menuitem", { name: "Archive", exact: true }).click();
  const notice = page.locator(".archived-notice");
  await expect(notice).toContainText("This chat is archived.");
  await expect(tab("Archive kept")).toHaveAttribute("aria-selected", "true");
  await page.screenshot({ path: test.info().outputPath("archived-on-screen.png") });
  await notice.getByRole("button", { name: "Restore", exact: true }).click();
  await expect(notice).toHaveCount(0);
  await expect(sidebar().getByRole("button", { name: "Archive kept", exact: true })).toBeVisible();
  // Closed tabs stay closed after a reload; restoring a chat does not reopen one.
  const open = await tabs().allTextContents();
  await client.updateThread(ids["Archive from sidebar"], { archived: false });
  await expect(sidebar().getByRole("button", { name: "Archive from sidebar", exact: true })).toBeVisible();
  await expect(tabs()).toHaveText(open);
  await reload();
  await expect(tabs()).toHaveText(open);
  await expect(tab("Archive kept")).toHaveAttribute("aria-selected", "true");
  expect(errors).toEqual([]);
});

test("each project keeps its own tabs across a switch to another project and back", async () => {
  const first = await tabs().allTextContents();
  expect(first.length).toBeGreaterThan(1);
  const directory = join(temporary, "second");
  await mkdir(join(directory, ".haarness"), { recursive: true });
  await writeFile(join(directory, ".haarness", "config.toml"), '[defaults]\nprovider="echo"\nmodel="echo-1"\n');
  await app.evaluate(({ dialog }, directory) => {
    dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [directory] });
  }, directory);
  await page.getByRole("button", { name: "Open another project", exact: true }).click();
  const second = page.getByRole("region", { name: "Project second", exact: true });
  await expect(second).toHaveClass(/current/);
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 60000 });
  const connection = JSON.parse(execFileSync(binary, ["--workspace", directory, "--trust-project", "desktop"], {
    env, input: JSON.stringify({ action: "connect" }), encoding: "utf8", timeout: 60000,
  }));
  const other = new Haarness({ baseUrl: connection.url, token: connection.token });
  others.push({ directory, client: other });
  // A project opened for the first time has no tabs of its own yet.
  await expect(tabs()).toHaveCount(0);
  for (const title of ["Second one", "Second two"]) {
    await other.createThread({ title, provider: "echo", model: "echo-1" });
    await second.getByRole("button", { name: title, exact: true }).dblclick();
  }
  await expect(tabs()).toHaveText(["Second one", "Second two"]);
  // Back in the first project, through one of its chats: all of its tabs return.
  await sidebar().getByRole("button", { name: "Reload two", exact: true }).click();
  await expect(sidebar()).toHaveClass(/current/);
  await expect(tabs()).toHaveText(first);
  await expect(page.getByRole("tab", { name: "Reload two", exact: true })).toHaveAttribute("aria-selected", "true");
  // And the second project's tabs return with it.
  await second.getByRole("button", { name: "Second one", exact: true }).click();
  await expect(second).toHaveClass(/current/);
  await expect(tabs()).toHaveText(["Second one", "Second two"]);
  await expect(page.getByRole("tab", { name: "Second one", exact: true })).toHaveAttribute("aria-selected", "true");
  expect(errors).toEqual([]);
});

test("an agent's own chat keeps its tab when the server archives it as the agent's work ends", async () => {
  await sidebar().getByRole("button", { name: "Reload two", exact: true }).click();
  await expect(sidebar()).toHaveClass(/current/);
  const parent = await client.createThread({ title: "Agent parent", provider: "echo", model: "echo-1" });
  const witness = await client.createThread({ title: "Agent witness", provider: "echo", model: "echo-1" });
  for (const title of ["Agent witness", "Agent parent"])
    await sidebar().getByRole("button", { name: title, exact: true }).dblclick();
  await client.startTurn(parent.id, {
    requestId: randomUUID(),
    text: "!tool task " + JSON.stringify({ description: "Tab child", agent: "general", prompt: "child says hello" }),
  });
  await expect.poll(() => client.activeTurns().then(turns => turns.length)).toBe(0);
  await page.getByRole("button", { name: "Open agent task: Tab child", exact: true }).click();
  const child = await page.locator(".workspace-tabs [role=tab][aria-selected=true]").getAttribute("data-cycle-tab");
  expect(child).toBeTruthy();
  expect(child).not.toBe(parent.id);
  const childTab = page.locator(`.workspace-tabs [data-cycle-tab="${child}"]`);
  await page.getByRole("tab", { name: "Agent parent", exact: true }).click();
  // The server archives an agent's chat itself when the agent's work ends.
  await client.updateThread(child!, { archived: true });
  // Archived after it, the witness's tab closing shows the window has seen both.
  await client.updateThread(witness.id, { archived: true });
  await expect(page.getByRole("tab", { name: "Agent witness", exact: true })).toHaveCount(0);
  await expect(childTab).toHaveCount(1);
  await reload();
  await expect(childTab).toHaveCount(1);
  await expect(page.getByRole("tab", { name: "Agent parent", exact: true })).toHaveAttribute("aria-selected", "true");
  expect(errors).toEqual([]);
});

test("a plain click opens a preview tab that the next one replaces, until the chat is used or kept", async () => {
  await sidebar().getByRole("button", { name: "Reload two", exact: true }).click();
  await expect(sidebar()).toHaveClass(/current/);
  for (const title of ["Preview one", "Preview two", "Preview three", "Preview four"])
    await client.createThread({ title, provider: "echo", model: "echo-1" });
  const row = (name: string) => sidebar().getByRole("button", { name, exact: true });
  const tab = (name: string) => page.getByRole("tab", { name, exact: true });
  const preview = page.locator(".workspace-tab.preview");
  const kept = await tabs().allTextContents();
  expect(kept.length).toBeGreaterThan(1);
  await expect(preview).toHaveCount(0);
  // The first plain click adds the preview; the next replaces it in place.
  await row("Preview one").click();
  await expect(tabs()).toHaveText([...kept, "Preview one"]);
  await expect(preview).toHaveText("Preview one");
  await expect(tab("Preview one")).toHaveAttribute("aria-description", /^Preview tab\./);
  expect(await preview.locator("[role=tab]").evaluate(element => getComputedStyle(element).fontStyle)).toBe("italic");
  await row("Preview two").click();
  await expect(tabs()).toHaveText([...kept, "Preview two"]);
  await expect(tab("Preview two")).toHaveAttribute("aria-selected", "true");
  // Choosing another tab leaves the preview as it is.
  await tab(kept[0]).click();
  await expect(preview).toHaveText("Preview two");
  await tab("Preview two").click();
  // A draft typed there keeps it, so the next click opens another preview.
  const composer = page.getByRole("textbox", { name: "Message", exact: true });
  await composer.fill("a draft keeps this tab");
  await expect(preview).toHaveCount(0);
  await row("Preview three").click();
  await expect(tabs()).toHaveText([...kept, "Preview two", "Preview three"]);
  await expect(preview).toHaveText("Preview three");
  // A middle-click opens a tab of its own beside the preview.
  await row("Preview one").click({ button: "middle" });
  await expect(tabs()).toHaveText([...kept, "Preview two", "Preview three", "Preview one"]);
  await expect(tab("Preview one")).toHaveAttribute("aria-selected", "true");
  await expect(preview).toHaveText("Preview three");
  // Double-clicking the preview's tab keeps it.
  await tab("Preview three").dblclick();
  await expect(preview).toHaveCount(0);
  // A Search pick previews; with Command (Control elsewhere) held it keeps a tab.
  const mod = process.platform === "darwin" ? "Meta" : "Control";
  await page.getByRole("button", { name: "Search", exact: true }).click();
  await page.getByRole("combobox", { name: "Search Haarness", exact: true }).fill("Preview four");
  await page.keyboard.press("Enter");
  await expect(preview).toHaveText("Preview four");
  await expect(composer).toBeFocused();
  await page.getByRole("button", { name: "Search", exact: true }).click();
  await page.getByRole("combobox", { name: "Search Haarness", exact: true }).fill("Reload two");
  await page.keyboard.press(`${mod}+Enter`);
  await expect(tab("Reload two")).toHaveAttribute("aria-selected", "true");
  await expect(preview).toHaveText("Preview four");
  // A preview on screen comes back as the preview after a reload; one
  // behind another tab does not come back.
  await tab("Preview four").click();
  const open = await tabs().allTextContents();
  await reload();
  await expect(tabs()).toHaveText(open);
  await expect(preview).toHaveText("Preview four");
  await tab("Preview two").click();
  await reload();
  await expect(tabs()).toHaveText(open.filter(name => name !== "Preview four"));
  await expect(preview).toHaveCount(0);
  await page.getByRole("textbox", { name: "Message", exact: true }).fill("");
  expect(errors).toEqual([]);
});

test("a preview holding only a folded paste stays, Enter on its tab keeps it, and a double-click in another project keeps a tab", async () => {
  await sidebar().getByRole("button", { name: "Reload two", exact: true }).click();
  await expect(sidebar()).toHaveClass(/current/);
  for (const title of ["Chip one", "Chip two", "Enter one"])
    await client.createThread({ title, provider: "echo", model: "echo-1" });
  const row = (name: string) => sidebar().getByRole("button", { name, exact: true });
  const tab = (name: string) => page.getByRole("tab", { name, exact: true });
  const preview = page.locator(".workspace-tab.preview");
  const composer = page.getByRole("textbox", { name: "Message", exact: true });
  // A long paste folds into a chip and leaves the box empty: the chat is in
  // use all the same, so the next plain click opens a tab beside it.
  await row("Chip one").click();
  await expect(preview).toHaveText("Chip one");
  await expect(composer).toBeFocused();
  const text = Array.from({ length: 200 }, (_, index) => "line " + index).join("\n");
  await composer.evaluate((element, text) => {
    const data = new DataTransfer();
    data.setData("text/plain", text);
    element.dispatchEvent(new ClipboardEvent("paste", { clipboardData: data, bubbles: true, cancelable: true }));
  }, text);
  await expect(page.locator(".prompt-context-item[data-kind=paste]")).toHaveCount(1);
  await expect(composer).toHaveValue("");
  await expect(preview).toHaveCount(0);
  // A chat of the project already open loads in place: the session bar and
  // its tabs stay on screen (a remounted bar would lose the mark).
  await page.locator(".session-bar").evaluate(element => element.setAttribute("data-mark", ""));
  await row("Chip two").click();
  await expect(tab("Chip one")).toHaveCount(1);
  await expect(preview).toHaveText("Chip two");
  await expect(page.locator(".session-bar[data-mark]")).toHaveCount(1);
  // Enter on the preview's own tab, while it is shown, keeps it.
  await tab("Chip two").focus();
  await expect(tab("Chip two")).toHaveAttribute("aria-description", /Press Enter or double-click to keep it open\.$/);
  await page.keyboard.press("Enter");
  await expect(preview).toHaveCount(0);
  await row("Enter one").click();
  await expect(preview).toHaveText("Enter one");
  // A double-click on a chat in another project keeps its tab there, though
  // that project opens (and chooses its own last chat) first.
  const other = others[0].client;
  await other.createThread({ title: "Second three", provider: "echo", model: "echo-1" });
  const second = page.getByRole("region", { name: "Project second", exact: true });
  await second.getByRole("button", { name: "Second three", exact: true }).dblclick();
  await expect(second).toHaveClass(/current/);
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 60000 });
  await expect(tab("Second three")).toHaveAttribute("aria-selected", "true");
  await expect(preview).toHaveCount(0);
  await second.getByRole("button", { name: "Second one", exact: true }).click();
  await expect(tab("Second three")).toHaveCount(1);
  expect(errors).toEqual([]);
});

test("the active chat tab stays in view in a strip that scrolls, and a working chat's tab shows a round dot", async () => {
  await sidebar().getByRole("button", { name: "Reload two", exact: true }).click();
  await expect(sidebar()).toHaveClass(/current/);
  const titles = Array.from({ length: 12 }, (_, index) => `Strip ${index + 1}`);
  const ids: Record<string, string> = {};
  for (const title of titles)
    ids[title] = (await client.createThread({ title, provider: "echo", model: "echo-1" })).id;
  for (const title of titles)
    await sidebar().getByRole("button", { name: title, exact: true }).click({ button: "middle" });
  const strip = page.locator(".workspace-tabs");
  expect(await strip.evaluate(element => element.scrollWidth > element.clientWidth)).toBe(true);
  const inStrip = async (name: string) => {
    const view = (await strip.boundingBox())!;
    const tab = (await page.locator(".workspace-tab").filter({ has: page.getByRole("tab", { name, exact: true }) }).boundingBox())!;
    return tab.x >= view.x - 1 && tab.x + tab.width <= view.x + view.width + 1;
  };
  await expect.poll(() => inStrip("Strip 12")).toBe(true);
  // Chosen anywhere else, a tab scrolled out of the strip comes back into it.
  await sidebar().getByRole("button", { name: "Strip 1", exact: true }).click();
  await expect(page.getByRole("tab", { name: "Strip 1", exact: true })).toHaveAttribute("aria-selected", "true");
  await expect.poll(() => inStrip("Strip 1")).toBe(true);
  await page.getByRole("button", { name: "Search", exact: true }).click();
  await page.getByRole("combobox", { name: "Search Haarness", exact: true }).fill("Strip 12");
  await page.keyboard.press("Enter");
  await expect.poll(() => inStrip("Strip 12")).toBe(true);
  // A chat at work marks its tab with a 6px dot beside the title.
  await client.startTurn(ids["Strip 12"], { requestId: randomUUID(), text: "!linger 4000 still working" });
  const dot = page.getByRole("tab", { name: "Strip 12", exact: true }).locator(".session-status.running");
  await expect(dot).toBeVisible();
  expect(await dot.evaluate(element => {
    const box = element.getBoundingClientRect();
    return [Math.round(box.width), Math.round(box.height), getComputedStyle(element).display];
  })).toEqual([6, 6, "inline-block"]);
  await expect.poll(() => client.activeTurns().then(turns => turns.length), { timeout: 30000 }).toBe(0);
  expect(errors).toEqual([]);
});
