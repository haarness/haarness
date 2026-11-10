import { test, expect, _electron as electron, type ElectronApplication, type Page } from "@playwright/test";
import { mkdtemp, mkdir, writeFile, realpath, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";
import { stopChatHome } from "./fixtures/chat-home";

// Words selected in a finished reply show a small Comment button where the
// selection ends; its box puts the quoted words and the comment into the
// message box, as a Preview comment does. Nothing shows for any other
// selection, and the button's keys leave Find and the chat selection alone.
test.describe.configure({ mode: "serial" });

let app: ElectronApplication, page: Page;
let temporary: string, workspace: string, binary: string, env: Record<string, string>;
const errors: string[] = [];

const PROMPT = [
  "Caching keeps the build quick for everyone.",
  "",
  "Restart the daemon after changing its configuration, then check the log.",
  "",
  "A third paragraph closes the reply.",
].join("\n");

// By its role in the page, not its name, which follows the interface language.
const message = () => page.locator("[data-composer-input]");
const comment = () => page.locator(".transcript").getByRole("button", { name: "Comment", exact: true });
const reply = (text: string) =>
  page.locator('article[data-type="assistant_message"] .markdown[data-markdown-ready="true"]').filter({ hasText: text });

/** Selects text in the page from one phrase to another (or within one), as
 * a drag would; to the very end of `scope` when `to` is null. */
async function select(scope: ReturnType<Page["locator"]>, from: string, to: string | null = from) {
  await scope.first().evaluate(
    (host, words: { from: string; to: string | null }) => {
      const walker = document.createTreeWalker(host, NodeFilter.SHOW_TEXT);
      const texts: Text[] = [];
      for (let node = walker.nextNode(); node; node = walker.nextNode()) texts.push(node as Text);
      const start = texts.find((text) => text.data.includes(words.from))!;
      const range = document.createRange();
      range.setStart(start, start.data.indexOf(words.from));
      const to = words.to;
      if (to === null) range.setEnd(host, host.childNodes.length);
      else {
        const finish = texts.find(
          (text) =>
            text.data.includes(to) &&
            (text === start || start.compareDocumentPosition(text) & Node.DOCUMENT_POSITION_FOLLOWING),
        )!;
        range.setEnd(finish, finish.data.indexOf(to) + to.length);
      }
      const selection = document.getSelection()!;
      selection.removeAllRanges();
      selection.addRange(range);
    },
    { from, to },
  );
}

async function send(text: string, answer: string) {
  await message().fill(text);
  await message().press("Enter");
  await expect(reply(answer)).toBeVisible();
  await expect(page.locator(".composer")).not.toHaveClass(/running/);
}

async function theme(value: "light" | "dark") {
  await page.evaluate((theme) => window.haarness.preferences({ theme }), value);
  await expect(page.locator("html")).toHaveAttribute("data-theme", value);
  // Colours ease into the new theme; the screenshots show where they settle.
  await page.waitForTimeout(400);
}

test.beforeAll(async () => {
  test.setTimeout(240000);
  temporary = await realpath(await mkdtemp(join(process.env.HAARNESS_E2E_DIR ?? tmpdir(), "haarness-reply-comment-")));
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
  await page.setViewportSize({ width: 1400, height: 900 });
  await app.evaluate(({ dialog }, directory) => {
    dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [directory] });
  }, workspace);
  await page.getByRole("button", { name: "Open another project", exact: true }).click();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 90000 });
  await send(PROMPT, "Restart the daemon");
});

test.afterAll(async () => {
  await app?.close();
  if (workspace)
    try {
      execFileSync(binary, ["--workspace", workspace, "--trust-project", "serve", "--stop"], { env, stdio: "ignore", timeout: 30_000 });
    } catch {}
  stopChatHome(binary, env);
  if (temporary) await rm(temporary, { recursive: true, force: true }).catch(() => {});
});

test("words selected in a reply get a Comment button under their end, and its comment goes into the message", async ({}, info) => {
  const text = reply("Restart the daemon");
  await select(text, "Restart the daemon", "configuration");
  await expect(comment()).toBeVisible();
  // Under the line where the selection ends, starting at its end, inside the reply.
  const [spot, box, end] = await Promise.all([
    comment().boundingBox(),
    text.boundingBox(),
    page.evaluate(() => {
      const rects = [...document.getSelection()!.getRangeAt(0).getClientRects()].filter((rect) => rect.width > 0);
      const last = rects.at(-1)!;
      return { right: last.right, bottom: last.bottom };
    }),
  ]);
  expect(spot!.y).toBeGreaterThanOrEqual(end.bottom);
  expect(spot!.y - end.bottom).toBeLessThan(12);
  expect(Math.abs(spot!.x - end.right)).toBeLessThan(2);
  expect(spot!.x + spot!.width).toBeLessThanOrEqual(box!.x + box!.width + 1);
  // It lies over the next paragraph, which shows through nowhere.
  await expect(comment()).toHaveCSS("background-color", /^rgb\(/);
  for (const value of ["light", "dark"] as const) {
    await theme(value);
    await page.locator(".transcript").screenshot({ path: info.outputPath(`reply-comment-button-${value}.png`) });
  }

  await comment().click();
  const form = page.getByRole("form", { name: "Comment on “Restart the daemon after changing its configuration”" });
  await expect(form).toBeVisible();
  await expect(comment()).toHaveCount(0);
  const field = form.getByRole("textbox", { name: "Comment on the reply", exact: true });
  await expect(field).toBeFocused();
  await expect(field).toHaveAttribute("placeholder", "Comment on these words…");
  await field.fill("Which log, the daemon's or the build's?");
  for (const value of ["light", "dark"] as const) {
    await theme(value);
    await page.locator(".transcript").screenshot({ path: info.outputPath(`reply-comment-box-${value}.png`) });
  }
  await field.press("Enter");
  await expect(form).toHaveCount(0);
  await expect(message()).toHaveValue(
    "Comment on this part of your reply:\n> Restart the daemon after changing its configuration\n\nWhich log, the daemon's or the build's?",
  );
  await expect(message()).toBeFocused();
  await expect(comment()).toHaveCount(0);
  // Nothing is kept on the reply afterwards.
  expect(await page.evaluate(() => document.getSelection()?.isCollapsed ?? true)).toBe(true);

  // A second comment follows what the message box already holds.
  await message().fill("Before that:");
  await select(text, "Caching keeps", "everyone.");
  await comment().click();
  await page.getByRole("textbox", { name: "Comment on the reply", exact: true }).fill("Even on CI?");
  await page.getByRole("button", { name: "Add to message", exact: true }).click();
  await expect(message()).toHaveValue(
    "Before that:\n\nComment on this part of your reply:\n> Caching keeps the build quick for everyone.\n\nEven on CI?",
  );
  await message().fill("");
  expect(errors).toEqual([]);
});

test("across paragraphs the quote keeps its lines, and a code block's Copy is not in it", async () => {
  const text = reply("Restart the daemon");
  await select(text, "quick for everyone", "A third");
  await comment().click();
  await page.getByRole("textbox", { name: "Comment on the reply", exact: true }).fill("Split these.");
  await page.getByRole("textbox", { name: "Comment on the reply", exact: true }).press("Enter");
  await expect(message()).toHaveValue(
    "Comment on this part of your reply:\n> quick for everyone.\n>\n> Restart the daemon after changing its configuration, then check the log.\n>\n> A third\n\nSplit these.",
  );
  await message().fill("");

  await send("::md", "fn main");
  // To the end of the reply, past the code block's Copy button.
  const code = reply("fn main");
  await select(code, "and", null);
  await comment().click();
  await page.getByRole("textbox", { name: "Comment on the reply", exact: true }).fill("Why Rust?");
  await page.getByRole("textbox", { name: "Comment on the reply", exact: true }).press("Enter");
  await expect(message()).toHaveValue("Comment on this part of your reply:\n> and code\n>\n> fn main() {}\n\nWhy Rust?");
  await message().fill("");
  expect(errors).toEqual([]);
});

test("nothing shows for a selection outside one finished reply, or while the pointer is still selecting", async () => {
  // The words of a message you sent.
  await select(page.locator(".message.user .bubble").first(), "Caching keeps", "everyone.");
  await page.waitForTimeout(300);
  await expect(comment()).toHaveCount(0);
  // From one reply into the next message.
  await select(page.locator(".transcript-body"), "A third paragraph", "::md");
  await page.waitForTimeout(300);
  await expect(comment()).toHaveCount(0);

  // A drag shows the button once it is let go, not along the way. (It starts
  // with nothing selected: a press inside a selection would drag its text.)
  await page.evaluate(() => document.getSelection()?.removeAllRanges());
  const target = reply("Restart the daemon").locator("p").nth(1);
  const box = (await target.boundingBox())!;
  await page.mouse.move(box.x + 2, box.y + box.height / 2);
  await page.mouse.down();
  await page.mouse.move(box.x + box.width / 3, box.y + box.height / 2, { steps: 4 });
  await page.waitForTimeout(200);
  await expect(comment()).toHaveCount(0);
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2, { steps: 4 });
  await page.mouse.up();
  await expect(comment()).toBeVisible();
  // A click elsewhere in the reply leaves nothing selected, and the button goes.
  await page.mouse.click(box.x + 4, box.y + box.height / 2);
  await expect(comment()).toHaveCount(0);
  expect(errors).toEqual([]);
});

test("the keyboard reaches the button, opens the box and backs out, leaving Find and Select chats alone", async () => {
  // Picking chats in the sidebar (Command-click, Control-click elsewhere) is on meanwhile.
  const bar = page.getByRole("toolbar", { name: "Chat selection", exact: true });
  await page.locator('.project-list button[aria-current="true"]').first().click({ modifiers: [process.platform === "darwin" ? "Meta" : "Control"] });
  await expect(bar).toBeVisible();
  await message().press(process.platform === "darwin" ? "Meta+f" : "Control+f");
  const find = page.getByRole("searchbox", { name: "Find in chat", exact: true });
  await expect(find).toBeFocused();
  await find.fill("daemon");
  const text = reply("Restart the daemon");
  await select(text, "check the log");
  await expect(comment()).toBeVisible();
  await page.locator(".transcript").focus();
  await page.keyboard.press("Tab");
  await expect(comment()).toBeFocused();
  await page.keyboard.press("Enter");
  const field = page.getByRole("textbox", { name: "Comment on the reply", exact: true });
  await expect(field).toBeFocused();
  // Escape that an input method takes is its own.
  await field.evaluate((element) =>
    element.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", isComposing: true, bubbles: true, cancelable: true })),
  );
  await expect(field).toBeVisible();
  // Escape closes the box only: the words are selected again, the button has the keyboard, Find stays.
  await field.press("Escape");
  await expect(field).toHaveCount(0);
  await expect(comment()).toBeFocused();
  expect(await page.evaluate(() => document.getSelection()?.toString())).toBe("check the log");
  await expect(find).toBeVisible();
  // Escape on the button lets the words go, and Find still stays.
  await page.keyboard.press("Escape");
  await expect(comment()).toHaveCount(0);
  expect(await page.evaluate(() => document.getSelection()?.isCollapsed ?? true)).toBe(true);
  await expect(find).toBeVisible();
  await expect(bar).toBeVisible();
  await find.press("Escape");
  await expect(find).toHaveCount(0);
  await page.locator(".transcript").focus();
  await page.keyboard.press("Escape");
  await expect(bar).toHaveCount(0);

  // An empty box goes when the pointer goes elsewhere; one with words stays.
  await select(text, "check the log");
  await comment().click();
  await expect(field).toBeFocused();
  await text.locator("p").first().click();
  await expect(field).toHaveCount(0);
  await select(text, "check the log");
  await comment().click();
  await field.fill("Kept while I look around.");
  await reply("Restart the daemon").locator("p").first().click();
  await expect(field).toHaveValue("Kept while I look around.");
  await page.getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(field).toHaveCount(0);
  await expect(comment()).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(comment()).toHaveCount(0);
  expect(errors).toEqual([]);
});

/** Where the selection ends on screen: the caret there. */
const selectionEnd = () =>
  page.evaluate(() => {
    const end = document.getSelection()!.getRangeAt(0).cloneRange();
    end.collapse(false);
    return end.getBoundingClientRect().left;
  });

test("the button follows the text's direction where the selection ends, in either interface", async ({}, info) => {
  // An English passage in an Arabic window still ends on its right.
  await page.evaluate(() => window.haarness.preferences({ language: "ar" }));
  await expect(page.locator("html")).toHaveAttribute("dir", "rtl");
  const english = reply("Restart the daemon");
  await select(english, "Restart the daemon", "configuration");
  const button = page.locator(".reply-comment-button");
  await expect(button).toBeVisible();
  await expect(button).toHaveCSS("direction", "rtl");
  await expect.poll(async () => Math.abs((await button.boundingBox())!.x - (await selectionEnd()))).toBeLessThan(2);
  await english.locator("p").first().click();
  await expect(button).toHaveCount(0);

  // An Arabic passage ends on its left, and the button grows leftward from there.
  await send("::finish completed أعد تشغيل الخادم بعد تغيير الإعدادات ثم راجع السجل.", "أعد تشغيل");
  const arabic = reply("أعد تشغيل");
  await select(arabic, "تشغيل الخادم", "الإعدادات");
  await expect.poll(async () => {
    const box = (await button.boundingBox())!;
    return Math.abs(box.x + box.width - (await selectionEnd()));
  }).toBeLessThan(2);
  for (const value of ["light", "dark"] as const) {
    await theme(value);
    await page.locator(".transcript").screenshot({ path: info.outputPath(`reply-comment-button-ar-${value}.png`) });
  }
  await button.click();
  const field = page.locator(".reply-comment-card textarea");
  await expect(field).toBeFocused();
  await field.fill("أي سجل؟");
  for (const value of ["light", "dark"] as const) {
    await theme(value);
    await page.locator(".transcript").screenshot({ path: info.outputPath(`reply-comment-box-ar-${value}.png`) });
  }
  await field.press("Enter");
  await expect(message()).toHaveValue("Comment on this part of your reply:\n> تشغيل الخادم بعد تغيير الإعدادات\n\nأي سجل؟");
  await message().fill("");

  // Arabic words in an English window end on their left too.
  await page.evaluate(() => window.haarness.preferences({ language: "en" }));
  await expect(page.locator("html")).not.toHaveAttribute("dir", "rtl");
  await select(arabic, "أعد", "تشغيل");
  await expect.poll(async () => {
    const box = (await comment().boundingBox())!;
    return Math.abs(box.x + box.width - (await selectionEnd()));
  }).toBeLessThan(2);
  await page.locator(".transcript").screenshot({ path: info.outputPath("reply-comment-button-rtl-text.png") });
  await arabic.click();
  await expect(comment()).toHaveCount(0);
  expect(errors).toEqual([]);
});
