import { test, expect, _electron as electron, type ElectronApplication, type Page } from "@playwright/test";
import { mkdtemp, mkdir, readFile, writeFile, realpath, rm, symlink, utimes } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";

// The composer's + menu offers the screenshots folder's newest images: picked
// ones are numbered and attach in that order, another folder can be chosen,
// and the renderer only ever names images by their place in the listing.
test.describe.configure({ mode: "serial" });

let app: ElectronApplication, page: Page;
let temporary: string, workspace: string, shots: string, other: string, shapes: string;
let binary: string, env: Record<string, string>;
const errors: string[] = [];

/** Answers for the native dialogs: folders to choose, and the buttons a question gets. */
type Calls = { folders: string[]; questions: string[]; answers: number[] };

const dialog = () => page.getByRole("dialog", { name: "Screenshots" });
const calls = () => app.evaluate(() => (globalThis as unknown as { calls: Calls }).calls);
const answer = (response: number) =>
  app.evaluate((_, response) => (globalThis as unknown as { calls: Calls }).calls.answers.push(response), response);
/** A press elsewhere in the window: in the message box, which it leaves as it was. */
const click = () => page.getByRole("textbox", { name: "Message", exact: true }).click();
const tile = (name: string) => dialog().getByRole("button", { name, exact: true });
const chips = () => page.locator(".composer-attachments .attachment-name");

test.beforeAll(async () => {
  test.setTimeout(240000);
  temporary = await realpath(await mkdtemp(join(process.env.HAARNESS_E2E_DIR ?? tmpdir(), "haarness-screenshots-")));
  workspace = join(temporary, "workspace");
  shots = join(temporary, "Screenshots folder");
  other = join(temporary, "Elsewhere");
  shapes = join(temporary, "Shapes");
  await mkdir(join(workspace, ".haarness"), { recursive: true });
  await mkdir(shots);
  await mkdir(other);
  await mkdir(shapes);
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
    // The picker opens this folder instead of the person's own.
    HAARNESS_SCREENSHOTS_TEST_DIR: shots,
  };
  delete env.ELECTRON_RUN_AS_NODE;
  delete env.ELECTRON_RENDERER_URL;
  app = await electron.launch({ args: ["."], env, timeout: 60000 });
  page = await app.firstWindow();
  page.on("pageerror", (error) => errors.push(error.message));
  await page.setViewportSize({ width: 1400, height: 950 });
  // Real images, drawn by Electron, so thumbnails have something to show.
  const image = (red: number, green: number, blue: number, width = 160, height = 100) =>
    app.evaluate(({ nativeImage }, [color, width, height]) => {
      const pixels = Buffer.alloc(width * height * 4);
      for (let index = 0; index < width * height; index++)
        pixels.set([color[2], color[1], color[0], 255], index * 4);
      return nativeImage.createFromBitmap(pixels, { width, height }).toPNG().toString("base64");
    }, [[red, green, blue], width, height] as [number[], number, number]);
  const files: [string, string, number][] = [
    [join(shots, "Screenshot 1.png"), await image(200, 40, 40), 3600],
    [join(shots, "Screenshot 2.png"), await image(40, 160, 60), 600],
    [join(shots, "Screenshot 3.png"), await image(40, 80, 200), 60],
    [join(other, "Elsewhere.png"), await image(220, 180, 40), 120],
    [join(shapes, "Wide.png"), await image(40, 160, 160), 300],
    [join(shapes, "Tall.png"), await image(160, 40, 160, 100, 160), 200],
  ];
  const now = Date.now() / 1000;
  for (const [path, data, age] of files) {
    await writeFile(path, Buffer.from(data, "base64"));
    await utimes(path, now - age, now - age);
  }
  // Neither a hidden image nor another kind of file is offered.
  await writeFile(join(shots, ".hidden.png"), Buffer.from(files[0][1], "base64"));
  await writeFile(join(shots, "notes.txt"), "not an image\n");
  // A few bytes whose header asks for 30000 by 30000 pixels (3.6 GB to decode).
  const huge = Buffer.alloc(33);
  Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13, 0x49, 0x48, 0x44, 0x52]).copy(huge);
  huge.writeUInt32BE(30000, 16);
  huge.writeUInt32BE(30000, 20);
  huge.set([8, 6, 0, 0, 0], 24);
  await writeFile(join(shapes, "Huge.png"), huge);
  // The folder dialog answers from a queue: the project first, then folders for
  // the picker. Questions are recorded and answered from their own queue (Cancel when empty).
  await app.evaluate(({ dialog: native }, folders) => {
    const calls: Calls = { folders, questions: [], answers: [] };
    (globalThis as unknown as { calls: Calls }).calls = calls;
    native.showOpenDialog = async () => {
      const next = calls.folders.shift();
      return next ? { canceled: false, filePaths: [next] } : { canceled: true, filePaths: [] };
    };
    native.showMessageBox = async (...args: unknown[]) => {
      const options = (args.length > 1 ? args[1] : args[0]) as { message: string };
      calls.questions.push(options.message);
      return { response: calls.answers.shift() ?? 1, checkboxChecked: false };
    };
  }, [workspace]);
  await page.getByRole("region", { name: "Project home" }).getByRole("button", { name: "Open project", exact: true }).click();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 90000 });
});

test.afterAll(async () => {
  await app?.close();
  if (workspace)
    try {
      execFileSync(binary, ["--workspace", workspace, "--trust-project", "serve", "--stop"], { env, stdio: "ignore", timeout: 30_000 });
    } catch {}
  if (temporary) await rm(temporary, { recursive: true, force: true }).catch(() => {});
});

test("picked screenshots are numbered and attach in the order they were picked", async ({}, info) => {
  const attach = page.getByRole("button", { name: "Attach", exact: true });
  await attach.click();
  await expect(page.getByRole("menuitem", { name: "Attach files", exact: true })).toBeVisible();
  await page.getByRole("menuitem", { name: "Attach screenshots", exact: true }).click();
  await expect(dialog()).toBeVisible();
  await expect(dialog().locator(".screenshot-folder-name")).toHaveText(shots.replace(/^\/Users\/[^/]+/, "~"));
  // Newest first; the hidden image and the text file are not offered.
  await expect(dialog().locator(".screenshot-name")).toHaveText(["Screenshot 3.png", "Screenshot 2.png", "Screenshot 1.png"]);
  await expect(tile("Screenshot 3.png")).toHaveAttribute("aria-description", "1 minute ago");
  await expect(tile("Screenshot 1.png")).toHaveAttribute("aria-description", "1 hour ago");
  await expect(tile("Screenshot 3.png").locator("img")).toHaveAttribute("src", /^data:image\/png;base64,/);

  const add = dialog().getByRole("button", { name: /^Add/ });
  await expect(add).toHaveText("Add screenshots");
  await expect(add).toBeDisabled();
  await tile("Screenshot 3.png").click();
  await tile("Screenshot 1.png").click();
  await expect(tile("Screenshot 3.png").locator(".screenshot-order")).toHaveText("1");
  await expect(tile("Screenshot 1.png").locator(".screenshot-order")).toHaveText("2");
  // Taking one out closes the gap; picking it again puts it last.
  await tile("Screenshot 3.png").click();
  await expect(tile("Screenshot 3.png")).toHaveAttribute("aria-pressed", "false");
  await expect(tile("Screenshot 1.png").locator(".screenshot-order")).toHaveText("1");
  await tile("Screenshot 3.png").click();
  await expect(tile("Screenshot 3.png").locator(".screenshot-order")).toHaveText("2");
  // The arrow keys move between tiles and Space picks.
  await tile("Screenshot 3.png").focus();
  await page.keyboard.press("ArrowRight");
  await expect(tile("Screenshot 2.png")).toBeFocused();
  await page.keyboard.press("Space");
  await expect(tile("Screenshot 2.png").locator(".screenshot-order")).toHaveText("3");
  await page.keyboard.press("Space");
  await expect(add).toHaveText("Add 2 screenshots");
  await dialog().screenshot({ path: info.outputPath("screenshots-light.png") });
  await page.evaluate(() => window.haarness.preferences({ theme: "dark" }));
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await dialog().screenshot({ path: info.outputPath("screenshots-dark.png") });
  await page.evaluate(() => window.haarness.preferences({ theme: "light" }));
  await add.click();
  await expect(dialog()).toHaveCount(0);
  await expect(chips()).toHaveText(["Screenshot 1.png", "Screenshot 3.png"]);
  await expect(page.getByRole("textbox", { name: "Message", exact: true })).toBeFocused();
  expect(errors).toEqual([]);
});

test("another folder can be picked from once, and Escape adds nothing", async () => {
  await app.evaluate((_, folder) => (globalThis as unknown as { calls: Calls }).calls.folders.push(folder), other);
  await page.getByRole("button", { name: "Attach", exact: true }).click();
  await page.getByRole("menuitem", { name: "Attach screenshots", exact: true }).click();
  await expect(dialog().locator(".screenshot-name")).toHaveText(["Screenshot 3.png", "Screenshot 2.png", "Screenshot 1.png"]);
  await dialog().getByRole("button", { name: "Choose folder…", exact: true }).click();
  await expect(dialog().locator(".screenshot-name")).toHaveText(["Elsewhere.png"]);
  await expect(dialog().locator(".screenshot-folder-name")).toHaveText(other.replace(/^\/Users\/[^/]+/, "~"));
  await tile("Elsewhere.png").click();
  await dialog().getByRole("button", { name: "Add 1 screenshot", exact: true }).click();
  await expect(chips()).toHaveText(["Screenshot 1.png", "Screenshot 3.png", "Elsewhere.png"]);

  // Next time the picker opens the screenshots folder again; Escape closes it.
  await page.getByRole("button", { name: "Attach", exact: true }).click();
  await page.getByRole("menuitem", { name: "Attach screenshots", exact: true }).click();
  await expect(dialog().locator(".screenshot-name").first()).toHaveText("Screenshot 3.png");
  await tile("Screenshot 2.png").click();
  await page.keyboard.press("Escape");
  await expect(dialog()).toHaveCount(0);
  await expect(chips()).toHaveCount(3);
  expect(errors).toEqual([]);
});

test("the folder is listed for a choice in the Attach menu, or when the person agrees, and a pick names only places in that listing", async () => {
  // Choosing Attach screenshots in the Attach menu asks nothing, from the keyboard or with clicks.
  await page.getByRole("button", { name: "Attach", exact: true }).focus();
  await page.keyboard.press("Enter");
  await expect(page.getByRole("menuitem", { name: "Attach files", exact: true })).toBeFocused();
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("Enter");
  await expect(tile("Screenshot 2.png")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(dialog()).toHaveCount(0);
  await page.getByRole("button", { name: "Attach", exact: true }).click();
  await page.getByRole("menuitem", { name: "Attach screenshots", exact: true }).click();
  await expect(tile("Screenshot 2.png")).toBeVisible();
  expect((await calls()).questions).toEqual([]);
  // A page asking on its own gets a question first, and nothing when it is declined.
  expect(await page.evaluate(() => window.haarness.screenshots({}))).toEqual({ canceled: true });
  expect((await calls()).questions).toEqual(["Show your newest screenshots?"]);
  // Agreed to, the renderer gets names, times and thumbnails, never paths.
  await answer(0);
  const listing = await page.evaluate(() => window.haarness.screenshots({}));
  expect((await calls()).questions).toHaveLength(2);
  expect(JSON.stringify(listing)).not.toContain("Screenshots folder/");
  expect(Object.keys((listing as { items: object[] }).items[0]).sort()).toEqual(["modified", "name", "thumbnail"]);
  // The dialog left open over that newer listing is refused rather than attaching other files.
  await tile("Screenshot 2.png").click();
  await dialog().getByRole("button", { name: "Add 1 screenshot", exact: true }).click();
  await expect(dialog().getByRole("alert")).toHaveText("The screenshots changed. Open Screenshots again.");
  await dialog().getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(chips()).toHaveCount(3);
  // A press elsewhere (the Enter that sends a message, say) lists nothing
  // without the question, even with a menu token asked for right after it.
  await click();
  expect(
    await page.evaluate(async () =>
      window.haarness.screenshots({ token: (await window.haarness.screenshotMenu()) ?? undefined }),
    ),
  ).toEqual({ canceled: true });
  expect((await calls()).questions).toHaveLength(3);
  // Nor does a token whose menu saw typing before the next press.
  await click();
  const token = await page.evaluate(() => window.haarness.screenshotMenu());
  expect(token).toBeTruthy();
  await page.keyboard.type("x");
  await click();
  expect(await page.evaluate((token) => window.haarness.screenshots({ token: token! }), token)).toEqual({ canceled: true });
  expect((await calls()).questions).toHaveLength(4);
  await page.keyboard.press("Backspace");
  // Chosen without a press Desktop can see (as VoiceOver and Voice Control
  // press), the dialog asks, and declining closes it.
  await page.evaluate(() => {
    document.querySelector<HTMLButtonElement>('.composer-leading button[aria-label="Attach"]')!.click();
    document.querySelector<HTMLButtonElement>('[role="menuitem"][aria-label="Attach screenshots"]')!.click();
  });
  await expect.poll(async () => (await calls()).questions.length).toBe(5);
  await expect(dialog()).toHaveCount(0);
  await answer(0);
  const result = await page.evaluate(async () => {
    const listing = (await window.haarness.screenshots({})) as { listing: string };
    const outcome = async (run: () => Promise<unknown>) =>
      run().then(
        (value) => ({ value }),
        (error: Error) => ({ error: error.message }),
      );
    return {
      stale: await outcome(() => window.haarness.screenshotAttach("not-a-listing", [0])),
      outside: await outcome(() => window.haarness.screenshotAttach(listing.listing, [7])),
      picked: await outcome(() => window.haarness.screenshotAttach(listing.listing, [2, 0])),
      again: await outcome(() => window.haarness.screenshotAttach(listing.listing, [0])),
    };
  });
  // A listing attaches once.
  expect((await calls()).questions).toHaveLength(6);
  expect(result.again).toEqual({ error: "The screenshots changed. Open Screenshots again." });
  expect(result.stale).toEqual({ error: "The screenshots changed. Open Screenshots again." });
  expect(result.outside).toEqual({ error: "Choose up to 20 screenshots from the list." });
  const picked = (result.picked as { value: { name: string; data: string }[] }).value;
  expect(picked.map((item) => item.name)).toEqual(["Screenshot 1.png", "Screenshot 3.png"]);
  expect(picked[0].data).toMatch(/^data:image\/png;/);

  // A screenshot swapped for a link to another file since it was listed is refused, not followed.
  const path = join(shots, "Screenshot 1.png");
  const original = await readFile(path);
  await answer(0);
  const latest = await page.evaluate(async () => ((await window.haarness.screenshots({})) as { listing: string }).listing);
  await rm(path);
  await symlink(join(other, "Elsewhere.png"), path);
  const swapped = await page.evaluate(
    (listing) => window.haarness.screenshotAttach(listing, [2]).then(() => "attached", (error: Error) => error.message),
    latest,
  );
  expect(swapped).toBe("The screenshots changed. Open Screenshots again.");
  await rm(path);
  await writeFile(path, original);
  await utimes(path, Date.now() / 1000 - 3600, Date.now() / 1000 - 3600);
});

test("thumbnails keep each image's shape, and an image too large to draw safely shows a placeholder", async ({}, info) => {
  await app.evaluate((_, folder) => (globalThis as unknown as { calls: Calls }).calls.folders.push(folder), shapes);
  await page.getByRole("button", { name: "Attach", exact: true }).click();
  await page.getByRole("menuitem", { name: "Attach screenshots", exact: true }).click();
  await dialog().getByRole("button", { name: "Choose folder…", exact: true }).click();
  await expect(dialog().locator(".screenshot-name")).toHaveText(["Huge.png", "Tall.png", "Wide.png"]);
  const natural = (name: string) =>
    tile(name)
      .locator("img")
      .evaluate((img: HTMLImageElement) => (img.complete ? [img.naturalWidth, img.naturalHeight] : []));
  await expect.poll(() => natural("Wide.png")).toEqual([160, 100]);
  await expect.poll(() => natural("Tall.png")).toEqual([100, 160]);
  // Listed and still pickable, with the image icon in place of a thumbnail.
  await expect(tile("Huge.png").locator("img")).toHaveCount(0);
  await expect(tile("Huge.png").locator(".screenshot-thumb svg")).toBeVisible();
  await dialog().screenshot({ path: info.outputPath("screenshots-shapes.png") });
  await dialog().getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(dialog()).toHaveCount(0);
  expect(errors).toEqual([]);
});
