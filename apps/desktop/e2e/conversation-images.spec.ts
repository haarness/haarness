import { test, expect, _electron as electron, type ElectronApplication, type Locator, type Page } from "@playwright/test";
import { Haarness } from "@haarness/sdk";
import { mkdtemp, mkdir, writeFile, realpath, rm, symlink } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";
import { crc32, deflateSync } from "node:zlib";

// An image a reply names by a path in its project shows that file, read
// through the server, which reads only inside the project; a path out of it,
// through a link or not, is never shown. The echo provider repeats the
// message, so a message with `![](out/chart.png)` gets a reply with the image.
// HAARNESS_IMAGE_SCREENSHOTS=<folder> keeps light and dark pictures there.
test.describe.configure({ mode: "serial" });

const shots = process.env.HAARNESS_IMAGE_SCREENSHOTS;
let app: ElectronApplication, page: Page, client: Haarness;
let temporary: string, workspace: string, binary: string, env: Record<string, string>;
let thread = "";
const errors: string[] = [];

/** A PNG of `width`×`height` whose pixels `paint` gives, without an image library. */
function png(width: number, height: number, paint: (x: number, y: number) => [number, number, number]) {
  const rows = Buffer.alloc((1 + width * 3) * height);
  for (let y = 0; y < height; y++)
    for (let x = 0; x < width; x++) rows.set(paint(x, y), y * (1 + width * 3) + 1 + x * 3);
  const chunk = (type: string, data: Buffer) => {
    const body = Buffer.concat([Buffer.from(type, "ascii"), data]);
    const size = Buffer.alloc(4);
    size.writeUInt32BE(data.length);
    const check = Buffer.alloc(4);
    check.writeUInt32BE(crc32(body));
    return Buffer.concat([size, body, check]);
  };
  const header = Buffer.alloc(13);
  header.writeUInt32BE(width, 0);
  header.writeUInt32BE(height, 4);
  header.set([8, 2, 0, 0, 0], 8);
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", header),
    chunk("IDAT", deflateSync(rows)),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

/** A small bar chart, so the pictures look like what an agent would show. */
function chart(width: number, height: number, hue: [number, number, number]) {
  const bars = [0.45, 0.7, 0.55, 0.9, 0.62];
  return png(width, height, (x, y) => {
    const slot = Math.floor((x / width) * bars.length);
    const inside = x % Math.floor(width / bars.length) > width / bars.length / 5;
    const top = height * (1 - bars[slot] * 0.86);
    if (inside && y > top && y < height - 8) return hue;
    if (y === height - 8) return [170, 170, 176];
    return [248, 248, 250];
  });
}

test.beforeAll(async () => {
  test.setTimeout(240000);
  temporary = await realpath(await mkdtemp(join(process.env.HAARNESS_E2E_DIR ?? tmpdir(), "haarness-conversation-images-")));
  workspace = join(temporary, "workspace");
  const outside = join(temporary, "outside");
  await mkdir(join(workspace, ".haarness"), { recursive: true });
  await mkdir(join(workspace, "out"), { recursive: true });
  await mkdir(outside, { recursive: true });
  await writeFile(join(workspace, ".haarness/config.toml"), '[defaults]\nprovider="echo"\nmodel="echo-1"\n');
  await writeFile(join(workspace, "out/chart.png"), chart(480, 300, [76, 110, 245]));
  await writeFile(join(workspace, "out/tall.png"), png(100, 1000, (_, y) => [40, 160 - Math.floor(y / 10), 120]));
  await writeFile(join(workspace, "out/wide.png"), chart(2400, 320, [16, 163, 127]));
  await writeFile(join(workspace, "diagram.svg"), '<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"/>');
  // A picture of a known odd size that must never reach the window.
  await writeFile(join(outside, "secret.png"), png(13, 7, () => [255, 0, 0]));
  await symlink(join(outside, "secret.png"), join(workspace, "escape.png"));
  await symlink("../outside", join(workspace, "shared"));
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
  await page.setViewportSize({ width: 1280, height: 900 });
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
  thread = (await client.createThread({ title: "Images", provider: "echo", model: "echo-1" })).id;
  await page.locator(".sessions").getByRole("button", { name: "Images", exact: true }).click();
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

/** Sends `text`, which the echo provider repeats as the reply, and returns that reply. */
async function reply(text: string, attachments?: string[]) {
  await expect.poll(async () => (await client.activeTurns()).length, { timeout: 30000 }).toBe(0);
  const before = await page.locator(".message.assistant").count();
  const turn = await client.startTurn(thread, { requestId: crypto.randomUUID(), text, attachments });
  await client.waitTurn(turn.id);
  await expect(page.locator(".message.assistant")).toHaveCount(before + 1);
  return page.locator(".message.assistant").last();
}

/** Light and dark pictures of `target`, kept when HAARNESS_IMAGE_SCREENSHOTS names a folder. */
async function pictures(target: Locator | Page, name: string) {
  if (!shots) return;
  await mkdir(shots, { recursive: true });
  for (const theme of ["light", "dark"] as const) {
    await page.evaluate((value) => window.haarness.preferences({ theme: value }), theme);
    await expect(page.locator("html")).toHaveAttribute("data-theme", theme);
    await page.evaluate(async () => {
      await Promise.all(document.getAnimations().map((animation) => animation.finished.catch(() => {})));
      await new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done)));
    });
    await target.screenshot({ path: join(shots, `${name}-${theme}.png`) });
  }
  await page.evaluate(() => window.haarness.preferences({ theme: "light" }));
}

const size = (image: Locator) =>
  image.evaluate((element: HTMLImageElement) => {
    const box = element.getBoundingClientRect();
    return { natural: [element.naturalWidth, element.naturalHeight], shown: [Math.round(box.width), Math.round(box.height)] };
  });

test("a reply's image from the project shows, fitted no taller than 320px and never stretched", async () => {
  const message = await reply(
    `Here is the chart: ![Weekly chart](out/chart.png)\n\nThe same, absolute: ![Absolute](${workspace}/out/chart.png)\n\n` +
      `A tall one: ![Tall](./out/tall.png)`,
  );
  const images = message.locator("img.project-image-loaded");
  await expect(images).toHaveCount(3);
  for (const image of await images.all()) await expect(image).toHaveAttribute("src", /^data:image\/png;base64,/);
  await expect(images.first()).toHaveAttribute("alt", "Weekly chart");
  expect(await size(images.first())).toEqual({ natural: [480, 300], shown: [480, 300] });
  // Taller than the box: 320px high, its width following, so the ratio holds.
  expect(await size(images.nth(2))).toEqual({ natural: [100, 1000], shown: [32, 320] });
  // Nothing in the conversation names a path for the window to load by itself.
  await expect(page.locator('.transcript .markdown img:not([src^="data:"])')).toHaveCount(0);
  // The person's own message shows the same images.
  await expect(page.locator(".message.user").last().locator("img.project-image-loaded")).toHaveCount(3);
  // Following the conversation, it stays at the newest output once the images are in.
  await expect
    .poll(() => page.locator(".transcript").evaluate((list) => list.scrollHeight - list.scrollTop - list.clientHeight))
    .toBeLessThan(4);
  const shown = await reply("Here is this week's chart:\n\n![Weekly chart](out/chart.png)\n\nThursday was the busiest day.");
  await expect(shown.locator("img.project-image-loaded")).toBeVisible();
  await pictures(shown, "reply-image");
  expect(errors).toEqual([]);
});

test("a path out of the project, through a link or not, is never read and says why", async () => {
  const message = await reply(
    "![Secret](../outside/secret.png) ![Linked](escape.png) ![Folder link](shared/secret.png) " +
      "![System](/etc/secret.png) ![Diagram](diagram.svg) ![Gone](out/missing.png)",
  );
  const notes = message.locator(".project-image");
  await expect(notes).toHaveCount(6);
  await expect(notes.locator(".project-image-note")).toHaveText([
    "Outside this project",
    "Not available",
    "Not available",
    "Outside this project",
    "Not available",
    "Not available",
  ]);
  await expect(notes.locator(".project-image-alt")).toHaveText(["Secret", "Linked", "Folder link", "System", "Diagram", "Gone"]);
  await expect(notes.first()).toHaveAttribute("title", "../outside/secret.png");
  // The picture outside the project never reached the window.
  expect(
    await page.evaluate(() =>
      Array.from(document.images).some((image) => image.naturalWidth === 13 && image.naturalHeight === 7),
    ),
  ).toBe(false);
  await expect(message.locator("img")).toHaveCount(0);
  await pictures(message, "reply-image-notes");
  expect(errors).toEqual([]);
});

test("a reply drawn again shows its image at once, and a changed file shows anew after the turn", async () => {
  await client.createThread({ title: "Elsewhere", provider: "echo", model: "echo-1" });
  // Leaving the chat and coming back draws its replies anew: no placeholder appears in between.
  await page.evaluate(() => {
    const seen: string[] = [];
    (window as unknown as { placeholders: string[] }).placeholders = seen;
    new MutationObserver((records) => {
      for (const record of records)
        for (const node of record.addedNodes) {
          if (!(node instanceof HTMLElement)) continue;
          const holder = ".project-image[data-project-path='out/chart.png']";
          if (node.matches(holder) || node.querySelector(holder)) seen.push(node.outerHTML.slice(0, 200));
        }
    }).observe(document.body, { childList: true, subtree: true });
  });
  await page.locator(".sessions").getByRole("button", { name: "Elsewhere", exact: true }).click();
  await page.locator(".sessions").getByRole("button", { name: "Images", exact: true }).click();
  await expect(page.locator(".message.assistant img.project-image-loaded[data-project-path='out/chart.png']").first()).toBeVisible();
  expect(await page.evaluate(() => (window as unknown as { placeholders: string[] }).placeholders)).toEqual([]);

  // The agent made a new chart: the next reply naming it shows the new one.
  await writeFile(join(workspace, "out/chart.png"), chart(360, 200, [230, 120, 40]));
  const message = await reply("Updated: ![Weekly chart](out/chart.png)");
  await expect
    .poll(() => message.locator("img.project-image-loaded").evaluate((image: HTMLImageElement) => image.naturalWidth))
    .toBe(360);
  expect(errors).toEqual([]);
});

test("placeholders speak the interface language", async () => {
  await page.evaluate(() => window.haarness.preferences({ language: "ar" }));
  try {
    await expect(page.locator("html")).toHaveAttribute("dir", "rtl");
    const message = await reply("![](../outside/secret.png) ![x](out/missing.png)");
    await expect(message.locator(".project-image-alt").first()).toHaveText("صورة");
    await expect(message.locator(".project-image-note")).toHaveText(["خارج هذا المشروع", "غير متاح"]);
    if (shots) await message.screenshot({ path: join(shots, "reply-image-notes-rtl.png") });
  } finally {
    await page.evaluate(() => window.haarness.preferences({ language: "en" }));
  }
  expect(errors).toEqual([]);
});
