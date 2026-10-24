import { test, expect, _electron as electron, type ElectronApplication, type Page } from "@playwright/test";
import { mkdtemp, mkdir, writeFile, realpath } from "node:fs/promises";
import { execFileSync } from "node:child_process";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { stopChatHome } from "./fixtures/chat-home";
import { isolatedEnv } from "./fixtures/isolated-env";

// A card leaving the conversation, and the turn it held going on to its end,
// move the transcript once for each thing that comes or goes, frame by frame:
// following, the view is at the end in every frame drawn, no row is drawn at
// one height and then another, and nothing moves in a frame where nothing
// came or went. The running step it let through, opened, stays where it was
// clicked while a row above the view changes size.
let app: ElectronApplication, page: Page, root: string, project: string, env: Record<string, string>;
const binary = resolve(process.env.HAARNESS_BIN ?? "../../target/debug/haarness");
const shots = process.env.HAARNESS_SCREENSHOTS;
const errors: string[] = [];
test.describe.configure({ mode: "serial" });

/** The transcript as one frame was drawn. */
type Frame = {
  /** Milliseconds since watching began. */
  at: number;
  /** What lies below the view. */
  below: number;
  /** The turn's prompt, from the top of the view. */
  prompt: number;
  /** The cards above the message box. */
  dock: number;
  /** The activity line, 0 when it is gone. */
  activity: number;
  /** The reply being written, 0 when there is none. */
  live: number;
  /** Each drawn row's height, by item. */
  rows: Record<string, number>;
};

async function api(method: string, ...args: unknown[]): Promise<any> {
  return page.evaluate(({ connection, method, args }) => window.haarness.api(connection, method, args), {
    connection: "local:" + project,
    method,
    args,
  });
}
async function idle() {
  await expect.poll(async () => (await api("activeTurns")).length, { timeout: 30000 }).toBe(0);
}
const box = () => page.locator("[data-composer-input]");
async function send(text: string) {
  await box().fill(text);
  await box().press("Enter");
}
const permission = () => page.locator(".prompt-dock.approval");
const question = () => page.getByRole("region", { name: "A question for you" });
const settle = () =>
  page.evaluate(() => new Promise((done) => setTimeout(() => requestAnimationFrame(() => requestAnimationFrame(done)), 300)));

/**
 * The frames drawn while `act` runs and its turn ends. Each is read once the
 * frame is laid out and the app's own observers have run for it, just before
 * it is painted: a ResizeObserver on a probe resized every frame, nested
 * deeper than anything the app observes so it changes nothing of when theirs
 * are told.
 */
async function frames(act: () => Promise<void>): Promise<Frame[]> {
  await page.evaluate(() => {
    const list = document.querySelector<HTMLElement>(".transcript")!;
    const holder = document.createElement("div");
    holder.style.cssText = "position:fixed;inset:0 auto auto 0;width:0;height:0;opacity:0;pointer-events:none";
    let inner = holder;
    for (let level = 0; level < 80; level++) inner = inner.appendChild(document.createElement("div"));
    const probe = inner.appendChild(document.createElement("div"));
    probe.style.cssText = "width:1px;height:1px";
    document.body.append(holder);
    const start = performance.now();
    const seen: Frame[] = [];
    Object.assign(window, { seen: 0 });
    const observer = new ResizeObserver(() => {
      Object.assign(window, { seen: seen.length + 1 });
      const view = list.getBoundingClientRect();
      const prompts = list.querySelectorAll<HTMLElement>(".transcript-body > [data-index] .message.user");
      const rows: Record<string, number> = {};
      for (const row of list.querySelectorAll<HTMLElement>(".transcript-body > [data-item-id]"))
        rows[row.dataset.itemId!] = row.offsetHeight;
      seen.push({
        at: Math.round(performance.now() - start),
        below: list.scrollHeight - list.scrollTop - list.clientHeight,
        prompt: prompts[prompts.length - 1].getBoundingClientRect().top - view.top,
        dock: document.querySelector(".approvals")?.getBoundingClientRect().height ?? 0,
        activity: list.querySelector<HTMLElement>(".assistant-activity")?.offsetHeight ?? 0,
        live: list.querySelector<HTMLElement>('[data-testid="streaming-reply"]')?.offsetHeight ?? 0,
        rows,
      });
    });
    observer.observe(probe);
    let wide = false;
    let frame = requestAnimationFrame(function resize() {
      wide = !wide;
      probe.style.width = wide ? "2px" : "1px";
      frame = requestAnimationFrame(resize);
    });
    Object.assign(window, {
      watched: () => {
        cancelAnimationFrame(frame);
        observer.disconnect();
        holder.remove();
        return seen;
      },
    });
  });
  // A frame drawn before acting: the card is in the first one watched.
  await expect.poll(() => page.evaluate(() => (window as unknown as { seen: number }).seen)).toBeGreaterThan(1);
  await act();
  await idle();
  await settle();
  return page.evaluate(() => (window as unknown as { watched: () => Frame[] }).watched());
}

/** Following throughout, from the frame the card left: see the top of the file. */
function steady(seen: Frame[], growing: string[] = []) {
  const gone = seen.findIndex((frame) => frame.dock === 0);
  expect(gone).toBeGreaterThan(0);
  const after = seen.slice(gone);
  // At the end in every frame: never short of it, or past it and pulled back.
  expect(after.filter((frame) => frame.below > 1).map((frame) => `${frame.at}ms: ${frame.below}px below`)).toEqual([]);
  // Drawn at one height, a row keeps it: none comes in empty and grows,
  // none is drawn anew and shrinks for a frame.
  const first = new Map<string, number>();
  const redrawn: string[] = [];
  for (const frame of seen)
    for (const [id, height] of Object.entries(frame.rows)) {
      if (!first.has(id)) first.set(id, height);
      else if (first.get(id) !== height && !growing.includes(id)) redrawn.push(`${frame.at}ms: ${id} ${first.get(id)} → ${height}`);
    }
  expect(redrawn).toEqual([]);
  // The view moves only with something that came or went: the card, a row,
  // the reply being written, the activity line, or the output of a step opened.
  const unexplained: string[] = [];
  let moves = 0;
  for (let index = 1; index < seen.length; index++) {
    const [was, now] = [seen[index - 1], seen[index]];
    if (Math.abs(now.prompt - was.prompt) < 1) continue;
    moves++;
    const changed =
      Math.abs(now.dock - was.dock) >= 1 ||
      now.activity !== was.activity ||
      now.live !== was.live ||
      Object.keys({ ...was.rows, ...now.rows }).some((id) => was.rows[id] !== now.rows[id]);
    if (!changed) unexplained.push(`${now.at}ms: moved ${Math.round(now.prompt - was.prompt)}px`);
  }
  expect(unexplained).toEqual([]);
  // The card's leaving is one of them, in the frame it left.
  expect(Math.abs(seen[gone].prompt - seen[gone - 1].prompt)).toBeGreaterThanOrEqual(1);
  return moves;
}

/** The drawn text of the person's messages in view, noted to tell later
 * whether it was drawn anew. */
async function notePrompts() {
  await page.evaluate(() => {
    const view = document.querySelector(".transcript")!.getBoundingClientRect();
    const noted = [...document.querySelectorAll(".transcript .message.user .markdown")].filter((text) => {
      const box = text.getBoundingClientRect();
      return box.bottom > view.top && box.top < view.bottom;
    });
    Object.assign(window, { noted });
  });
}
/** How many of them are still the text drawn then. */
const keptPrompts = () =>
  page.evaluate(() => {
    const noted = (window as unknown as { noted: Element[] }).noted;
    return { noted: noted.length, kept: noted.filter((text) => text.isConnected).length };
  });

/** The window, light and dark, kept when HAARNESS_SCREENSHOTS names a folder. */
async function pictures(name: string) {
  for (const theme of ["light", "dark"] as const) {
    await page.evaluate((theme) => window.haarness.preferences({ theme }), theme);
    await expect(page.locator("html")).toHaveAttribute("data-theme", theme);
    await page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done))));
    await page.screenshot({ path: shots ? join(shots, `${name}-${theme}.png`) : test.info().outputPath(`${name}-${theme}.png`) });
  }
  await page.evaluate(() => window.haarness.preferences({ theme: "light" }));
}

test.beforeAll(async () => {
  test.setTimeout(120000);
  root = await realpath(await mkdtemp(join(tmpdir(), "haarness-card-leaving-")));
  project = join(root, "project");
  await mkdir(join(project, ".haarness"), { recursive: true });
  await writeFile(
    join(project, ".haarness/config.toml"),
    '[defaults]\nprovider="echo"\nmodel="echo-1"\n[permissions]\nbash="ask"\n',
  );
  env = isolatedEnv();
  Object.assign(env, {
    HAARNESS_BIN: binary,
    HAARNESS_HOME: join(root, "home"),
    HAARNESS_DATA_DIR: join(root, "data"),
    HAARNESS_DESKTOP_DATA: join(root, "desktop"),
    HAARNESS_TEST_PROVIDER: "1",
    HAARNESS_DESKTOP_TEST: "1",
  });
  delete env.ELECTRON_RUN_AS_NODE;
  delete env.ELECTRON_RENDERER_URL;
  app = await electron.launch({ args: ["."], env, timeout: 60000 });
  page = await app.firstWindow();
  page.setDefaultTimeout(15000);
  page.on("pageerror", (error) => errors.push(error.message));
  await app.evaluate(({ dialog }, path) => {
    dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [path] });
  }, project);
  // A first launch is a ready chat of This computer; the project opens from
  // the sidebar's +.
  await page.getByRole("button", { name: "Open another project", exact: true }).click();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 60000 });
  await page.getByRole("button", { name: "New chat", exact: true }).click();
  await expect(page.getByRole("textbox", { name: "Message", exact: true })).toBeVisible();
  // Enough history that the view has somewhere to move.
  await send("::long 40");
  await expect(page.locator(".transcript")).toContainText("line 40 marker-40");
  await idle();
  await settle();
});
test.afterAll(async () => {
  if (app) {
    await app.evaluate(({ BrowserWindow }) => { for (const win of BrowserWindow.getAllWindows()) win.destroy(); }).catch(() => {});
    await app.close().catch(() => {});
  }
  try { execFileSync(binary, ["--workspace", project, "serve", "--stop"], { env, stdio: "ignore", timeout: 30000 }); } catch {}
  stopChatHome(binary, env);
  await test.info().attach("fixture", { body: root ?? "not started", contentType: "text/plain" });
});

test("a permission card leaving, and its step and turn ending, move the view once each", async () => {
  for (let round = 0; round < 3; round++) {
    await send('!tool bash {"command":"sleep 1; echo hi"}');
    await expect(permission()).toBeVisible();
    await settle();
    if (round === 0) await pictures("card-waiting");
    await notePrompts();
    const seen = await frames(() => permission().getByRole("button", { name: "Allow once", exact: true }).click());
    // The card, the reply and the activity line: the last two in one frame
    // when they arrive together.
    expect(steady(seen)).toBeLessThanOrEqual(3);
    // The items coming in drew none of the person's messages anew.
    const prompts = await keptPrompts();
    expect(prompts.noted).toBeGreaterThan(0);
    expect(prompts.kept).toBe(prompts.noted);
    if (round === 0) await pictures("card-left");
  }
});

test("with the step opened as it waits, only its output moves the view as well", async () => {
  await send('!tool bash {"command":"sleep 1; echo hi"}');
  await expect(permission()).toBeVisible();
  const step = page.locator(".tool-call").last();
  await step.locator(":scope > summary").click();
  await expect(step).toHaveAttribute("open", "");
  await settle();
  const id = (await step.locator("xpath=ancestor::*[@data-item-id][1]").getAttribute("data-item-id"))!;
  const seen = await frames(() => permission().getByRole("button", { name: "Allow once", exact: true }).click());
  await expect(step.locator(".tool-output")).toContainText("hi");
  expect(steady(seen, [id])).toBeLessThanOrEqual(4);
});

test("a question card leaving moves the view once with the row of its answer", async () => {
  await send('!tool question {"prompt":"Which color?","choices":["Blue","Green"]}');
  await expect(question()).toBeVisible();
  await settle();
  const seen = await frames(() => question().getByRole("button", { name: "Blue", exact: true }).press("Enter"));
  expect(steady(seen)).toBeLessThanOrEqual(3);
  await expect(page.locator(".tool-call").filter({ hasText: "Which color?" })).toHaveCount(1);
  expect(errors).toEqual([]);
});

test("the running step it let through, opened, stays where it was clicked as a row above the view shrinks", async () => {
  // Long replies, one of them drawn wholly above the view once the step is in
  // (the nearest may still reach into the view, and the farthest may be out
  // of the list's drawn rows).
  for (let reply = 0; reply < 3; reply++) {
    await send("::long 30");
    await idle();
  }
  await settle();
  await send('!tool bash {"command":"sleep 4; echo held"}');
  await permission().getByRole("button", { name: "Allow once", exact: true }).click();
  await expect(permission()).toHaveCount(0);
  const step = page.locator(".tool-call").last();
  const summary = step.locator(":scope > summary");
  await settle();
  // Opened as it runs, while following: the end waits for more to be written.
  await summary.click();
  await expect(step).toHaveAttribute("open", "");
  await expect(step.locator(".tool-empty")).toHaveText("Waiting for output…");
  await settle();
  const at = () => summary.evaluate((element) => element.getBoundingClientRect().top);
  const before = await at();
  // The nearest drawn row wholly above the view made shorter, as one measured
  // anew smaller is (a wider window, an image's placeholder settling).
  const shrunk = await page.evaluate(() => {
    const list = document.querySelector<HTMLElement>(".transcript")!;
    const view = list.getBoundingClientRect();
    const row = [...list.querySelectorAll<HTMLElement>(".transcript-body > [data-index]")]
      .filter((row) => row.getBoundingClientRect().bottom < view.top - 50 && row.offsetHeight > 300)
      .sort((a, b) => b.getBoundingClientRect().bottom - a.getBoundingClientRect().bottom)[0];
    const text = row?.querySelector<HTMLElement>(".markdown");
    if (!text) return false;
    text.style.height = text.offsetHeight - Math.min(200, text.offsetHeight - 100) + "px";
    text.style.overflow = "hidden";
    return true;
  });
  expect(shrunk).toBe(true);
  await settle();
  expect(Math.abs((await at()) - before)).toBeLessThan(1);
  await idle();
  await expect(step.locator(".tool-output")).toContainText("held");
  expect(errors).toEqual([]);
});
