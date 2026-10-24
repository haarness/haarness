import { test, expect, _electron as electron, type ElectronApplication, type Page, type FrameLocator } from "@playwright/test";
import { Haarness } from "@haarness/sdk";
import { mkdtemp, mkdir, writeFile, realpath, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";
import { stopChatHome } from "./fixtures/chat-home";

// MCP Apps: a tool an MCP server pairs with a ui:// page shows that page under its row, in a
// sandboxed frame. The page gets the tool's input and result and the theme; it may open links (at
// once after a click in it, otherwise once the person agrees), put a message in the message box
// and read its own page; it may not call tools, read other resources, reach undeclared hosts or
// leave its frame.
test.describe.configure({ mode: "serial" });

let app: ElectronApplication, page: Page, client: Haarness;
let temporary: string, workspace: string, binary: string, env: Record<string, string>;
let thread = "";
const errors: string[] = [];
const fixture = resolve("../../crates/haarness-mcp/tests/fixtures/app.py");

type Calls = { shell: unknown[][] };

test.beforeAll(async () => {
  test.setTimeout(240000);
  temporary = await realpath(await mkdtemp(join(process.env.HAARNESS_E2E_DIR ?? tmpdir(), "haarness-mcp-apps-")));
  workspace = join(temporary, "workspace");
  await mkdir(join(workspace, ".haarness"), { recursive: true });
  await writeFile(
    join(workspace, ".haarness/config.toml"),
    '[defaults]\nprovider="echo"\nmodel="echo-1"\n\n' +
      `[mcp.app]\ntransport = "stdio"\ncommand = "python3"\nargs = [${JSON.stringify(fixture)}]\nenabled = true\n\n` +
      // The same fixture again: its tools name pages of the first one.
      `[mcp.evil]\ntransport = "stdio"\ncommand = "python3"\nargs = [${JSON.stringify(fixture)}]\nenabled = true\n`,
  );
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
  // Record links instead of opening them.
  await app.evaluate(({ dialog, shell }, directory) => {
    const calls: Calls = { shell: [] };
    (globalThis as unknown as { calls: Calls }).calls = calls;
    dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [directory] });
    shell.openExternal = async (...args: unknown[]) => void calls.shell.push(["openExternal", ...args]);
  }, workspace);
  await page.getByRole("button", { name: "Open another project", exact: true }).click();
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
    .poll(async () => (await client.mcpServers()).filter((server) => server.status === "connected").map((server) => server.name).sort(), { timeout: 30000 })
    .toEqual(["app", "evil"]);
  thread = (await client.createThread({ title: "App fixture", provider: "echo", model: "echo-1" })).id;
  await page.locator(".sessions").getByRole("button", { name: "App fixture", exact: true }).click();
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
  stopChatHome(binary, env);
  if (temporary) await rm(temporary, { recursive: true, force: true }).catch(() => {});
});

const calls = () => app.evaluate(() => (globalThis as unknown as { calls: Calls }).calls);
const toast = () => page.locator(".notifications .toast");

async function settled() {
  await expect.poll(async () => (await client.activeTurns()).length, { timeout: 30000 }).toBe(0);
}
/** Calls the tool through the echo provider and returns the newest page under a row. */
async function show(argumentsJson: string) {
  await settled();
  const turn = await client.startTurn(thread, {
    requestId: crypto.randomUUID(),
    text: `!tool mcp__app__show_weather ${argumentsJson}`,
  });
  await client.waitTurn(turn.id);
  const view = page.locator(".mcp-app").last();
  await expect(view).toHaveAttribute("data-state", "ready", { timeout: 30000 });
  return view;
}
const content = (view: ReturnType<Page["locator"]>): FrameLocator =>
  view.frameLocator("iframe").frameLocator("iframe");
/**
 * Waits until the page's frame stops moving. An inline notice or request
 * appearing or leaving under a page makes the conversation follow its end a
 * frame or two later, and Playwright measures a press inside the nested frame,
 * which does not see the frame itself move: the press would land on the
 * page's empty body.
 */
async function steady(view: ReturnType<Page["locator"]>) {
  const frame = view.locator("iframe");
  let last = "";
  await expect
    .poll(
      async () => {
        await page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done))));
        const box = JSON.stringify(await frame.boundingBox());
        const same = box === last;
        last = box;
        return same;
      },
      { intervals: [50] },
    )
    .toBe(true);
}

test("the page shows the tool's input and result in the app's theme and follows it", async ({}, info) => {
  const view = await show('{"city":"Porto"}');
  const frame = content(view);
  await expect(frame.locator("#input")).toHaveText('input {"city":"Porto"}');
  await expect(frame.locator("#result")).toContainText('"proof":"mcp-app-result-proof"');
  await expect(frame.locator("#host")).toHaveText("host en inline desktop show_weather");
  await expect(frame.locator("#theme")).toHaveText(/^theme (light|dark) styled$/);
  await expect(view.locator("iframe")).toHaveAttribute("title", "Show the weather app");
  await expect(view.locator("iframe")).toHaveAttribute("sandbox", "allow-scripts allow-forms");
  // The frame takes the height the page reported, within the inline limit.
  const height = (await view.locator(".mcp-app-frame").boundingBox())!.height;
  expect(height).toBeGreaterThan(200);
  expect(height).toBeLessThanOrEqual(642);
  await page.evaluate(() => window.haarness.preferences({ theme: "dark" }));
  await expect(frame.locator("#theme")).toHaveText("theme dark");
  await view.screenshot({ path: info.outputPath("mcp-app-dark.png") });
  await page.evaluate(() => window.haarness.preferences({ theme: "light" }));
  await expect(frame.locator("#theme")).toHaveText("theme light");
  await view.screenshot({ path: info.outputPath("mcp-app-light.png") });
  expect(errors).toEqual([]);
});

test("a link opens at once after a click in the page, and otherwise only once the person agrees", async () => {
  const view = page.locator(".mcp-app").last();
  const frame = content(view);
  await steady(view);
  await frame.getByRole("button", { name: "Open forecast" }).click();
  await expect(frame.locator("#status")).toHaveText("link {}");
  await expect.poll(async () => (await calls()).shell).toEqual([["openExternal", "https://example.com/forecast"]]);

  // A page that asks by itself waits for the person: Dismiss refuses it.
  const asking = await show('{"city":"Braga","autolink":true}');
  const request = asking.getByRole("group", { name: "Link from the app" });
  await expect(request).toContainText("This app wants to open a link.");
  await expect(request).toContainText("https://example.com/auto");
  await request.getByRole("button", { name: "Dismiss", exact: true }).click();
  await expect(content(asking).locator("#status")).toHaveText('autolink {"isError":true}');
  await expect(request).toHaveCount(0);
  expect((await calls()).shell).toHaveLength(1);

  // Open sends it through the app's link check.
  const again = await show('{"city":"Faro","autolink":true}');
  await again.getByRole("group", { name: "Link from the app" }).getByRole("button", { name: "Open", exact: true }).click();
  await expect(content(again).locator("#status")).toHaveText("autolink {}");
  await expect.poll(async () => (await calls()).shell).toEqual([
    ["openExternal", "https://example.com/forecast"],
    ["openExternal", "https://example.com/auto"],
  ]);
});

test("a message from the page goes into the message box and is never sent by itself", async () => {
  const view = page.locator(".mcp-app").last();
  const before = (await client.listItems(thread)).length;
  const box = page.getByRole("textbox", { name: "Message" });
  await box.fill("Draft first");
  await steady(view);
  await content(view).getByRole("button", { name: "Ask about tomorrow" }).click();
  await expect(content(view).locator("#status")).toHaveText("message {}");
  await expect(box).toHaveValue("Draft first\n\nWhat about tomorrow in Lisbon?");
  await expect(toast()).toHaveText("The app's message is in the message box. Send it when you are ready.");
  await page.waitForTimeout(500);
  expect((await client.activeTurns()).length).toBe(0);
  expect((await client.listItems(thread)).length).toBe(before);
  await box.fill("");
});

test("one click lets the page send one message; a second waits for the person", async () => {
  const view = page.locator(".mcp-app").last();
  const box = page.getByRole("textbox", { name: "Message" });
  await box.fill("");
  await steady(view);
  await content(view).getByRole("button", { name: "Ask twice" }).click();
  await expect(content(view).locator("#status")).toHaveText("first {}");
  await expect(box).toHaveValue("First of two.");
  const request = view.getByRole("group", { name: "Message from the app" });
  await expect(request).toContainText("Second of two.");
  await request.getByRole("button", { name: "Dismiss", exact: true }).click();
  await expect(content(view).locator("#result")).toHaveText('second {"isError":true}');
  await expect(box).toHaveValue("First of two.");
  await box.fill("");
  expect(errors).toEqual([]);
});

test("one click lets the page open one link; a second waits for the person", async () => {
  const view = page.locator(".mcp-app").last();
  const before = (await calls()).shell.length;
  await steady(view);
  await content(view).getByRole("button", { name: "Open twice" }).click();
  await expect(content(view).locator("#status")).toHaveText("first link {}");
  const request = view.getByRole("group", { name: "Link from the app" });
  await expect(request).toContainText("https://example.com/second");
  expect((await calls()).shell.slice(before)).toEqual([["openExternal", "https://example.com/first"]]);
  await request.getByRole("button", { name: "Dismiss", exact: true }).click();
  await expect(content(view).locator("#result")).toHaveText('second link {"isError":true}');
  expect((await calls()).shell.slice(before)).toEqual([["openExternal", "https://example.com/first"]]);
  expect(errors).toEqual([]);
});

test("a link the app may not open is refused, with a notice naming its scheme only after a press in the page", async () => {
  const before = (await calls()).shell.length;
  // Asked by the page on its own: refused, and nothing shows, so a page cannot repeat or word a notice.
  const view = await show('{"city":"Tavira","autofile":true}');
  await expect(content(view).locator("#status")).toHaveText(
    "autofile error Haarness opens only http, https and mailto links from an app.",
  );
  await page.waitForTimeout(300);
  await expect(view.locator(".mcp-app-refused")).toHaveCount(0);
  await steady(view);
  // After a click in the page, the person who clicked sees why nothing opened. A scheme that is
  // not a well-known one could be words the page chose, so the notice names none.
  await content(view).getByRole("button", { name: "Open a worded link" }).click();
  await expect(content(view).locator("#status")).toHaveText(
    "worded link error Haarness opens only http, https and mailto links from an app.",
  );
  const notice = view.locator(".mcp-app-refused");
  await expect(notice).toHaveText(
    /^This app tried to open a link Haarness does not open\. Apps may open only http, https and mailto links\.\s*Dismiss$/,
  );
  await expect(notice).not.toContainText("haarness-help");
  // A well-known scheme is named; the rest of the address, the page's text, is not.
  await steady(view);
  await content(view).getByRole("button", { name: "Open a file link" }).click();
  await expect(content(view).locator("#status")).toHaveText(
    "file link error Haarness opens only http, https and mailto links from an app.",
  );
  await expect(notice).toHaveText(
    /^This app tried to open a file: link\. Apps may open only http, https and mailto links\.\s*Dismiss$/,
  );
  await expect(notice).not.toContainText("/etc/hosts");
  await notice.getByRole("button", { name: "Dismiss", exact: true }).click();
  await expect(notice).toHaveCount(0);
  expect((await calls()).shell.slice(before)).toEqual([]);
  expect(errors).toEqual([]);
});

test("a theme change outside the colours, such as the font, reaches the page", async () => {
  const view = page.locator(".mcp-app").last();
  await page.evaluate(() => document.documentElement.style.setProperty("--sans", "Georgia, serif"));
  await expect(content(view).locator("#font")).toHaveText("font Georgia, serif");
  await page.evaluate(() => document.documentElement.style.removeProperty("--sans"));
  await expect(content(view).locator("#font")).not.toHaveText("font Georgia, serif");
  expect(errors).toEqual([]);
});

test("a message the page writes by itself waits for the person and leaves the draft and focus alone", async () => {
  const box = page.getByRole("textbox", { name: "Message" });
  await box.fill("Keep this draft");
  await box.focus();
  const view = await show('{"city":"Evora","automessage":true}');
  const request = view.getByRole("group", { name: "Message from the app" });
  await expect(request).toContainText("This app wants to write a message for you.");
  await expect(request).toContainText("Automatic message from the page.");
  await page.waitForTimeout(300);
  await expect(box).toHaveValue("Keep this draft");
  await expect(box).toBeFocused();
  await request.getByRole("button", { name: "Insert app message", exact: true }).click();
  await expect(content(view).locator("#status")).toHaveText("automessage {}");
  await expect(box).toHaveValue("Keep this draft\n\nAutomatic message from the page.");
  await expect(request).toHaveCount(0);
  expect((await client.activeTurns()).length).toBe(0);

  // Dismiss refuses it and changes nothing.
  await box.fill("");
  const again = await show('{"city":"Beja","automessage":true}');
  await again.getByRole("group", { name: "Message from the app" }).getByRole("button", { name: "Dismiss", exact: true }).click();
  await expect(content(again).locator("#status")).toHaveText('automessage {"isError":true}');
  await expect(box).toHaveValue("");
  expect(errors).toEqual([]);
});

test("the page cannot call tools, read other resources, reach undeclared hosts or leave its frame", async () => {
  const view = page.locator(".mcp-app").last();
  const frame = content(view);
  // The bar the previous test dismissed can still be leaving: the conversation
  // may move under the first click, so the click is repeated until it lands.
  await steady(view);
  await expect(async () => {
    await frame.getByRole("button", { name: "Refresh" }).click();
    await expect(frame.locator("#status")).toHaveText(
      "call error Haarness does not let apps call tools yet. Ask in the chat instead.",
      { timeout: 2000 },
    );
  }).toPass({ timeout: 15000 });
  await frame.getByRole("button", { name: "Read own page" }).click();
  await expect(frame.locator("#status")).toHaveText("read 1 contents");
  await frame.getByRole("button", { name: "Read another resource" }).click();
  await expect(frame.locator("#status")).toHaveText("read error An app may read only ui:// resources of its own server.");
  await frame.getByRole("button", { name: "Fetch undeclared" }).click();
  await expect(frame.locator("#status")).toHaveText("blocked connect-src");
  // The page-only tool never reached the model's tool list either.
  expect((await client.mcpTools("app")).map((tool) => tool.name).sort()).toEqual([
    "mcp__app__forge",
    "mcp__app__plain",
    "mcp__app__show_weather",
  ]);

  await frame.getByRole("button", { name: "Leave the page" }).click();
  await expect(view).toHaveAttribute("data-state", "left");
  await expect(view.locator(".mcp-app-notice")).toContainText("This app tried to leave its page, so Haarness closed it.");
  await view.getByRole("button", { name: "Try again", exact: true }).click();
  await expect(view).toHaveAttribute("data-state", "ready", { timeout: 30000 });
  await expect(content(view).locator("#input")).toHaveText('input {"city":"Beja","automessage":true}');
  // Only the three links the person allowed ever reached the system.
  expect((await calls()).shell).toHaveLength(3);
  expect(errors).toEqual([]);
});

test("a tool cannot show another server's page by naming it", async () => {
  await settled();
  const turn = await client.startTurn(thread, { requestId: crypto.randomUUID(), text: "!tool mcp__evil__forge {}" });
  await client.waitTurn(turn.id);
  // The forging call's own row shows the call and no page.
  const row = page.locator("[data-item-id]").filter({ has: page.locator(".tool-call", { hasText: "mcp__evil__forge" }) });
  await expect(row).toHaveCount(1);
  await page.waitForTimeout(500);
  await expect(row.locator(".mcp-app")).toHaveCount(0);
  expect(errors).toEqual([]);
});

test("scrolling over a page scrolls the conversation", async () => {
  const transcript = page.locator(".transcript");
  await transcript.evaluate((element) => (element.scrollTop = element.scrollHeight));
  const bottom = await transcript.evaluate((element) => element.scrollTop);
  expect(bottom).toBeGreaterThan(0);
  const box = (await page.locator(".mcp-app iframe").last().boundingBox())!;
  await page.mouse.move(box.x + box.width / 2, box.y + Math.min(box.height / 2, 120));
  await page.mouse.wheel(0, -400);
  await expect.poll(() => transcript.evaluate((element) => element.scrollTop)).toBeLessThan(bottom - 100);
});
