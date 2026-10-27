import { test, expect, _electron as electron, type ElectronApplication, type Page } from "@playwright/test";
import { Haarness, type Thread } from "@haarness/sdk";
import { mkdtemp, mkdir, writeFile, realpath, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";
import { createServer, type Server } from "node:http";
import { stopChatHome } from "./fixtures/chat-home";

// Pick an element in the Haarness browser: the crosshair (or ⇧⌘C) outlines
// what is under the pointer, a click turns it into a chip in the message box
// without the page hearing the click, and the message box takes the
// person's note at once. Esc stops; Shift keeps picking; the agent's own
// input still reaches the page while the person picks.
test.describe.configure({ mode: "serial" });

let app: ElectronApplication, page: Page, client: Haarness, thread: Thread, site: Server;
let temporary: string, workspace: string, siteURL: string, env: Record<string, string>;
let endpoint: { url: string; token: string };
let tabID: string;
const errors: string[] = [];
const binary = resolve(process.env.HAARNESS_BIN ?? "../../target/debug/haarness");
const shots = process.env.HAARNESS_PICK_SCREENSHOTS;

/** The page: `frame` is the cross-site address of the embedded form, so it runs in a process of its own. */
const fixture = (frame: string) => `<!doctype html><html lang="en"><head><title>Pick fixture</title>
<style>body{font:16px system-ui;margin:0;padding:32px;background:#f5f2ec;color:#1d1b19}button{font:inherit;padding:10px 18px;border-radius:8px;border:1px solid #b9b2a6;background:#fff}.card{margin-top:24px;padding:20px;border-radius:12px;background:#fff;max-width:520px}#grid{display:grid;grid-template-columns:repeat(8,46px);gap:6px;padding:0;margin:16px 0;list-style:none}#grid li{height:26px;background:#fff;border-radius:4px;text-align:center;font-size:12px;line-height:26px}iframe{width:320px;height:90px;border:1px solid #b9b2a6;background:#fff}</style></head><body>
<h1 id="title">Pick fixture</h1>
<button id="save" class="primary wide" onclick="saved++"><span>Save</span> changes</button>
<button id="count" onclick="counted++;this.textContent='Counted '+counted">Count</button>
<button id="delete" onclick="deleted++">Delete</button>
<input id="field" aria-label="Agent field">
<div class="card"><p>Shipping is free on orders over $50.</p></div>
<ul id="grid">${Array.from({ length: 24 }, (_, index) => `<li>${index + 1}</li>`).join("")}</ul>
<iframe id="embedded" title="Embedded form" src="${frame}"></iframe>
<a href="/other">Other page</a>
<script>
window.saved = 0; window.counted = 0; window.deleted = 0; window.heard = []; window.keys = [];
for (const type of ["pointerdown", "mousedown", "mouseup", "click", "dblclick", "contextmenu", "focusin"])
  addEventListener(type, (event) => { if (!["count", "field"].includes(event.target.id)) heard.push(type); }, true);
for (const type of ["keydown", "keyup"]) addEventListener(type, (event) => keys.push(type + ":" + event.key), true);
</script></body></html>`;
const framePage = `<!doctype html><html lang="en"><head><title>Embedded form</title></head><body style="margin:8px;font:14px system-ui">
<button id="framed" onclick="framed++;this.textContent='Framed '+framed">Frame button</button>
<script>window.framed = 0;</script></body></html>`;

async function tool(name: string, args: object = {}) {
  const sdk = new Haarness({ baseUrl: endpoint.url, token: endpoint.token });
  const result = await sdk.callBrowserTool(thread.id, { name: "browser_" + name, arguments: args as any }, { timeoutMs: 90_000 });
  sdk.close();
  if (result.isError) throw new Error(result.output);
  return JSON.parse(result.output);
}
const evaluate = async (script: string) => (await tool("evaluate", { tabID, script })).value;
/** The middle of an element of the page, in the view's pixels. */
async function center(selector: string) {
  return JSON.parse(
    await evaluate(
      `(() => { const r = document.querySelector(${JSON.stringify(selector)}).getBoundingClientRect(); return JSON.stringify({ x: Math.round(r.left + r.width / 2), y: Math.round(r.top + r.height / 2) }); })()`,
    ),
  ) as { x: number; y: number };
}
/** The person's pointer in the page: it moves there, then presses and releases. A
 * press carries no modifiers, so Shift comes with the move, as a hand holding it does. */
async function mouse(point: { x: number; y: number }, options: { press?: boolean; modifiers?: string[] } = {}) {
  await app.evaluate(
    async ({ webContents }, { url, point, press, modifiers }) => {
      const contents = webContents.getAllWebContents().find((item) => item.getURL().startsWith(url))!;
      contents.sendInputEvent({ type: "mouseMove", x: point.x, y: point.y, modifiers: modifiers as any });
      if (!press) return;
      // A hand moves, then presses: Chromium delivers moves on the next frame.
      await new Promise((done) => setTimeout(done, 100));
      contents.sendInputEvent({ type: "mouseDown", x: point.x, y: point.y, button: "left", clickCount: 1, modifiers: modifiers as any });
      contents.sendInputEvent({ type: "mouseUp", x: point.x, y: point.y, button: "left", clickCount: 1, modifiers: modifiers as any });
    },
    { url: siteURL, point, press: !!options.press, modifiers: options.modifiers ?? [] },
  );
}
/** Shift goes down or up where the keyboard is, as the person's key does. */
const shift = (type: "keyDown" | "keyUp") =>
  app.evaluate(({ webContents, BrowserWindow }, type) => {
    const focused = webContents.getFocusedWebContents() ?? BrowserWindow.getAllWindows()[0].webContents;
    focused.sendInputEvent({ type, keyCode: "Shift", modifiers: type === "keyDown" ? ["shift"] : [] });
  }, type);
const pickButton = () => page.getByRole("button", { name: "Pick an element", exact: true });
const messageBox = () => page.getByRole("textbox", { name: "Message" });
const chips = () => page.locator(".prompt-context-item");
/**
 * Pixels at CSS points of the page as the person sees it now (`"page"`) or
 * of a screenshot file, scaled to the page's CSS width.
 */
const sample = (source: string, points: { x: number; y: number }[], width: number) =>
  app.evaluate(
    async ({ nativeImage, webContents }, { source, points, width, url }) => {
      const image =
        source === "page"
          ? await webContents.getAllWebContents().find((item) => item.getURL().startsWith(url))!.capturePage()
          : nativeImage.createFromPath(source);
      const scaled = image.resize({ width, quality: "best" });
      const bitmap = scaled.toBitmap();
      const row = scaled.getSize().width;
      return points.map(({ x, y }) => Array.from(bitmap.subarray((y * row + x) * 4, (y * row + x) * 4 + 3)));
    },
    { source, points, width, url: siteURL },
  );
const distance = (a: number[], b: number[]) => Math.max(...a.map((value, index) => Math.abs(value - b[index])));
/** The person's keys reach the page, which has the keyboard, one at a time. */
const typePerson = (names: string[]) =>
  app.evaluate(async ({ webContents }, { url, names }) => {
    const contents = webContents.getAllWebContents().find((item) => item.getURL().startsWith(url))!;
    contents.focus();
    for (const keyCode of names) {
      contents.sendInputEvent({ type: "keyDown", keyCode });
      if (keyCode.length === 1 || keyCode === "Space") contents.sendInputEvent({ type: "char", keyCode: keyCode === "Space" ? " " : keyCode });
      contents.sendInputEvent({ type: "keyUp", keyCode });
      await new Promise((done) => setTimeout(done, 30));
    }
  }, { url: siteURL, names });
/**
 * What a field holds after the agent fills it with `texts` in turn: each fill
 * selects all, presses Backspace and types the text. Where the synthetic
 * select-all does not select (macOS, an unfocused window), Backspace takes
 * one character instead; either way each key and text landed exactly once.
 */
const filled = (start: string, texts: string[]) => [
  texts.at(-1)!,
  texts.reduce((value, text) => value.slice(0, -1) + text, start),
];
/** Whether the picker's outline is in the page now. */
const outlined = () => evaluate("!!document.querySelector('haarness-element-picker')");
/** The window as the person sees it: the app with the browser's page drawn in place. */
async function composite(name: string) {
  if (!shots) return;
  const capture = await app.evaluate(async ({ BrowserWindow }) => {
    const win = BrowserWindow.getAllWindows()[0];
    const views = [];
    for (const view of win.contentView.children) {
      if (!("webContents" in view) || !view.getVisible()) continue;
      const contents = view.webContents as Electron.WebContents;
      if (contents === win.webContents) continue;
      views.push({ bounds: view.getBounds(), data: (await contents.capturePage()).toDataURL() });
    }
    return { base: (await win.webContents.capturePage()).toDataURL(), width: win.getContentSize()[0], views };
  });
  const data = await page.evaluate(async (capture) => {
    const load = (src: string) =>
      new Promise<HTMLImageElement>((done, fail) => {
        const image = new Image();
        image.onload = () => done(image);
        image.onerror = fail;
        image.src = src;
      });
    const base = await load(capture.base);
    const canvas = document.createElement("canvas");
    canvas.width = base.naturalWidth;
    canvas.height = base.naturalHeight;
    const scale = base.naturalWidth / capture.width;
    const context = canvas.getContext("2d")!;
    context.drawImage(base, 0, 0);
    for (const view of capture.views)
      context.drawImage(await load(view.data), view.bounds.x * scale, view.bounds.y * scale, view.bounds.width * scale, view.bounds.height * scale);
    return canvas.toDataURL("image/png");
  }, capture);
  await mkdir(shots, { recursive: true });
  await writeFile(join(shots, name + ".png"), Buffer.from(data.split(",")[1], "base64"));
}
async function theme(value: "light" | "dark") {
  await app.evaluate(({ nativeTheme }, value) => {
    nativeTheme.themeSource = value;
  }, value);
  await page.emulateMedia({ colorScheme: value });
  await expect
    .poll(() => page.evaluate(() => matchMedia("(prefers-color-scheme: dark)").matches))
    .toBe(value === "dark");
  await page.waitForTimeout(300);
}

test.beforeAll(async () => {
  test.setTimeout(240_000);
  temporary = await realpath(await mkdtemp(join(process.env.HAARNESS_E2E_DIR ?? tmpdir(), "haarness-browser-pick-")));
  workspace = join(temporary, "workspace");
  await mkdir(join(workspace, ".haarness"), { recursive: true });
  await writeFile(join(workspace, ".haarness/config.toml"), '[defaults]\nprovider="echo"\nmodel="echo-1"\n');
  env = {
    ...Object.fromEntries(Object.entries(process.env).filter((x): x is [string, string] => x[1] !== undefined)),
    HAARNESS_BIN: binary,
    HAARNESS_HOME: join(temporary, "home"),
    HAARNESS_DATA_DIR: join(temporary, "state"),
    HAARNESS_DESKTOP_DATA: join(temporary, "desktop"),
    HAARNESS_TEST_PROVIDER: "1",
    HAARNESS_DESKTOP_TEST: "1",
  };
  delete env.ELECTRON_RUN_AS_NODE;
  delete env.ELECTRON_RENDERER_URL;
  site = createServer((request, response) => {
    response.setHeader("Content-Type", "text/html");
    const port = (site.address() as { port: number }).port;
    response.end(
      request.url === "/other"
        ? "<!doctype html><title>Other</title><h1>Other page</h1>"
        : request.url === "/frame"
          ? framePage
          : fixture(`http://localhost:${port}/frame`),
    );
  });
  await new Promise<void>((done) => site.listen(0, "127.0.0.1", done));
  siteURL = "http://127.0.0.1:" + (site.address() as any).port;
  app = await electron.launch({ args: ["."], env, timeout: 120_000 });
  page = await app.firstWindow();
  page.on("pageerror", (error) => errors.push(error.message));
  await app.evaluate(({ BrowserWindow, dialog }, directory) => {
    const win = BrowserWindow.getAllWindows()[0];
    win.setContentSize(1400, 900);
    win.show();
    dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [directory] });
  }, workspace);
  await page.getByRole("button", { name: "Open another project", exact: true }).click();
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
  thread = await client.createThread({ title: "Element notes", provider: "echo", model: "echo-1" });
  await page.locator(".sessions").getByRole("button", { name: "Element notes", exact: true }).click();
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
  stopChatHome(binary, env);
  if (temporary) await rm(temporary, { recursive: true, force: true }).catch(() => {});
});

test("a click picks the element under the pointer into the message without reaching the page", async () => {
  // Nothing to pick before a page is open.
  await page.evaluate(() => window.dispatchEvent(new Event("resize")));
  tabID = (await tool("tabs_open", { url: siteURL + "/" })).id;
  await expect(page.getByRole("region", { name: "Web browser" })).toBeVisible();
  await expect(pickButton()).toBeEnabled();
  await expect(pickButton()).toHaveAttribute("title", /Pick an element — /);
  await messageBox().fill("Make this ");
  await pickButton().click();
  await expect(pickButton()).toHaveAttribute("aria-pressed", "true");
  await expect.poll(outlined).toBe(true);
  // Hovering the label inside the button outlines the button itself.
  const label = await center("#save span");
  await mouse(label);
  await page.waitForTimeout(250);
  await theme("light");
  await composite("pick-hover-light");
  await theme("dark");
  await composite("pick-hover-dark");
  await mouse(label, { press: true });
  await expect(chips()).toHaveCount(1);
  await expect(chips().first()).toContainText("button#save.primary.wide “Save changes”");
  await expect(pickButton()).toHaveAttribute("aria-pressed", "false");
  // The note goes straight into the message box, after what was typed.
  await expect(messageBox()).toBeFocused();
  await page.keyboard.type("bigger");
  await expect(messageBox()).toHaveValue("Make this bigger");
  await expect(messageBox()).toHaveAttribute("placeholder", "Ask about the selected element…");
  // The page heard no press, click or focus, and its button did not run.
  expect(JSON.parse(await evaluate("JSON.stringify({ heard, saved })"))).toEqual({ heard: [], saved: 0 });
  // The outline lingers a moment as taken, then goes.
  await expect.poll(outlined).toBe(false);
  // Hovering the chip says what and where; opening it shows what was captured.
  const summary = chips().first().locator("summary");
  await expect(summary).toHaveAttribute("title", new RegExp(`button#save\\.primary\\.wide · \\d+×\\d+\\n127\\.0\\.0\\.1:\\d+\\n#save`));
  await summary.click();
  const preview = chips().first().locator(".prompt-context-preview");
  await expect(preview).toBeVisible();
  await expect(preview).toContainText("Picked page element · ");
  await expect(preview).toContainText(siteURL + "/");
  await expect(preview).toContainText("#save");
  await expect(preview.locator("pre")).toContainText('<button id="save" class="primary wide" onclick="saved++"><span>Save</span> changes</button>');
  await theme("light");
  await composite("pick-chip-light");
  await theme("dark");
  await composite("pick-chip-dark");
  await summary.click();
  // The chip goes with the message: the model reads the element as page data.
  await messageBox().press("Enter");
  await expect(chips()).toHaveCount(0);
  await expect
    .poll(async () => {
      const items = await client.listItems(thread.id);
      return (items as any[]).map((item) => item.data).find((data) => data.type === "user_message")?.text ?? "";
    })
    .toContain("<summary>Page element: button#save.primary.wide “Save changes” (snapshot)</summary>");
  const sent = ((await client.listItems(thread.id)) as any[]).map((item) => item.data).find((data) => data.type === "user_message").text as string;
  expect(sent.startsWith("Make this bigger\n\n<details>")).toBe(true);
  expect(sent).toContain(`(tab ${tabID})`);
  expect(sent).toContain('- Selector: "#save"');
  expect(sent).toContain('```html\n<button id="save" class="primary wide" onclick="saved++"><span>Save</span> changes</button>\n```');
  // The bubble folds the element the way it folds a file snapshot.
  await expect(page.locator(".message.user summary").filter({ hasText: "Page element: button#save.primary.wide" })).toBeVisible();
});

test("Esc stops picking without adding anything, and Shift-click picks several", async () => {
  await pickButton().click();
  await expect(pickButton()).toHaveAttribute("aria-pressed", "true");
  await page.keyboard.press("Escape");
  await expect(pickButton()).toHaveAttribute("aria-pressed", "false");
  await expect.poll(outlined).toBe(false);
  await expect(chips()).toHaveCount(0);
  // The Escape stopped picking only: no turn was stopped, nothing was sent.
  await pickButton().click();
  await mouse(await center("#title"), { press: true, modifiers: ["shift"] });
  await expect(chips()).toHaveCount(1);
  await expect(pickButton()).toHaveAttribute("aria-pressed", "true");
  // Shift held in the window while the pointer rests still keeps picking too.
  await shift("keyDown");
  await mouse(await center("#count"), { press: true });
  await shift("keyUp");
  await expect(chips()).toHaveCount(2);
  await expect(pickButton()).toHaveAttribute("aria-pressed", "true");
  await mouse(await center(".card p"), { press: true });
  await expect(chips()).toHaveCount(3);
  await expect(chips().nth(2)).toContainText("p");
  await expect(pickButton()).toHaveAttribute("aria-pressed", "false");
  await expect(messageBox()).toHaveAttribute("placeholder", "Ask about the selected elements…");
  // Picking the same element again leaves one chip.
  await pickButton().click();
  await mouse(await center("#title"), { press: true });
  await expect(chips()).toHaveCount(3);
  expect(JSON.parse(await evaluate("JSON.stringify({ heard, saved, counted })"))).toEqual({ heard: [], saved: 0, counted: 0 });
  // × removes a chip.
  for (const left of [2, 1, 0]) {
    await chips().first().getByRole("button", { name: /^Remove context/ }).click();
    await expect(chips()).toHaveCount(left);
  }
});

test("the agent's own clicks still reach the page while the person picks", async () => {
  await pickButton().click();
  await expect(pickButton()).toHaveAttribute("aria-pressed", "true");
  const snapshot = (await tool("snapshot", { tabID })).content as string;
  const ref = snapshot.split("\n").find((line) => line.includes('"Count"'))?.match(/@e\d+/)?.[0];
  expect(ref).toBeTruthy();
  await tool("click", { tabID, ref });
  expect(await evaluate("counted")).toBe(1);
  await expect(pickButton()).toHaveAttribute("aria-pressed", "true");
  // The agent's screenshot never shows the person's outline, which the
  // person sees around the button at that moment.
  const box = JSON.parse(
    await evaluate("(() => { const r = save.getBoundingClientRect(); return JSON.stringify({ left: r.left, y: r.top + r.height / 2, width: innerWidth }); })()"),
  ) as { left: number; y: number; width: number };
  await mouse(await center("#save"));
  await page.waitForTimeout(300);
  const ring = { x: Math.round(box.left - 1), y: Math.round(box.y) };
  const background = { x: Math.round(box.left - 20), y: Math.round(box.y) };
  const seen = await sample("page", [ring, background], box.width);
  const shot = await tool("screenshot", { tabID });
  const taken = await sample(shot.files[0].path, [ring, background], box.width);
  expect(distance(seen[0], seen[1])).toBeGreaterThan(40);
  expect(distance(taken[0], taken[1])).toBeLessThan(16);
  await expect.poll(outlined).toBe(true);
  // A page that goes away ends picking.
  await tool("navigate", { tabID, url: siteURL + "/other" });
  await expect(pickButton()).toHaveAttribute("aria-pressed", "false");
  await tool("navigate", { tabID, url: siteURL + "/" });
  await expect(pickButton()).toBeEnabled();
});

test("while picking the page hears none of the person's keys, and Escape's release stays with the picker", async () => {
  await evaluate("keys.length = 0");
  await pickButton().click();
  await expect(pickButton()).toHaveAttribute("aria-pressed", "true");
  // Space, Enter and letters would press what the page focused.
  await typePerson(["Space", "Enter", "d"]);
  expect(await evaluate("JSON.stringify(keys)")).toBe("[]");
  await typePerson(["Escape"]);
  await expect(pickButton()).toHaveAttribute("aria-pressed", "false");
  await expect.poll(outlined).toBe(false);
  expect(await evaluate("JSON.stringify(keys)")).toBe("[]");
  // Not picking, the page has its keys again.
  await typePerson(["d"]);
  await expect.poll(() => evaluate("keys.join(' ')")).toContain("keydown:d");
  expect(JSON.parse(await evaluate("JSON.stringify({ saved, deleted })"))).toEqual({ saved: 0, deleted: 0 });
});

test("a frame is picked as a whole, and the agent's click inside one lands while the person picks", async () => {
  const frame = (await tool("frames", { tabID })).frames.find((item: { url: string }) => item.url.endsWith("/frame"));
  expect(frame).toBeTruthy();
  await pickButton().click();
  await expect(pickButton()).toHaveAttribute("aria-pressed", "true");
  // The person's press over the frame picks the frame, and the button inside it does not run.
  await mouse(await center("#embedded"), { press: true, modifiers: ["shift"] });
  await expect(chips()).toHaveCount(1);
  await expect(chips().first()).toContainText("iframe#embedded");
  expect((await tool("evaluate", { tabID, frameID: frame.id, script: "framed" })).value).toBe(0);
  // The agent clicks the button inside the frame, and it runs.
  const snapshot = (await tool("snapshot", { tabID, frameID: frame.id })).content as string;
  const ref = snapshot.split("\n").find((line) => line.includes('"Frame button"'))?.match(/@e\d+/)?.[0];
  expect(ref).toBeTruthy();
  await tool("click", { tabID, ref });
  expect((await tool("evaluate", { tabID, frameID: frame.id, script: "framed" })).value).toBe(1);
  await expect(pickButton()).toHaveAttribute("aria-pressed", "true");
  // The frame ignores the pointer again: the next press over it picks it again.
  await mouse(await center("#embedded"), { press: true });
  await expect(pickButton()).toHaveAttribute("aria-pressed", "false");
  expect((await tool("evaluate", { tabID, frameID: frame.id, script: "framed" })).value).toBe(1);
  await chips().first().getByRole("button", { name: /^Remove context/ }).click();
  await expect(chips()).toHaveCount(0);
});

test("the person's clicks while the agent types pick instead of pressing, and Escape still stops", async () => {
  const snapshot = (await tool("snapshot", { tabID })).content as string;
  const field = snapshot.split("\n").find((line) => line.includes('"Agent field"'))?.match(/@e\d+/)?.[0];
  expect(field).toBeTruthy();
  await pickButton().click();
  await expect(pickButton()).toHaveAttribute("aria-pressed", "true");
  const target = await center("#delete");
  // The agent types again and again; meanwhile the person presses Delete as
  // fast as the system lets them, with Shift, so they keep picking.
  let typing = true;
  const agent = (async () => {
    try {
      for (let round = 0; round < 12; round++) await tool("fill", { tabID, ref: field, text: "agent text " + round });
    } finally {
      typing = false;
    }
  })();
  await app.evaluate(async ({ webContents }, { url, target }) => {
    const contents = webContents.getAllWebContents().find((item) => item.getURL().startsWith(url))!;
    contents.sendInputEvent({ type: "mouseMove", x: target.x, y: target.y, modifiers: ["shift"] });
    await new Promise((done) => setTimeout(done, 100));
  }, { url: siteURL, target });
  while (typing)
    await app.evaluate(async ({ webContents }, { url, target }) => {
      const contents = webContents.getAllWebContents().find((item) => item.getURL().startsWith(url))!;
      for (let press = 0; press < 25; press++) {
        contents.sendInputEvent({ type: "mouseDown", x: target.x, y: target.y, button: "left", clickCount: 1 });
        contents.sendInputEvent({ type: "mouseUp", x: target.x, y: target.y, button: "left", clickCount: 1 });
        await new Promise((done) => setTimeout(done, 2));
      }
    }, { url: siteURL, target });
  await agent;
  expect(await evaluate("deleted")).toBe(0);
  // Every key and every text the agent sent landed once.
  expect(filled("", Array.from({ length: 12 }, (_, round) => "agent text " + round))).toContain(
    await evaluate("document.querySelector('#field').value"),
  );
  await expect(chips()).toHaveCount(1);
  await expect(chips().first()).toContainText("button#delete “Delete”");
  await expect(pickButton()).toHaveAttribute("aria-pressed", "true");
  // The person's Escape stops picking in the middle of the agent's typing,
  // and the agent's typing goes on to the end.
  await evaluate("keys.length = 0");
  const start = await evaluate("document.querySelector('#field').value");
  const more = (async () => {
    for (let round = 0; round < 6; round++) await tool("fill", { tabID, ref: field, text: "more text " + round });
  })();
  await page.waitForTimeout(150);
  await typePerson(["Escape"]);
  await expect(pickButton()).toHaveAttribute("aria-pressed", "false");
  await more;
  expect(filled(start, Array.from({ length: 6 }, (_, round) => "more text " + round))).toContain(
    await evaluate("document.querySelector('#field').value"),
  );
  expect(await evaluate("keys.filter((key) => key.endsWith(':Escape')).length")).toBe(0);
  await chips().first().getByRole("button", { name: /^Remove context/ }).click();
  await expect(chips()).toHaveCount(0);
});

test("a page's worth of elements can be picked into one message", async () => {
  await pickButton().click();
  for (let cell = 1; cell <= 21; cell++)
    await mouse(await center(`#grid li:nth-of-type(${cell})`), { press: true, modifiers: ["shift"] });
  await expect(chips()).toHaveCount(21);
  await expect(page.locator(".notice-inline, [role=alert]").filter({ hasText: /context/i })).toHaveCount(0);
  await page.keyboard.press("Escape");
  await expect(pickButton()).toHaveAttribute("aria-pressed", "false");
  await messageBox().fill("Tidy these");
  await messageBox().press("Enter");
  await expect(chips()).toHaveCount(0);
  await expect
    .poll(async () => {
      const items = await client.listItems(thread.id);
      const sent = (items as any[]).map((item) => item.data).filter((data) => data.type === "user_message").at(-1)?.text ?? "";
      return sent.split("<summary>Page element: li").length - 1;
    })
    .toBe(21);
});

test("the shortcut's menu item and the command palette start picking, and stop it when asked again", async () => {
  const clickMenu = () =>
    app.evaluate(({ Menu, BrowserWindow }) => {
      const find = (items: Electron.MenuItem[]): Electron.MenuItem | undefined => {
        for (const item of items) {
          if (item.label === "Pick an element") return item;
          const inner = item.submenu && find(item.submenu.items);
          if (inner) return inner;
        }
      };
      const item = find(Menu.getApplicationMenu()!.items)!;
      item.click(undefined, BrowserWindow.getAllWindows()[0], undefined as any);
      return item.accelerator;
    });
  expect(await clickMenu()).toBe("CmdOrCtrl+Shift+C");
  await expect(pickButton()).toHaveAttribute("aria-pressed", "true");
  await clickMenu();
  await expect(pickButton()).toHaveAttribute("aria-pressed", "false");
  // Closed, the Browser opens on the request and starts picking.
  await page.getByRole("button", { name: "Close Browser panel", exact: true }).click();
  await expect(page.getByRole("region", { name: "Web browser" })).toBeHidden();
  await page.getByRole("button", { name: "Search", exact: true }).click();
  await page.getByRole("combobox", { name: "Search Haarness", exact: true }).fill("Pick an element");
  await page.getByRole("option", { name: /Select part of the page/ }).click();
  await expect(page.getByRole("region", { name: "Web browser" })).toBeVisible();
  await expect(pickButton()).toHaveAttribute("aria-pressed", "true");
  await page.keyboard.press("Escape");
  await expect(pickButton()).toHaveAttribute("aria-pressed", "false");
  await theme("light");
  await pickButton().click();
  await composite("pick-toolbar-light");
  await page.keyboard.press("Escape");
  await theme("dark");
  await pickButton().click();
  await composite("pick-toolbar-dark");
  await page.keyboard.press("Escape");
  expect(errors).toEqual([]);
});
