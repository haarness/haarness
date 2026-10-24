import { test, expect, _electron as electron, type ElectronApplication, type Page } from "@playwright/test";
import { Haarness } from "@haarness/sdk";
import { mkdtemp, mkdir, writeFile, realpath, rm } from "node:fs/promises";
import { createServer, type IncomingHttpHeaders, type Server } from "node:http";
import type { AddressInfo } from "node:net";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";
import { stopChatHome } from "./fixtures/chat-home";

// An image a reply links to on another host loads only when the person asks:
// until then the reply shows a placeholder naming the host, and nothing
// reaches it. The app fetches the image itself, without cookies or a
// referrer, and the page's CSP still loads no remote image. Markup that copies
// a placeholder loads nothing. The fixture host names are pinned to this
// computer through the test-only `haarnessImageHosts`.
test.describe.configure({ mode: "serial" });

let app: ElectronApplication, page: Page, client: Haarness;
let temporary: string, workspace: string, binary: string, env: Record<string, string>;
let server: Server, port = 0, thread = "";
const requests: { url: string; headers: IncomingHttpHeaders }[] = [];
const errors: string[] = [];
const PNG = Buffer.from(
  "iVBORw0KGgoAAAANSUhEUgAAAAgAAAAICAYAAADED76LAAAAEUlEQVR4nGNgaGj4jxePDAUAhDd/wb3aPiAAAAAASUVORK5CYII=",
  "base64",
);

test.beforeAll(async () => {
  test.setTimeout(240000);
  server = createServer((request, response) => {
    requests.push({ url: request.url ?? "", headers: request.headers });
    if (request.url?.startsWith("/slow.png")) {
      // Slow enough for a reply to be drawn again while it loads.
      return void setTimeout(() => {
        response.writeHead(200, { "content-type": "image/png" });
        response.end(PNG);
      }, 4000);
    }
    if (request.url?.startsWith("/chart.png") || request.url === "/badge.png") {
      response.writeHead(200, { "content-type": "image/png", "set-cookie": "track=1" });
      return response.end(PNG);
    }
    response.writeHead(200, { "content-type": "text/html" });
    response.end("<p>not an image</p>");
  });
  await new Promise<void>((done) => server.listen(0, "127.0.0.1", done));
  port = (server.address() as AddressInfo).port;
  temporary = await realpath(await mkdtemp(join(process.env.HAARNESS_E2E_DIR ?? tmpdir(), "haarness-remote-images-")));
  workspace = join(temporary, "workspace");
  await mkdir(join(workspace, ".haarness"), { recursive: true });
  await writeFile(join(workspace, ".haarness/config.toml"), '[defaults]\nprovider="echo"\nmodel="echo-1"\n');
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
  await app.evaluate(({ dialog }, directory) => {
    dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [directory] });
    (globalThis as { haarnessImageHosts?: Record<string, string> }).haarnessImageHosts = {
      "images.test": "127.0.0.1",
      "badges.test": "127.0.0.1",
    };
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
  thread = (await client.createThread({ title: "Remote images", provider: "echo", model: "echo-1" })).id;
  await page.locator(".sessions").getByRole("button", { name: "Remote images", exact: true }).click();
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
  server?.closeAllConnections();
  await new Promise((done) => (server ? server.close(done) : done(undefined)));
  if (temporary) await rm(temporary, { recursive: true, force: true }).catch(() => {});
});

/** Sends `text`, which the echo provider repeats as the reply, and returns that reply. */
async function reply(text: string) {
  await expect.poll(async () => (await client.activeTurns()).length, { timeout: 30000 }).toBe(0);
  const before = await page.locator(".message.assistant").count();
  const turn = await client.startTurn(thread, { requestId: crypto.randomUUID(), text });
  await client.waitTurn(turn.id);
  await expect(page.locator(".message.assistant")).toHaveCount(before + 1);
  return page.locator(".message.assistant").last();
}

test("an image from another host waits for a click, then loads through the app without cookies or a referrer", async ({}, info) => {
  const message = await reply(
    `The chart: ![Weekly chart](http://images.test:${port}/chart.png?d=secret) and a badge ![](http://badges.test:${port}/badge.png)`,
  );
  const chart = message.locator(".remote-image").first();
  await expect(chart.locator(".remote-image-alt")).toHaveText("Weekly chart");
  const load = chart.getByRole("button", { name: "Load image from images.test", exact: true });
  await expect(load).toBeVisible();
  // An image with no alt text still says what it is.
  await expect(message.locator(".remote-image").nth(1).locator(".remote-image-alt")).toHaveText("Image");
  // Nothing reached either host, and no remote image is in the page.
  await page.waitForTimeout(500);
  expect(requests).toEqual([]);
  await expect(page.locator('.markdown img[src^="http"]')).toHaveCount(0);
  await message.screenshot({ path: info.outputPath("remote-image-placeholder.png") });

  await load.click();
  const image = message.locator("img.remote-image-loaded");
  await expect(image).toHaveCount(1);
  await expect(image).toHaveAttribute("src", /^data:image\/png;base64,/);
  await expect(image).toHaveAttribute("alt", "Weekly chart");
  expect(await image.evaluate((element: HTMLImageElement) => element.complete && element.naturalWidth)).toBe(8);
  // Only the host the person chose was asked, once, and it learned nothing about the app.
  expect(requests.map((request) => request.url)).toEqual(["/chart.png?d=secret"]);
  expect(requests[0].headers.cookie).toBeUndefined();
  expect(requests[0].headers.referer).toBeUndefined();
  await expect(message.getByRole("button", { name: "Load image from badges.test", exact: true })).toBeVisible();
  await message.screenshot({ path: info.outputPath("remote-image-loaded.png") });

  // The same image later in this chat shows at once, without asking the host again.
  const again = await reply(`Again: ![Weekly chart](http://images.test:${port}/chart.png?d=secret)`);
  await expect(again.locator("img.remote-image-loaded")).toHaveAttribute("src", /^data:image\/png;base64,/);
  await expect(again.locator(".remote-image")).toHaveCount(0);
  expect(requests).toHaveLength(1);
  expect(errors).toEqual([]);
});

test("an address that is not an image, or is on this computer, is refused and says so", async () => {
  const message = await reply(
    `![Page](http://images.test:${port}/page.html) ![Local](http://127.0.0.1:${port}/chart.png)`,
  );
  const before = requests.length;
  await message.getByRole("button", { name: "Load image from images.test", exact: true }).click();
  await expect(page.locator(".notifications .toast").last()).toHaveText("The image from images.test could not be loaded.");
  await expect(message.locator(".remote-image")).toHaveCount(2);
  await expect(message.getByRole("button", { name: "Load image from images.test", exact: true })).toBeEnabled();
  expect(requests.slice(before).map((request) => request.url)).toEqual(["/page.html"]);

  await message.getByRole("button", { name: "Load image from 127.0.0.1", exact: true }).click();
  await expect(page.locator(".notifications .toast").last()).toHaveText("The image from 127.0.0.1 could not be loaded.");
  // A private address is refused before any request is made.
  expect(requests.slice(before).map((request) => request.url)).toEqual(["/page.html"]);
  await expect(message.locator("img")).toHaveCount(0);
  expect(errors).toEqual([]);
});

test("markup that copies a placeholder loads nothing, alone or beside a real one", async () => {
  // A reply can write raw HTML that looks like a placeholder for one host and points at another.
  const forged =
    `<span class="remote-image" title="forged" data-remote-host="images.test" data-remote-src="http://badges.test:${port}/forged.png?d=SECRET">` +
    `<span class="remote-image-load">Load image from images.test</span></span>`;
  const message = await reply(`${forged} ![Real](http://images.test:${port}/chart.png?ride=1)`);
  const before = requests.length;
  // The reply keeps none of the placeholder's classes or data, so clicking its copy does nothing.
  const copy = message.locator('[title="forged"]');
  await expect(copy).toHaveCount(1);
  expect(await copy.getAttribute("class")).toBeNull();
  expect(await copy.locator("span").getAttribute("class")).toBeNull();
  await expect(message.locator("[data-remote-src], [data-remote-host]")).toHaveCount(1);
  await copy.locator("span").click();
  // Copies put into the page some other way are not the app's either: one to click, and one
  // for the real host that would ride along with the real button's click.
  await message.locator(".markdown").evaluate((root, port) => {
    const copy = (src: string, label: string) => {
      const holder = document.createElement("span");
      holder.className = "remote-image injected";
      holder.dataset.remoteHost = "images.test";
      holder.dataset.remoteSrc = src;
      const button = document.createElement("button");
      button.className = "remote-image-load";
      button.textContent = label;
      holder.append(button);
      root.append(holder);
    };
    copy(`http://badges.test:${port}/injected.png?d=SECRET`, "Injected copy");
    copy(`http://images.test:${port}/riding.png?d=SECRET`, "Riding copy");
  }, port);
  await message.getByRole("button", { name: "Injected copy", exact: true }).click();
  await page.waitForTimeout(500);
  expect(requests.slice(before)).toEqual([]);
  await message.getByRole("button", { name: "Load image from images.test", exact: true }).click();
  await expect(message.locator("img.remote-image-loaded")).toHaveCount(1);
  await page.waitForTimeout(300);
  expect(requests.slice(before).map((request) => request.url)).toEqual(["/chart.png?ride=1"]);
  await expect(message.getByRole("button", { name: "Riding copy", exact: true })).toBeVisible();
  expect(errors).toEqual([]);
});

test("a reply drawn again while its image loads shows the image once it arrives", async () => {
  await client.createThread({ title: "Other chat", provider: "echo", model: "echo-1" });
  const load = (message: ReturnType<Page["locator"]>) =>
    message.getByRole("button", { name: "Load image from images.test", exact: true });
  const first = await reply(`![Slow](http://images.test:${port}/slow.png?n=1)`);
  const before = requests.length;
  await load(first).click();
  // Another reply with the same image is drawn while the fetch runs: its button waits, off.
  const second = await reply(`Same: ![Slow](http://images.test:${port}/slow.png?n=1)`);
  await expect(load(second)).toBeDisabled();
  // Every reply shows the image when it arrives, from one request.
  await expect(first.locator("img.remote-image-loaded")).toHaveCount(1, { timeout: 15000 });
  await expect(second.locator("img.remote-image-loaded")).toHaveCount(1);
  expect(requests.slice(before).map((request) => request.url)).toEqual(["/slow.png?n=1"]);

  // Leaving the chat and coming back while a fetch runs draws the reply anew; it still ends with its image.
  const third = await reply(`![Slow](http://images.test:${port}/slow.png?n=2)`);
  await load(third).click();
  await page.locator(".sessions").getByRole("button", { name: "Other chat", exact: true }).click();
  await page.locator(".sessions").getByRole("button", { name: "Remote images", exact: true }).click();
  const back = page.locator(".message.assistant").last();
  await expect(back.locator("img.remote-image-loaded")).toHaveCount(1, { timeout: 15000 });
  await expect(load(back)).toHaveCount(0);
  expect(errors).toEqual([]);
});

test("the placeholder speaks the interface language and keeps its host readable right to left", async ({}, info) => {
  await page.evaluate(() => window.haarness.preferences({ language: "ar" }));
  try {
    await expect(page.locator("html")).toHaveAttribute("dir", "rtl");
    // A placeholder drawn in English before follows the language too.
    await expect(
      page.locator(".message.assistant").first().getByRole("button", { name: "تحميل الصورة من badges.test", exact: true }),
    ).toBeVisible();
    // A reply drawn in Arabic: its button names the host inside the translated sentence.
    const message = await reply(`![](http://badges.test:${port}/badge.png?rtl=1)`);
    const load = message.getByRole("button", { name: "تحميل الصورة من badges.test", exact: true });
    await expect(load).toBeVisible();
    await expect(message.locator(".remote-image-alt")).toHaveText("صورة");
    await message.screenshot({ path: info.outputPath("remote-image-rtl.png") });
  } finally {
    await page.evaluate(() => window.haarness.preferences({ language: "en" }));
  }
  expect(errors).toEqual([]);
});
