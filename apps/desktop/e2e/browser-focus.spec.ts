import { test, expect, _electron as electron, type ElectronApplication, type Page } from "@playwright/test";
import { Haarness, type Thread } from "@haarness/sdk";
import { mkdtemp, mkdir, writeFile, realpath, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";
import { createServer, type Server } from "node:http";

// The person's keyboard and screen stay theirs while an agent drives the
// Haarness browser: pages the agent loads, clicks or types in cannot take the
// keyboard from the message box, a page put away while it has the keyboard
// gives it back to the app, and a file chooser a page opens during the
// agent's action never appears over the window.
test.describe.configure({ mode: "serial" });

let app: ElectronApplication, page: Page, client: Haarness, thread: Thread, site: Server;
let temporary: string, workspace: string, siteURL: string, env: Record<string, string>;
let endpoint: { url: string; token: string };
const errors: string[] = [];
const binary = resolve(process.env.HAARNESS_BIN ?? "../../target/debug/haarness");

const focusPage = `<!doctype html><html lang="en"><head><title>Focus fixture</title></head><body>
<label>Field <input id="field" autofocus></label>
<button id="grab" onclick="setTimeout(() => { window.focus(); field.focus(); }, 20)">Grab focus</button>
<a href="/focus?next">Next page</a>
<label>Pick a file <input type="file" id="file"></label>
<button onclick="file.click()">Choose file</button>
<button onclick="setTimeout(() => file.click(), 1500)">Choose file later</button>
<p id="picked">none</p>
<script>window.focus(); field.focus(); file.oncancel = () => picked.textContent = "cancelled"; file.onchange = () => picked.textContent = "picked";</script>
</body></html>`;

async function tool(name: string, args: object = {}) {
  const sdk = new Haarness({ baseUrl: endpoint.url, token: endpoint.token });
  const result = await sdk.callBrowserTool(thread.id, { name: "browser_" + name, arguments: args as any }, { timeoutMs: 90_000 });
  sdk.close();
  if (result.isError) throw new Error(result.output);
  return JSON.parse(result.output);
}
function ref(snapshot: string, text: string) {
  const line = snapshot.split("\n").find((line) => /@e\d+/.test(line) && line.includes(JSON.stringify(text)));
  const value = line?.match(/@e\d+/)?.[0];
  if (!value) throw new Error("No ref for " + text + " in " + snapshot);
  return value;
}

test.beforeAll(async () => {
  test.setTimeout(240_000);
  temporary = await realpath(await mkdtemp(join(process.env.HAARNESS_E2E_DIR ?? tmpdir(), "haarness-browser-focus-")));
  workspace = join(temporary, "workspace");
  await mkdir(join(workspace, ".haarness"), { recursive: true });
  await writeFile(join(workspace, ".haarness/config.toml"), '[defaults]\nprovider="echo"\nmodel="echo-1"\n');
  env = {
    ...(Object.fromEntries(Object.entries(process.env).filter((x): x is [string, string] => x[1] !== undefined))),
    HAARNESS_BIN: binary,
    HAARNESS_HOME: join(temporary, "home"),
    HAARNESS_DATA_DIR: join(temporary, "state"),
    HAARNESS_DESKTOP_DATA: join(temporary, "desktop"),
    HAARNESS_TEST_PROVIDER: "1",
    HAARNESS_DESKTOP_TEST: "1",
  };
  delete env.ELECTRON_RUN_AS_NODE;
  delete env.ELECTRON_RENDERER_URL;
  site = createServer((_request, response) => {
    response.setHeader("Content-Type", "text/html");
    response.end(focusPage);
  });
  await new Promise<void>((done) => site.listen(0, "127.0.0.1", done));
  siteURL = "http://127.0.0.1:" + (site.address() as any).port;
  app = await electron.launch({ args: ["."], env, timeout: 120_000 });
  app.context().on("dialog", () => {});
  page = await app.firstWindow();
  page.on("pageerror", (error) => errors.push(error.message));
  await app.evaluate(({ BrowserWindow, dialog }, directory) => {
    const win = BrowserWindow.getAllWindows()[0];
    win.setContentSize(1400, 950);
    win.show();
    dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [directory] });
  }, workspace);
  await page.getByRole("region", { name: "Project home" }).getByRole("button", { name: "Open project", exact: true }).click();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 90_000 });
  endpoint = JSON.parse(
    execFileSync(binary, ["--workspace", workspace, "--trust-project", "desktop"], {
      env,
      input: '{"action":"connect"}',
      encoding: "utf8",
      timeout: 60_000,
    }),
  );
  client = new Haarness({ baseUrl: endpoint.url, token: endpoint.token });
  thread = await client.createThread({ title: "Browser focus", provider: "echo", model: "echo-1" });
  await page.locator(".sessions").getByRole("button", { name: "Browser focus", exact: true }).click();
  await expect
    .poll(async () => {
      try {
        const state = await tool("tabs_list");
        return state.browsers?.find((entry: any) => entry.browser === "haarness")?.available === false ? -1 : state.tabs.length;
      } catch {
        return -1;
      }
    })
    .toBeGreaterThanOrEqual(0);
});

test.afterAll(async () => {
  await app?.close().catch(() => {});
  client?.close();
  await new Promise<void>((done) => (site ? site.close(() => done()) : done()));
  if (workspace)
    try {
      execFileSync(binary, ["--workspace", workspace, "--trust-project", "serve", "--stop"], { env, stdio: "ignore", timeout: 30_000 });
    } catch {}
  if (temporary) await rm(temporary, { recursive: true, force: true }).catch(() => {});
});

/** Where the keyboard is: the app's own page or a browser page, and whether the message box has it there. */
const keyboard = async () => ({
  ...(await app.evaluate(({ BrowserWindow, webContents }) => {
    const win = BrowserWindow.getAllWindows()[0];
    return {
      window: win.isFocused(),
      app: win.webContents.isFocused(),
      pages: webContents
        .getAllWebContents()
        .filter((contents) => contents !== win.webContents && contents.isFocused())
        .map((contents) => new URL(contents.getURL()).search),
    };
  })),
  ...(await page.evaluate(() => {
    const box = document.querySelector<HTMLTextAreaElement>("[data-composer-input]");
    const active = document.activeElement;
    // Named for a failure message only.
    const element = active ? [active.tagName, active.getAttribute("aria-label") ?? active.className].join(" ") : "";
    return { composer: active === box, element };
  })),
});
/** Makes the window the one the system types into: only then does a page holding the keyboard show. */
async function activate() {
  await app.evaluate(({ app, BrowserWindow }) => {
    app.focus({ steal: true });
    BrowserWindow.getAllWindows()[0].focus();
  });
  await expect.poll(async () => (await keyboard()).window).toBe(true);
}
/** Where the keyboard is once a step has settled, with the window active again whenever another app took it. */
async function settled() {
  await page.waitForTimeout(400);
  for (let attempt = 0; ; attempt++) {
    const value = await keyboard();
    if (value.window || attempt === 5) return value;
    await activate();
  }
}
/** Gives a page the keyboard, as a click in it does. */
const typeInPage = (search: string) =>
  app.evaluate(({ webContents }, search) => {
    webContents.getAllWebContents().find((contents) => contents.getURL().endsWith("/focus" + search))!.focus();
  }, search);

// The caret is not compared: the person may type while the window is active.
const inMessageBox = { app: true, pages: [], composer: true };

test("the message box keeps the keyboard while the agent loads, clicks and types in pages that ask for it", async () => {
  const box = page.getByRole("textbox", { name: "Message" });
  const first = (await tool("tabs_open", { url: siteURL + "/focus?first" })).id;
  await expect(page.getByRole("region", { name: "Web browser" })).toBeVisible();
  await box.fill("Keep typing here");
  await activate();
  await box.evaluate((element: HTMLTextAreaElement) => {
    element.focus();
    element.setSelectionRange(4, 4);
  });
  expect(await settled()).toMatchObject(inMessageBox);
  // Each page focuses its field when it loads, and calls window.focus().
  await tool("navigate", { tabID: first, url: siteURL + "/focus?second" });
  expect(await settled()).toMatchObject(inMessageBox);
  let snapshot = (await tool("snapshot", { tabID: first })).content;
  // A click whose handler focuses the field and the window again.
  await tool("click", { tabID: first, ref: ref(snapshot, "Grab focus") });
  expect(await settled()).toMatchObject(inMessageBox);
  snapshot = (await tool("snapshot", { tabID: first })).content;
  await tool("fill", { tabID: first, ref: ref(snapshot, "Field "), text: "agent text" });
  expect(await settled()).toMatchObject(inMessageBox);
  await tool("press", { tabID: first, key: "Enter" });
  expect(await settled()).toMatchObject(inMessageBox);
  await tool("back", { tabID: first });
  expect(await settled()).toMatchObject(inMessageBox);
  const second = (await tool("tabs_open", { url: siteURL + "/focus?third" })).id;
  expect(await settled()).toMatchObject(inMessageBox);
  await tool("tabs_focus", { tabID: first });
  expect(await settled()).toMatchObject(inMessageBox);
  await tool("screenshot", { tabID: first });
  expect(await settled()).toMatchObject(inMessageBox);
  await tool("tabs_close", { tabID: second });
  expect(await box.inputValue()).toBe("Keep typing here");
  // The check sees a page that holds the keyboard: the person clicking into it.
  await typeInPage("?first");
  expect(await settled()).toMatchObject({ app: false, pages: ["?first"] });
  expect(errors).toEqual([]);
});

test("a page put away while it has the keyboard gives it back to the message box", async () => {
  const box = page.getByRole("textbox", { name: "Message" });
  const first =
    (await tool("tabs_list")).tabs.find((tab: { url: string }) => tab.url.endsWith("/focus?first"))?.id ??
    (await tool("tabs_open", { url: siteURL + "/focus?first" })).id;
  await box.evaluate((element: HTMLTextAreaElement) => {
    element.focus();
    element.setSelectionRange(4, 4);
  });
  // The person types in the page; the agent shows another tab.
  const other = (await tool("tabs_open", { url: siteURL + "/focus?other" })).id;
  await tool("tabs_focus", { tabID: first });
  await activate();
  await typeInPage("?first");
  expect(await settled()).toMatchObject({ app: false, pages: ["?first"] });
  await tool("tabs_focus", { tabID: other });
  expect(await settled()).toMatchObject(inMessageBox);
  // The person types in the page; the agent closes it.
  await typeInPage("?other");
  expect(await settled()).toMatchObject({ app: false, pages: ["?other"] });
  await tool("tabs_close", { tabID: other });
  expect(await settled()).toMatchObject(inMessageBox);
  // A control other than the message box, left with the window's keyboard,
  // does not get keys meant for the page.
  await page.getByRole("button", { name: "Hide side panel", exact: true }).focus();
  await typeInPage("?first");
  expect(await settled()).toMatchObject({ app: false, pages: ["?first"] });
  const another = (await tool("tabs_open", { url: siteURL + "/focus?another" })).id;
  expect(await settled()).toMatchObject({ app: false, pages: [] });
  await tool("tabs_close", { tabID: another });
  await tool("tabs_focus", { tabID: first });
  expect(errors).toEqual([]);
});

test("a file chooser a page opens during or just after the agent's click never appears, and the agent is told to upload instead", async () => {
  // While a native chooser is open the page hears nothing; a held one is
  // cancelled for the page at once. (The window's own focus is no witness
  // here: other apps on the machine take it at any time.)
  const picked = async () => (await tool("evaluate", { tabID, script: "document.getElementById('picked').textContent" })).value;
  const reset = () => tool("evaluate", { tabID, script: "document.getElementById('picked').textContent = 'none'" });
  const tabID = (await tool("tabs_open", { url: siteURL + "/focus?files" })).id;
  let snapshot = (await tool("snapshot", { tabID })).content;
  const clicked = await tool("click", { tabID, ref: ref(snapshot, "Choose file") });
  expect(clicked.fileChooser).toMatch(/^The page opened a file chooser, which Haarness kept from appearing.*browser\.files\.upload/);
  await expect.poll(picked).toBe("cancelled");
  // A widget that opens its chooser a moment after the click (once a request
  // returns, say) is held too, and the agent's next result says so.
  await reset();
  snapshot = (await tool("snapshot", { tabID })).content;
  const later = await tool("click", { tabID, ref: ref(snapshot, "Choose file later") });
  expect(later.fileChooser).toBeUndefined();
  await page.waitForTimeout(2500);
  const after = await tool("evaluate", { tabID, script: "document.getElementById('picked').textContent" });
  expect(after.value).toBe("cancelled");
  expect(after.fileChooser).toMatch(/^Shortly after your last action, the page opened a file chooser/);
  // A press of the person's own ends the hold at once: the system's chooser
  // opens as usual, so the page hears no cancel.
  const box = (await tool("evaluate", {
    tabID,
    script: "(() => { document.getElementById('picked').textContent = 'none'; const r = [...document.querySelectorAll('button')].find((b) => b.textContent === 'Choose file').getBoundingClientRect(); return { x: r.x + r.width / 2, y: r.y + r.height / 2 }; })()",
  })).value as { x: number; y: number };
  await page.waitForTimeout(500);
  await app.evaluate(({ webContents }, { x, y }) => {
    const contents = webContents.getAllWebContents().find((contents) => contents.getURL().endsWith("/focus?files"))!;
    contents.sendInputEvent({ type: "mouseDown", x, y, button: "left", clickCount: 1 });
    contents.sendInputEvent({ type: "mouseUp", x, y, button: "left", clickCount: 1 });
  }, box);
  await page.waitForTimeout(1500);
  expect(await picked()).toBe("none");
  expect(errors).toEqual([]);
});
