import { test, expect, _electron as electron, type ElectronApplication, type Locator, type Page } from "@playwright/test";
import { Haarness } from "@haarness/sdk";
import { mkdtemp, mkdir, readFile, writeFile, realpath, rm, symlink } from "node:fs/promises";
import { createHash } from "node:crypto";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";
import { crc32, deflateSync } from "node:zlib";

// An image a reply names by a path in its project shows that file, read
// through the server, which reads only inside the project; a path out of it,
// through a link or not, is never shown. A read tool call that returned an
// image previews it in its opened row, and any image of the conversation
// opens full size. The echo provider repeats the message, so a message with
// `![](out/chart.png)` gets a reply with the image.
// HAARNESS_IMAGE_SCREENSHOTS=<folder> keeps light and dark pictures there.
test.describe.configure({ mode: "serial" });

const shots = process.env.HAARNESS_IMAGE_SCREENSHOTS;
let app: ElectronApplication, page: Page, client: Haarness;
let temporary: string, workspace: string, binary: string, env: Record<string, string>;
let thread = "";
let api: { url: string; token: string };
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
  api = { url: connection.url, token: connection.token };
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

/** Writes a project file through the server's file API, as the Files panel does. */
async function writeProjectFile(path: string, bytes: Buffer) {
  const revision = createHash("sha256").update(await readFile(join(workspace, path))).digest("hex");
  const response = await fetch(`${api.url}/v1/files/content`, {
    method: "PUT",
    headers: { authorization: `Bearer ${api.token}`, "content-type": "application/json" },
    body: JSON.stringify({ path, content: bytes.toString("base64"), encoding: "base64", revision }),
  });
  expect(response.status).toBe(200);
}

/**
 * Sends `text`, which the echo provider repeats as the reply, and returns that
 * reply, found by its item: the transcript draws only the rows near the view.
 */
async function reply(text: string, attachments?: string[]) {
  await expect.poll(async () => (await client.activeTurns()).length, { timeout: 30000 }).toBe(0);
  // Back to following the newest output (End), as the person would after looking at an earlier image.
  await page.locator(".transcript").focus();
  await page.keyboard.press("End");
  const turn = await client.startTurn(thread, { requestId: crypto.randomUUID(), text, attachments });
  await client.waitTurn(turn.id);
  const items = await client.listItems(thread);
  const saved = items.findLast((item) => item.turnId === turn.id && item.data.type === "assistant_message")!;
  const message = page.locator(`[data-item-id="${saved.id}"] .message.assistant`);
  await expect(message).toBeInViewport();
  await expect(page.locator(".assistant-activity")).toHaveCount(0);
  return message;
}

/**
 * Opens a tool row and returns its preview. A tool row just after a turn is
 * now and then drawn anew closed, on the base build too ("tool activity shows
 * readable commands" in desktop.spec.ts fails the same way there); it is
 * opened again then, while a preview that never comes still fails.
 */
async function openPreview(row: Locator) {
  const preview = row.locator(".tool-image img");
  await expect(async () => {
    if ((await row.getAttribute("open")) === null) await row.locator("summary").click();
    await expect(preview).toBeVisible({ timeout: 2000 });
  }).toPass({ timeout: 15_000 });
  return preview;
}

/**
 * A picture of `target` where it is now: the window clipped to its box, so a
 * row the transcript draws anew meanwhile does not hold the picture up.
 */
async function shoot(target: Locator | Page, path: string) {
  if (!("boundingBox" in target)) return void (await target.screenshot({ path }));
  const box = (await target.boundingBox())!;
  const view = page.viewportSize()!;
  const top = Math.max(0, box.y);
  await page.screenshot({
    path,
    clip: { x: Math.max(0, box.x), y: top, width: Math.min(box.width, view.width), height: Math.min(box.y + box.height, view.height) - top },
  });
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
    await shoot(target, join(shots, `${name}-${theme}.png`));
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
  // The person's own message shows the same images as thumbnails, which never fold it.
  const own = page.locator(".message.user").last();
  await expect(own.locator("img.project-image-loaded")).toHaveCount(3);
  expect(await own.locator("img.project-image-loaded").evaluateAll((images) =>
    images.map((image) => Math.round(image.getBoundingClientRect().height)),
  )).toEqual([120, 120, 120]);
  await expect(own.locator(".bubble-body")).not.toHaveClass(/folded/);
  await expect(own.getByRole("button", { name: "Show more", exact: true })).toHaveCount(0);
  // Following the conversation, it stays at the newest output once the images are in.
  await expect
    .poll(() => page.locator(".transcript").evaluate((list) => list.scrollHeight - list.scrollTop - list.clientHeight))
    .toBeLessThan(4);
  await own.scrollIntoViewIfNeeded();
  await pictures(own, "own-message-image");
  const shown = await reply("Here is this week's chart:\n\n![Weekly chart](out/chart.png)\n\nThursday was the busiest day.");
  await expect(shown.locator("img.project-image-loaded")).toBeVisible();
  await pictures(shown, "reply-image");
  expect(errors).toEqual([]);
});

test("thumbnails side by side in your own message count as one line toward the fold", async () => {
  // Twelve lines of text and one line of three thumbnails: thirteen lines, which fold.
  const lines = Array.from({ length: 12 }, (_, index) => `Line ${index + 1}`).join("\n");
  await reply(`${lines}\n\n![a](out/chart.png) ![b](out/tall.png) ![c](out/wide.png)`);
  const own = page.locator(".message.user").last();
  await expect(own.locator("img.project-image-loaded")).toHaveCount(3);
  await expect(own.getByRole("button", { name: "Show more", exact: true })).toBeVisible();
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

test("an image that arrives above the view moves nothing being read", async () => {
  await writeFile(join(workspace, "out/late.png"), chart(480, 300, [200, 80, 160]));
  // The test hook in the main process holds this read until it is let go.
  await app.evaluate(() => {
    (globalThis as { haarnessHeldReads?: Set<string> }).haarnessHeldReads = new Set(["out/late.png"]);
  });
  const late = await reply("Before the late chart.\n\n![Late](out/late.png)\n\nRead on from here.");
  const waiting = late.locator(".project-image");
  await expect(waiting).toHaveCount(1);
  const id = await late.evaluate((element) => element.closest("[data-item-id]")!.getAttribute("data-item-id"));
  for (let index = 0; index < 3; index++)
    await reply(Array.from({ length: 8 }, (_, line) => `Paragraph ${index}.${line} keeps the reader busy for a while.`).join("\n\n"));
  // The person scrolls back up and reads just below the waiting image.
  const list = page.locator(".transcript");
  const placed = () =>
    list.evaluate((element, row) => {
      const holder = element.querySelector(`[data-item-id="${row}"] .project-image`);
      return holder ? holder.getBoundingClientRect().top - element.getBoundingClientRect().top : null;
    }, id);
  await list.hover();
  for (let step = 0, at = await placed(); step < 80 && !(at !== null && at > 0 && at < 200); step++, at = await placed()) {
    await page.mouse.wheel(0, at !== null && at >= 200 ? 100 : -150);
    await page.waitForTimeout(40);
  }
  await list.evaluate((element, row) => {
    const holder = element.querySelector(`[data-item-id="${row}"] .project-image`)!;
    element.scrollTop += holder.getBoundingClientRect().bottom - element.getBoundingClientRect().top + 4;
  }, id);
  const text = late.getByText("Read on from here.");
  const where = () =>
    text.evaluate((element) => Math.round(element.getBoundingClientRect().top - element.closest(".transcript")!.getBoundingClientRect().top));
  await page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done))));
  const before = await where();
  expect(before).toBeGreaterThan(0);
  await app.evaluate(() => (globalThis as { haarnessHeldReads?: Set<string> }).haarnessHeldReads?.clear());
  await expect(late.locator("img.project-image-loaded")).toHaveJSProperty("naturalWidth", 480);
  await page.evaluate(() => new Promise((done) => setTimeout(done, 300)));
  expect(await where()).toBe(before);
  expect(errors).toEqual([]);
});

test("two images coming in at once above the view still move nothing being read", async () => {
  await writeFile(join(workspace, "out/first.png"), chart(480, 280, [200, 120, 40]));
  await writeFile(join(workspace, "out/second.png"), chart(480, 200, [40, 120, 200]));
  await app.evaluate(() => {
    (globalThis as { haarnessHeldReads?: Set<string> }).haarnessHeldReads = new Set(["out/first.png", "out/second.png"]);
  });
  const both = await reply("Two late charts.\n\n![First](out/first.png)\n\n![Second](out/second.png)\n\nRead on from here.");
  await expect(both.locator(".project-image")).toHaveCount(2);
  const id = await both.evaluate((element) => element.closest("[data-item-id]")!.getAttribute("data-item-id"));
  for (let index = 0; index < 3; index++)
    await reply(Array.from({ length: 8 }, (_, line) => `Line ${index}.${line} keeps the reader busy for a while.`).join("\n\n"));
  const list = page.locator(".transcript");
  const placed = () =>
    list.evaluate((element, row) => {
      const holders = element.querySelectorAll(`[data-item-id="${row}"] .project-image`);
      return holders.length ? holders[1].getBoundingClientRect().top - element.getBoundingClientRect().top : null;
    }, id);
  await list.hover();
  for (let step = 0, at = await placed(); step < 80 && !(at !== null && at > 0 && at < 200); step++, at = await placed()) {
    await page.mouse.wheel(0, at !== null && at >= 200 ? 100 : -150);
    await page.waitForTimeout(40);
  }
  // Both waiting images just above the view; the text below them at its top.
  await list.evaluate((element, row) => {
    const holders = element.querySelectorAll(`[data-item-id="${row}"] .project-image`);
    element.scrollTop += holders[1].getBoundingClientRect().bottom - element.getBoundingClientRect().top + 4;
  }, id);
  const text = both.getByText("Read on from here.");
  const where = () =>
    text.evaluate((element) => Math.round(element.getBoundingClientRect().top - element.closest(".transcript")!.getBoundingClientRect().top));
  await page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done))));
  const before = await where();
  expect(before).toBeGreaterThan(0);
  await app.evaluate(() => (globalThis as { haarnessHeldReads?: Set<string> }).haarnessHeldReads?.clear());
  await expect(both.locator("img.project-image-loaded")).toHaveCount(2);
  await page.evaluate(() => new Promise((done) => setTimeout(done, 300)));
  expect(await where()).toBe(before);
  expect(errors).toEqual([]);
});

/** Scrolls the transcript with the wheel until `placed` (an offset from the view's top, or null) is within [0, 200). */
async function scrollUntil(placed: () => Promise<number | null>) {
  const list = page.locator(".transcript");
  await list.hover();
  for (let step = 0, at = await placed(); step < 80 && !(at !== null && at > 0 && at < 200); step++, at = await placed()) {
    await page.mouse.wheel(0, at !== null && at >= 200 ? 100 : -150);
    await page.waitForTimeout(40);
  }
}

const frames = (count = 2) =>
  page.evaluate(
    (count) =>
      new Promise((done) => {
        const next = (left: number) => (left ? requestAnimationFrame(() => next(left - 1)) : done(undefined));
        next(count);
      }),
    count,
  );

test("an image in a closed compaction summary holds up no correction for a reply above the view", async () => {
  await writeFile(join(workspace, "out/shared.png"), chart(480, 160, [60, 140, 200]));
  // A chat whose history starts with a compaction summary (a closed disclosure) showing the image.
  const source = await client.createThread({ title: "Compaction source", provider: "echo", model: "echo-1" });
  const archive = await client.exportSessionArchive(source.id);
  const id = crypto.randomUUID();
  const now = new Date().toISOString();
  archive.rootThreadId = id;
  archive.threads[0].thread.id = id;
  archive.threads[0].thread.title = "Compacted with images";
  const message = (text: string) => ({ type: "assistant_message" as const, text });
  const datas = [
    { type: "compaction" as const, summary: "Earlier work made a chart.\n\n![Shared](out/shared.png)", files_touched: [], outstanding: [] },
    message("Before the chart.\n\n![Shared](out/shared.png)\n\nRead on from here."),
    ...Array.from({ length: 6 }, (_, index) => message(`Part ${index}.\n\n` + "Context paragraph. ".repeat(60))),
  ];
  archive.threads[0].items = datas.map((data, index) => ({
    id: crypto.randomUUID(), threadId: id, turnId: null, sequence: index + 1, createdAt: now, data,
  }));
  await client.importSessionArchive({ archive });
  await page.locator(".sessions").getByRole("button", { name: "Compacted with images", exact: true }).click();
  const reply = page.locator(`[data-item-id="${archive.threads[0].items[1].id}"]`);
  const list = page.locator(".transcript");
  await scrollUntil(() =>
    list.evaluate((element, row) => {
      const image = element.querySelector(`[data-item-id="${row}"] img.project-image-loaded`);
      return image ? image.getBoundingClientRect().top - element.getBoundingClientRect().top : null;
    }, archive.threads[0].items[1].id),
  );
  await expect(reply.locator("img.project-image-loaded")).toHaveJSProperty("naturalHeight", 160);
  // The reply's image just above the view; its text below at the top.
  await list.evaluate((element, row) => {
    const image = element.querySelector(`[data-item-id="${row}"] img.project-image-loaded`)!;
    element.scrollTop += image.getBoundingClientRect().bottom - element.getBoundingClientRect().top + 4;
  }, archive.threads[0].items[1].id);
  const text = reply.getByText("Read on from here.");
  const where = () =>
    text.evaluate((element) => Math.round(element.getBoundingClientRect().top - element.closest(".transcript")!.getBoundingClientRect().top));
  await frames();
  const before = await where();
  expect(before).toBeGreaterThan(0);
  // The agent remakes the chart and its turn ends: both copies are read again, the hidden one first.
  await writeFile(join(workspace, "out/shared.png"), chart(480, 320, [200, 140, 60]));
  const turn = await client.startTurn(id, { requestId: crypto.randomUUID(), text: "Done." });
  await client.waitTurn(turn.id);
  await expect(reply.locator("img.project-image-loaded")).toHaveJSProperty("naturalHeight", 320);
  await page.waitForTimeout(300);
  expect(Math.abs((await where()) - before)).toBeLessThanOrEqual(1);
  await page.locator(".sessions").getByRole("button", { name: "Images", exact: true }).click();
  expect(errors).toEqual([]);
});

test("a correction no measure answers is given up after two frames, so the next image is still corrected", async () => {
  await writeFile(join(workspace, "out/one.png"), chart(480, 120, [90, 90, 200]));
  await writeFile(join(workspace, "out/two.png"), chart(480, 140, [200, 90, 90]));
  await app.evaluate(() => {
    (globalThis as { haarnessHeldReads?: Set<string> }).haarnessHeldReads = new Set(["out/one.png", "out/two.png"]);
  });
  const message = await reply("Two charts in turn.\n\n![One](out/one.png)\n\n![Two](out/two.png)\n\nRead on from here.");
  const id = await message.evaluate((element) => element.closest("[data-item-id]")!.getAttribute("data-item-id"));
  for (let index = 0; index < 3; index++)
    await reply(Array.from({ length: 8 }, (_, line) => `Line ${index}.${line} keeps the reader busy for a while.`).join("\n\n"));
  const list = page.locator(".transcript");
  const holder = (index: number) =>
    list.evaluate(
      (element, [row, at]) => {
        const holders = element.querySelectorAll(`[data-item-id="${row}"] .project-image`);
        return holders[at as number] ? holders[at as number].getBoundingClientRect().top - element.getBoundingClientRect().top : null;
      },
      [id, index] as const,
    );
  await scrollUntil(() => holder(1));
  await list.evaluate((element, row) => {
    const holders = element.querySelectorAll(`[data-item-id="${row}"] .project-image`);
    element.scrollTop += holders[1].getBoundingClientRect().bottom - element.getBoundingClientRect().top + 4;
  }, id);
  const text = message.getByText("Read on from here.");
  const where = () =>
    text.evaluate((element) => Math.round(element.getBoundingClientRect().top - element.closest(".transcript")!.getBoundingClientRect().top));
  await frames();
  // A measure that never comes, as for text the window skips: the first image is not corrected.
  await page.evaluate(() => {
    const real = window.ResizeObserver;
    (window as unknown as { realResizeObserver: typeof ResizeObserver }).realResizeObserver = real;
    window.ResizeObserver = class {
      observe() {}
      unobserve() {}
      disconnect() {}
    } as unknown as typeof ResizeObserver;
  });
  await app.evaluate(() => (globalThis as { haarnessHeldReads?: Set<string> }).haarnessHeldReads?.delete("out/one.png"));
  await expect(message.locator("img.project-image-loaded")).toHaveCount(1);
  await page.evaluate(() => {
    window.ResizeObserver = (window as unknown as { realResizeObserver: typeof ResizeObserver }).realResizeObserver;
  });
  await frames(3);
  const after = await where();
  // Two frames on, the waiting correction is given up: the second image is corrected.
  await app.evaluate(() => (globalThis as { haarnessHeldReads?: Set<string> }).haarnessHeldReads?.clear());
  await expect(message.locator("img.project-image-loaded")).toHaveCount(2);
  await page.waitForTimeout(300);
  // Within a pixel: the view's offset is whole pixels, the text's place is not.
  expect(Math.abs((await where()) - after)).toBeLessThanOrEqual(1);
  expect(errors).toEqual([]);
});

test("when a tall code block fills the view, an image coming in above it in two replies moves nothing", async () => {
  await writeFile(join(workspace, "out/both.png"), chart(480, 220, [120, 180, 90]));
  await app.evaluate(() => {
    (globalThis as { haarnessHeldReads?: Set<string> }).haarnessHeldReads = new Set(["out/both.png"]);
  });
  await reply("Here first: ![Both](out/both.png)");
  const code = Array.from({ length: 80 }, (_, line) => `const line${line} = ${line};`).join("\n");
  const message = await reply("And here: ![Both](out/both.png)\n\n```ts\n" + code + "\n```");
  const id = await message.evaluate((element) => element.closest("[data-item-id]")!.getAttribute("data-item-id"));
  const list = page.locator(".transcript");
  const top = () =>
    list.evaluate((element, row) => {
      const block = element.querySelector(`[data-item-id="${row}"] pre`);
      return block ? block.getBoundingClientRect().top - element.getBoundingClientRect().top : null;
    }, id);
  await scrollUntil(top);
  // The code block starts just above the view and fills it: no text starts inside the view.
  await list.evaluate((element, row) => {
    const block = element.querySelector(`[data-item-id="${row}"] pre`)!;
    element.scrollTop += block.getBoundingClientRect().top - element.getBoundingClientRect().top + 30;
  }, id);
  await frames();
  const before = await top();
  expect(before).toBeLessThan(0);
  await app.evaluate(() => (globalThis as { haarnessHeldReads?: Set<string> }).haarnessHeldReads?.clear());
  await expect(message.locator("img.project-image-loaded")).toHaveJSProperty("naturalHeight", 220);
  await page.waitForTimeout(300);
  expect(Math.abs((await top())! - before!)).toBeLessThanOrEqual(1);
  expect(errors).toEqual([]);
});

/**
 * Sends `text` with the read of `path` held, scrolls so that the `index`th
 * image placeholder of its reply is just above the view, lets the read go, and
 * gives back how far "Read on from here." moved once the images came in.
 */
async function movedByHeldImage(markdown: string, path: string, index: number) {
  await app.evaluate((_electron, held) => {
    (globalThis as { haarnessHeldReads?: Set<string> }).haarnessHeldReads = new Set([held]);
  }, path);
  const message = await reply(markdown);
  const id = await message.evaluate((element) => element.closest("[data-item-id]")!.getAttribute("data-item-id"));
  for (let part = 0; part < 3; part++)
    await reply(Array.from({ length: 8 }, (_, line) => `Part ${part}.${line} keeps the reader busy for a while.`).join("\n\n"));
  const list = page.locator(".transcript");
  const holder = () =>
    list.evaluate(
      (element, [row, at]) => {
        const holders = element.querySelectorAll(`[data-item-id="${row}"] .project-image`);
        return holders[at as number] ? holders[at as number].getBoundingClientRect().top - element.getBoundingClientRect().top : null;
      },
      [id, index] as const,
    );
  await scrollUntil(holder);
  await list.evaluate(
    (element, [row, at]) => {
      const holders = element.querySelectorAll(`[data-item-id="${row}"] .project-image`);
      element.scrollTop += holders[at as number].getBoundingClientRect().bottom - element.getBoundingClientRect().top + 4;
    },
    [id, index] as const,
  );
  const text = message.getByText("Read on from here.");
  const where = () =>
    text.evaluate((element) => element.getBoundingClientRect().top - element.closest(".transcript")!.getBoundingClientRect().top);
  await frames();
  const before = await where();
  expect(before).toBeGreaterThan(0);
  await app.evaluate(() => (globalThis as { haarnessHeldReads?: Set<string> }).haarnessHeldReads?.clear());
  await expect(message.locator("img.project-image-loaded").first()).toBeAttached();
  await page.waitForTimeout(300);
  return (await where()) - before;
}

test("the same image hidden in a closed disclosure and shown above the view moves nothing being read", async () => {
  await writeFile(join(workspace, "out/dual.png"), chart(480, 200, [70, 150, 210]));
  const moved = await movedByHeldImage(
    // One HTML block, so the image is inside the disclosure (Markdown draws each block on its own).
    'Dual.\n\n<details><summary>Earlier</summary><img src="out/dual.png" alt="Hidden"></details>\n\n![Shown](out/dual.png)\n\nRead on from here.',
    "out/dual.png",
    1,
  );
  expect(Math.abs(moved)).toBeLessThanOrEqual(1);
  expect(errors).toEqual([]);
});

test("an image in a closed disclosure's summary, which is drawn, moves nothing being read", async () => {
  await writeFile(join(workspace, "out/summary.png"), chart(480, 200, [210, 150, 70]));
  const moved = await movedByHeldImage(
    '<details><summary><img src="out/summary.png" alt="Summary"> The summary</summary>Body.</details>\n\nRead on from here.',
    "out/summary.png",
    0,
  );
  expect(Math.abs(moved)).toBeLessThanOrEqual(1);
  expect(errors).toEqual([]);
});

test("scrolling up through replies that finish drawing above the view moves only by the scroll", async () => {
  // A long chat drawn for the first time: each reply measures, then grows as its text is drawn.
  const long = await client.createThread({ title: "Long scroll", provider: "echo", model: "echo-1" });
  for (let index = 0; index < 24; index++) {
    const text =
      `## Part ${index}\n\n` +
      Array.from({ length: 6 + (index % 5) }, (_, line) => `- Item ${index}.${line} with enough words to wrap across the reading column now and then`).join("\n") +
      "\n\n```ts\n" + Array.from({ length: 4 + (index % 3) }, (_, line) => `const value${line} = ${index * line};`).join("\n") + "\n```";
    const turn = await client.startTurn(long.id, { requestId: crypto.randomUUID(), text });
    await client.waitTurn(turn.id);
  }
  await page.locator(".sessions").getByRole("button", { name: "Long scroll", exact: true }).click();
  const list = page.locator(".transcript");
  await expect(list.locator("[data-item-id]").last()).toBeVisible();
  await list.hover();
  await page.mouse.wheel(0, -40);
  await page.waitForTimeout(200);
  // Scroll up 6px a frame; the row at the top of the view must move by exactly that.
  const jumps = await list.evaluate(async (element) => {
    const frame = () => new Promise((done) => requestAnimationFrame(done));
    const top = () => {
      const view = element.getBoundingClientRect();
      const row = document.elementFromPoint(view.left + view.width / 2, view.top + 5)?.closest<HTMLElement>("[data-item-id]");
      return row ? { id: row.dataset.itemId, top: row.getBoundingClientRect().top } : undefined;
    };
    const found: number[] = [];
    for (let step = 0; step < 150 && element.scrollTop > 0; step++) {
      const before = top();
      element.scrollTop -= 6;
      await frame();
      await frame();
      const after = top();
      // A frame may lose its 6px to a correction made at the same time; it may not jump.
      if (before && after && before.id === after.id && Math.abs(after.top - before.top - 6) > 12) found.push(after.top - before.top);
    }
    return found;
  });
  expect(jumps).toEqual([]);
  await page.locator(".sessions").getByRole("button", { name: "Images", exact: true }).click();
  expect(errors).toEqual([]);
});

test("an earlier reply on screen takes the chart its chat remade once the turn ends, though the last reply only says done", async () => {
  await writeFile(join(workspace, "out/remade.png"), chart(300, 180, [120, 80, 200]));
  const first = await reply("The chart: ![Remade](out/remade.png)");
  const image = first.locator("img.project-image-loaded");
  await expect(image).toHaveJSProperty("naturalWidth", 300);
  // A command remakes it (not through the file API), and the next reply only says it is done.
  await writeFile(join(workspace, "out/remade.png"), chart(420, 180, [200, 120, 80]));
  await reply("Done.");
  await expect.poll(() => image.evaluate((element: HTMLImageElement) => element.naturalWidth)).toBe(420);
  expect(errors).toEqual([]);
});

/** An 8x8 baseline JPEG, made once with macOS sips; the test below rewrites its frame header. */
const SMALL_JPEG =
  "/9j/4AAQSkZJRgABAQAASABIAAD/4QBMRXhpZgAATU0AKgAAAAgAAYdpAAQAAAABAAAAGgAAAAAAA6ABAAMAAAABAAEAAKACAAQAAAABAAAACKADAAQAAAABAAAACAAAAAD/7QA4UGhvdG9zaG9wIDMuMAA4QklNBAQAAAAAAAA4QklNBCUAAAAAABDUHYzZjwCyBOmACZjs+EJ+/8AAEQgACAAIAwEiAAIRAQMRAf/EAB8AAAEFAQEBAQEBAAAAAAAAAAABAgMEBQYHCAkKC//EALUQAAIBAwMCBAMFBQQEAAABfQECAwAEEQUSITFBBhNRYQcicRQygZGhCCNCscEVUtHwJDNicoIJChYXGBkaJSYnKCkqNDU2Nzg5OkNERUZHSElKU1RVVldYWVpjZGVmZ2hpanN0dXZ3eHl6g4SFhoeIiYqSk5SVlpeYmZqio6Slpqeoqaqys7S1tre4ubrCw8TFxsfIycrS09TV1tfY2drh4uPk5ebn6Onq8fLz9PX29/j5+v/EAB8BAAMBAQEBAQEBAQEAAAAAAAABAgMEBQYHCAkKC//EALURAAIBAgQEAwQHBQQEAAECdwABAgMRBAUhMQYSQVEHYXETIjKBCBRCkaGxwQkjM1LwFWJy0QoWJDThJfEXGBkaJicoKSo1Njc4OTpDREVGR0hJSlNUVVZXWFlaY2RlZmdoaWpzdHV2d3h5eoKDhIWGh4iJipKTlJWWl5iZmqKjpKWmp6ipqrKztLW2t7i5usLDxMXGx8jJytLT1NXW19jZ2uLj5OXm5+jp6vLz9PX29/j5+v/bAEMABgYGBgYGCgYGCg4KCgoOEg4ODg4SFxISEhISFxwXFxcXFxccHBwcHBwcHCIiIiIiIicnJycnLCwsLCwsLCwsLP/bAEMBBwcHCwoLEwoKEy4fGh8uLi4uLi4uLi4uLi4uLi4uLi4uLi4uLi4uLi4uLi4uLi4uLi4uLi4uLi4uLi4uLi4uLv/dAAQAAf/aAAwDAQACEQMRAD8AzNEtfu8V1X2X2rF0T+GuqrrxWQYP2j9078gqy+pxP//Z";

test("an image whose header hides its size is measured by the window before decoding, and too large says so", async () => {
  // Over a thousand comment segments before its frame, which the cheap header check gives up on;
  // the frame says 8000x8000 pixels, 64 megapixels.
  const bytes = Buffer.from(SMALL_JPEG, "base64");
  const frame = bytes.indexOf(Buffer.from([0xff, 0xc0]));
  bytes.writeUInt16BE(8000, frame + 5);
  bytes.writeUInt16BE(8000, frame + 7);
  const comments = Buffer.concat(Array.from({ length: 1100 }, () => Buffer.from([0xff, 0xfe, 0x00, 0x02])));
  await writeFile(join(workspace, "out/huge.jpg"), Buffer.concat([bytes.subarray(0, 2), comments, bytes.subarray(2)]));
  const message = await reply("![Huge](out/huge.jpg)");
  await expect(message.locator(".project-image .project-image-note")).toHaveText("Too large to show");
  await expect(message.locator("img")).toHaveCount(0);
  expect(errors).toEqual([]);
});

test("a reply drawn again shows its image at once, and a file written anew shows anew", async () => {
  await reply("The chart again: ![Weekly chart](out/chart.png)");
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

  // A new chart written through the file API: the next reply naming it shows the new one at once.
  await writeProjectFile("out/chart.png", chart(360, 200, [230, 120, 40]));
  const message = await reply("Updated: ![Weekly chart](out/chart.png)");
  await expect
    .poll(() => message.locator("img.project-image-loaded").evaluate((image: HTMLImageElement) => image.naturalWidth))
    .toBe(360);
  expect(errors).toEqual([]);
});

/** Runs a read through the echo provider and returns its row, once the turn's reply is drawn. */
async function read(path: string) {
  const answer = await reply(`!tool read {"path":"${path}"}`);
  const items = await client.listItems(thread);
  const call = items.findLast((item) => item.data.type === "tool_call")!;
  const result = items.findLast((item) => item.data.type === "tool_result")!.data as { metadata?: { attachments?: string[] } };
  await expect(answer).toBeVisible();
  return { row: page.locator(`[data-item-id="${call.id}"] details.tool-call`), attachment: result.metadata?.attachments?.[0] };
}

test("a read that returned an image shows it once its row is open", async () => {
  // A small image comes back as the file it read, which the row reads from the project.
  const { row, attachment } = await read("out/chart.png");
  expect(attachment).toBe(join(workspace, "out/chart.png"));
  await expect(row).toBeVisible();
  // Closed, the row is as it was.
  await expect(row.locator(".tool-image")).toHaveCount(0);
  const preview = await openPreview(row);
  await expect(preview).toHaveAttribute("src", /^data:image\/png;base64,/);
  expect((await size(preview)).natural).toEqual([360, 200]);
  // At its own size it is plain; it opens only when shown smaller (below).
  await expect(preview).not.toHaveAttribute("data-image-open");
  await pictures(row, "read-image");

  // A large one comes back smaller, carried in the result: shown as it is, not read.
  const wide = await read("out/wide.png");
  expect(wide.attachment).toMatch(/^data:image\/png;name=wide\.png;base64,/);
  const shown = await openPreview(wide.row);
  await expect(shown).toHaveAttribute("src", /^data:image\/png;base64,/);
  const measured = await size(shown);
  expect(measured.natural).toEqual([2000, 267]);
  expect(measured.shown[1]).toBeLessThanOrEqual(320);
  // Closing the row takes the image away again.
  await wide.row.locator("summary").click();
  await expect(wide.row.locator(".tool-image")).toHaveCount(0);

  // A file changed since the read is not passed off as what the agent read.
  await writeFile(join(workspace, "out/fresh.png"), chart(200, 100, [90, 90, 200]));
  const fresh = await read("out/fresh.png");
  await writeProjectFile("out/fresh.png", chart(300, 150, [200, 90, 90]));
  await fresh.row.locator("summary").click();
  await expect(fresh.row.locator(".tool-console")).toBeVisible();
  await page.waitForTimeout(400);
  await expect(fresh.row.locator(".tool-image")).toHaveCount(0);

  // A picture remembered from before the read, of another size, is read again and shows what the read saw.
  await writeFile(join(workspace, "out/before.png"), chart(240, 120, [90, 160, 90]));
  await expect((await reply("![Before](out/before.png)")).locator("img.project-image-loaded")).toHaveJSProperty("naturalWidth", 240);
  await writeFile(join(workspace, "out/before.png"), chart(320, 160, [160, 90, 90]));
  const later = await read("out/before.png");
  const shownNow = await openPreview(later.row);
  await expect(shownNow).toHaveJSProperty("naturalWidth", 320);
  expect(errors).toEqual([]);
});

/** The room the viewer leaves for its image: the window inside the dialog's padding. */
const room = (viewer: Locator) =>
  viewer.evaluate((dialog) => {
    const style = getComputedStyle(dialog);
    return [
      Math.floor(dialog.clientWidth - parseFloat(style.paddingLeft) - parseFloat(style.paddingRight)),
      Math.floor(dialog.clientHeight - parseFloat(style.paddingTop) - parseFloat(style.paddingBottom)),
    ];
  });

/** The size the viewer shows an image at: its own, or smaller to fit `space` with its ratio kept, never larger. */
const fitted = (natural: number[], space: number[]) => {
  const scale = Math.min(1, space[0] / natural[0], space[1] / natural[1]);
  return [Math.floor(natural[0] * scale), Math.floor(natural[1] * scale)];
};

test("an image shown smaller than it is opens at its own size, or fitted to the window; Esc, a click outside or Close shut it", async () => {
  const attachment = (width: number, height: number) =>
    `data:image/png;base64,${png(width, height, (x, y) => [(x * 3) % 256, 120, (y * 2) % 256]).toString("base64")}`;
  const [photo, icon] = [attachment(600, 400), attachment(64, 48)];
  const message = await reply("With attachments: ![Wide](out/wide.png)\n\nAt its own size: ![Weekly chart](out/chart.png)", [
    photo,
    icon,
  ]);
  const viewer = page.locator("dialog.image-viewer");
  const wide = message.locator("img.project-image-loaded[data-project-path='out/wide.png']");
  const own = message.locator("img.project-image-loaded[data-project-path='out/chart.png']");
  // Shown smaller than it is, an image is a button that opens it; at its own size it is plain.
  await expect(wide).toHaveAttribute("data-image-open", "");
  await expect(wide).toHaveAttribute("role", "button");
  await expect(own).toHaveJSProperty("naturalWidth", 360);
  await expect(own).not.toHaveAttribute("data-image-open");
  await expect(own).not.toHaveAttribute("role");
  expect(await own.evaluate((image) => getComputedStyle(image).cursor)).toBe("auto");
  expect(await wide.evaluate((image) => getComputedStyle(image).cursor)).toBe("zoom-in");
  // So are attachments: a photo shown as a thumbnail opens, a small icon shown whole does not.
  const attached = page.locator(".message.user").last().locator(".attachment-image img");
  await expect(attached.first()).toHaveAttribute("data-image-open", "");
  await expect(attached.last()).toHaveJSProperty("naturalWidth", 64);
  await expect(attached.last()).not.toHaveAttribute("data-image-open");
  await attached.last().click();
  await expect(viewer).toHaveCount(0);

  await wide.click();
  await expect(viewer).toBeVisible();
  const full = viewer.locator("img.image-viewer-image");
  await expect(full).toHaveAttribute("src", (await wide.getAttribute("src"))!);
  const space = await room(viewer);
  // Larger than the window, it is scaled down to fit it.
  await expect.poll(async () => (await size(full)).shown).toEqual(fitted([2400, 320], space));
  expect((await size(full)).shown[0]).toBe(space[0]);
  await pictures(page, "image-viewer");

  // The arrow keys step through the conversation's images: before it, the photo its message attached.
  await page.keyboard.press("ArrowLeft");
  await expect(full).toHaveAttribute("src", photo);
  // Smaller than the window, it shows at its own size, never larger.
  await expect.poll(async () => (await size(full)).shown).toEqual([600, 400]);
  await pictures(page, "image-viewer-own-size");
  await page.keyboard.press("ArrowRight");
  await expect(full).toHaveAttribute("src", (await wide.getAttribute("src"))!);
  // Esc closes it, and the keyboard is back on the image that opened it.
  await page.keyboard.press("Escape");
  await expect(viewer).toHaveCount(0);
  await expect(wide).toBeFocused();
  // From the keyboard: Enter opens it, a click outside the image closes it.
  await page.keyboard.press("Enter");
  await expect(viewer).toBeVisible();
  await page.mouse.click(20, 20);
  await expect(viewer).toHaveCount(0);
  // An image at its own size does not open.
  await own.click();
  await expect(viewer).toHaveCount(0);

  // An attachment and a read's preview shown smaller open the same way; Close shuts it.
  await attached.first().click();
  await expect(full).toHaveAttribute("src", photo);
  await viewer.getByRole("button", { name: "Close", exact: true }).click();
  await expect(viewer).toHaveCount(0);
  await expect(attached.first()).toBeFocused();
  const preview = await openPreview((await read("out/wide.png")).row);
  await expect(preview).toHaveAttribute("data-image-open", "");
  await preview.click();
  await expect(full).toHaveAttribute("src", /^data:image\/png;base64,/);
  await page.keyboard.press("Escape");
  await expect(viewer).toHaveCount(0);
  expect(errors).toEqual([]);
});

test("the model's thinking and a side chat's answer show the project's images the same way", async () => {
  await page.evaluate(() => window.haarness.preferences({ thinking: "show" }));
  try {
    await reply("::reason ![Tall](out/tall.png)");
    const thought = page.locator(".message.reasoning").last().locator("img.project-image-loaded");
    await expect(thought).toBeVisible();
    expect((await size(thought)).shown).toEqual([32, 320]);
  } finally {
    await page.evaluate(() => window.haarness.preferences({ thinking: "compact" }));
  }

  await page.getByRole("textbox", { name: "Message", exact: true }).fill("/btw ![Tall](out/tall.png)");
  await page.getByRole("button", { name: "Send message", exact: true }).click();
  const answer = page.locator(".side-chat-answer img.project-image-loaded");
  await expect(answer).toBeVisible();
  expect((await size(answer)).shown).toEqual([32, 320]);
  // It opens full size there too.
  await answer.click();
  await expect(page.locator("dialog.image-viewer img")).toHaveAttribute("src", (await answer.getAttribute("src"))!);
  await page.keyboard.press("Escape");
  await page.getByRole("button", { name: "Hide side chat", exact: true }).click();
  expect(errors).toEqual([]);
});

test("a local file: URL names a project file through the real sanitizer; one on another host does not", async () => {
  const message = await reply(
    `![Local](file://${workspace}/out/tall.png) ![Localhost](file://localhost${workspace}/out/chart.png) ` +
      "![Share](file://fileserver/share/chart.png) ![System](file:///etc/chart.png)",
  );
  await expect(message.locator("img.project-image-loaded")).toHaveCount(2);
  await expect(message.locator("img.project-image-loaded").first()).toHaveAttribute("data-project-path", "out/tall.png");
  await expect(message.locator("img.project-image-loaded").last()).toHaveAttribute("data-project-path", "out/chart.png");
  await expect(message.locator(".project-image .project-image-note")).toHaveText(["Outside this project", "Outside this project"]);
  // No address the window would load by itself is left in the page.
  await expect(message.locator('img:not([src^="data:"])')).toHaveCount(0);
  expect(errors).toEqual([]);
});

test("placeholders speak the interface language, and the viewer's arrows follow reading order right to left", async () => {
  await page.evaluate(() => window.haarness.preferences({ language: "ar" }));
  try {
    await expect(page.locator("html")).toHaveAttribute("dir", "rtl");
    const message = await reply("![](../outside/secret.png) ![x](out/missing.png)");
    await expect(message.locator(".project-image-alt").first()).toHaveText("صورة");
    await expect(message.locator(".project-image-note")).toHaveText(["خارج هذا المشروع", "غير متاح"]);
    if (shots) await shoot(message, join(shots, "reply-image-notes-rtl.png"));

    const both = await reply("![A](out/wide.png)\n\n![B](out/tall.png)");
    const [first, second] = [both.locator("img.project-image-loaded").first(), both.locator("img.project-image-loaded").last()];
    await expect(second).toHaveAttribute("data-image-open", "");
    await first.click();
    const full = page.locator("dialog.image-viewer img.image-viewer-image");
    await expect(full).toHaveAttribute("alt", "A");
    // In Arabic the next image in reading order is to the left.
    await page.keyboard.press("ArrowLeft");
    await expect(full).toHaveAttribute("alt", "B");
    await page.keyboard.press("ArrowRight");
    await expect(full).toHaveAttribute("alt", "A");
    await page.keyboard.press("Escape");
    await expect(page.locator("dialog.image-viewer")).toHaveCount(0);
  } finally {
    await page.evaluate(() => window.haarness.preferences({ language: "en" }));
  }
  expect(errors).toEqual([]);
});
