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
const comment = () => page.locator(".transcript").getByRole("button", { name: "Comment on the selected words", exact: true });
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
  await send("::finish completed أعد تشغيل الخادم بعد تغيير الإعدادات ثم راجع السجل.\n\nسطر ثانٍ يختم الرد.", "أعد تشغيل");
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

/** The transcript's scroll position and what lies below the view. */
const view = () =>
  page.locator(".transcript").evaluate((list) => ({
    top: list.scrollTop,
    below: list.scrollHeight - list.scrollTop - list.clientHeight,
  }));

/** Scrolls the transcript with the wheel, as the reader would, to its top or its end. */
async function wheelTo(end: "top" | "end") {
  await page.locator(".transcript").hover();
  await expect.poll(async () => {
    await page.mouse.wheel(0, end === "top" ? -4000 : 4000);
    const now = await view();
    return end === "top" ? now.top : now.below;
  }).toBeLessThanOrEqual(1);
}

/** Where the selection starts and ends on screen. */
const selected = () =>
  page.evaluate(() => {
    const range = document.getSelection()!.getRangeAt(0);
    const lines = [...range.getClientRects()].filter((rect) => rect.width > 0);
    const end = range.cloneRange();
    end.collapse(false);
    return { top: lines[0].top, bottom: lines.at(-1)!.bottom, end: end.getBoundingClientRect().left };
  });

/** Within a pixel and a half, for edges placed on rounded pixels. */
const near = (a: number, b: number) => Math.abs(a - b) <= 1.5;
const centre = (box: { y: number; height: number }) => box.y + box.height / 2;

test("on a reply's last line the button sits in its footer, after Copy and Fork, in either direction", async ({}, info) => {
  const text = reply("Restart the daemon");
  // The reply's own Copy and Fork, in the footer under its text.
  const tools = text.locator("xpath=..").locator(".message-tools");
  // The selection ends early on the last line: the button waits after the actions.
  await select(text, "A third");
  await expect(comment()).toBeVisible();
  await expect.poll(async () => {
    const [button, actions] = [(await comment().boundingBox())!, (await tools.boundingBox())!];
    return [near(centre(button), centre(actions)), near(button.x, actions.x + actions.width + 8)];
  }).toEqual([true, true]);
  for (const value of ["light", "dark"] as const) {
    await theme(value);
    await page.locator(".transcript").screenshot({ path: info.outputPath(`reply-comment-footer-${value}.png`) });
  }
  // Ending further along, it starts where the selection ends, still in the footer's row.
  await select(text, "closes the reply.");
  await expect.poll(async () => {
    const button = (await comment().boundingBox())!;
    return [near(centre(button), centre((await tools.boundingBox())!)), near(button.x, (await selected()).end)];
  }).toEqual([true, true]);
  await text.locator("p").first().click();
  await expect(comment()).toHaveCount(0);
  // Right to left the footer is mirrored, and so is the button's place in it.
  await page.evaluate(() => window.haarness.preferences({ language: "ar" }));
  await expect(page.locator("html")).toHaveAttribute("dir", "rtl");
  const arabic = reply("أعد تشغيل");
  const arabicTools = arabic.locator("xpath=..").locator(".message-tools");
  await select(arabic, "سطر");
  const button = page.locator(".reply-comment-button");
  await expect.poll(async () => {
    const [box, actions] = [(await button.boundingBox())!, (await arabicTools.boundingBox())!];
    return [near(centre(box), centre(actions)), near(box.x + box.width, actions.x - 8)];
  }).toEqual([true, true]);
  for (const value of ["light", "dark"] as const) {
    await theme(value);
    await page.locator(".transcript").screenshot({ path: info.outputPath(`reply-comment-footer-ar-${value}.png`) });
  }
  await arabic.click();
  await expect(button).toHaveCount(0);
  await page.evaluate(() => window.haarness.preferences({ language: "en" }));
  await expect(page.locator("html")).not.toHaveAttribute("dir", "rtl");
  expect(errors).toEqual([]);
});

test("on the newest reply of a long chat the box opens over the words, and the chat stays where it is", async ({}, info) => {
  await send("::long 60", "marker-60");
  const long = reply("marker-60");
  await expect.poll(async () => (await view()).below).toBeLessThanOrEqual(4);
  const before = await view();
  await select(long, "line 60 marker-60");
  const words = await selected();
  await comment().click();
  const field = page.getByRole("textbox", { name: "Comment on the reply", exact: true });
  await expect(field).toBeFocused();
  const card = (await page.locator(".reply-comment-card").boundingBox())!;
  expect(card.y + card.height).toBeLessThanOrEqual(words.top);
  expect(Math.abs((await view()).top - before.top)).toBeLessThanOrEqual(1);
  await expect(page.locator(".jump-latest")).toHaveCount(0);
  await page.locator(".transcript").screenshot({ path: info.outputPath("reply-comment-box-over.png") });
  await field.press("Escape");
  await expect(field).toHaveCount(0);
  await expect(comment()).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(comment()).toHaveCount(0);
  await expect(page.locator(".jump-latest")).toHaveCount(0);
  expect((await view()).below).toBeLessThanOrEqual(4);
  expect(errors).toEqual([]);
});

test("while a box is open, a reply arriving does not move the chat, and following comes back when it closes", async () => {
  await message().fill("!linger 5000 ::long 80");
  await message().press("Enter");
  await expect(page.locator(".composer")).toHaveClass(/running/);
  await select(reply("marker-60"), "line 55 marker-55");
  await comment().click();
  const field = page.getByRole("textbox", { name: "Comment on the reply", exact: true });
  await expect(field).toBeFocused();
  await field.fill("Still here while it writes.");
  const held = await view();
  await expect(reply("marker-80")).toHaveCount(1, { timeout: 30_000 });
  await expect(page.locator(".composer")).not.toHaveClass(/running/);
  // The box kept its place and the keyboard.
  await expect(field).toBeFocused();
  await expect(field).toBeInViewport();
  expect(Math.abs((await view()).top - held.top)).toBeLessThanOrEqual(1);
  // Closing it takes the view to the newest output, with nothing left to jump to.
  await page.getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(field).toHaveCount(0);
  await expect.poll(async () => (await view()).below).toBeLessThanOrEqual(4);
  await expect(page.locator(".jump-latest")).toHaveCount(0);
  await expect(comment()).toHaveCount(0);
  expect(errors).toEqual([]);
});

test("a reply still being written offers nothing; once saved, its selected words get the button", async () => {
  await message().fill("::scrollstream");
  await message().press("Enter");
  const live = page.locator('[data-testid="streaming-reply"]');
  await expect(live).toContainText("STREAM_REFLOW_STAGE_ONE");
  await select(live, "STREAM_REFLOW_STAGE_ONE");
  await page.waitForTimeout(150);
  expect(
    await page.evaluate(() => ({
      writing: !!document.querySelector('[data-testid="streaming-reply"]'),
      buttons: document.querySelectorAll(".reply-comment-button").length,
    })),
  ).toEqual({ writing: true, buttons: 0 });
  // Once saved, the same words get the button.
  await expect(reply("STREAM_REFLOW_DONE")).toHaveCount(1);
  await expect(page.locator('[data-testid="streaming-reply"]')).toHaveCount(0);
  await select(reply("STREAM_REFLOW_DONE"), "STREAM_REFLOW_STAGE_ONE");
  await expect(comment()).toBeVisible();
  await reply("STREAM_REFLOW_DONE").locator("p").first().click();
  await expect(comment()).toHaveCount(0);
  expect(errors).toEqual([]);
});

test("another chat clears a box, and an agent's own chat offers nothing", async () => {
  await wheelTo("top");
  const text = reply("Restart the daemon");
  await select(text, "check the log");
  await comment().click();
  const field = page.getByRole("textbox", { name: "Comment on the reply", exact: true });
  await expect(field).toBeFocused();
  await field.fill("Kept only in this chat.");
  await message().fill(
    "!tool task " + JSON.stringify({ description: "Reply child", agent: "general", prompt: "child says hello" }),
  );
  await message().press("Enter");
  const task = page.getByRole("button", { name: "Open agent task: Reply child", exact: true });
  await expect(task).toBeVisible({ timeout: 30_000 });
  await expect(page.locator(".composer")).not.toHaveClass(/running/, { timeout: 30_000 });
  await task.click();
  const back = page.getByRole("button", { name: "Back to parent chat", exact: true });
  await expect(back).toBeVisible();
  await expect(field).toHaveCount(0);
  // The agent's chat has no message box to comment into.
  const child = reply("child says hello");
  await expect(child).toBeVisible();
  await select(child, "child says hello");
  await page.waitForTimeout(300);
  await expect(page.locator(".reply-comment-button")).toHaveCount(0);
  await back.click();
  await expect(back).toHaveCount(0);
  // Back in the chat, its reply is drawn again without the box.
  await wheelTo("top");
  await expect(reply("Restart the daemon")).toBeVisible();
  await expect(field).toHaveCount(0);
  expect(errors).toEqual([]);
});

test("words being written survive their reply scrolling out of the list and back", async () => {
  // Enough chat after the reply that the list stops drawing it at the end.
  for (const word of ["Filler one", "Filler two", "Filler three"]) await send(word, "Echo: " + word);
  await wheelTo("top");
  const text = reply("Restart the daemon");
  const id = await page.locator("[data-item-id]", { has: text }).getAttribute("data-item-id");
  await select(text, "check the log");
  await comment().click();
  const field = page.getByRole("textbox", { name: "Comment on the reply", exact: true });
  await expect(field).toBeFocused();
  await field.fill("Survives the scroll.");
  // Far enough down, the list no longer draws the reply's row at all.
  await wheelTo("end");
  await expect(page.locator(`[data-item-id="${id}"]`)).toHaveCount(0);
  await wheelTo("top");
  await expect(field).toHaveValue("Survives the scroll.");
  // Drawn again, it waits to be clicked rather than taking the keyboard.
  await expect(field).not.toBeFocused();
  await page.getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(field).toHaveCount(0);
  await page.keyboard.press("Escape");
  expect(errors).toEqual([]);
});

test("the button follows the words when the window's width moves them", async () => {
  await wheelTo("top");
  const text = reply("Restart the daemon");
  await select(text, "Restart the daemon", "check the log.");
  await expect(comment()).toBeVisible();
  const at = async () => {
    const [button, words] = [(await comment().boundingBox())!, await selected()];
    return { left: near(button.x, words.end), under: near(button.y, words.bottom + 4), words };
  };
  const wide = await at();
  expect([wide.left, wide.under]).toEqual([true, true]);
  await page.setViewportSize({ width: 700, height: 900 });
  await expect.poll(async () => {
    const narrow = await at();
    return [narrow.left, narrow.under, narrow.words.end !== wide.words.end || narrow.words.bottom !== wide.words.bottom];
  }).toEqual([true, true, true]);
  await page.setViewportSize({ width: 1400, height: 900 });
  await text.locator("p").first().click();
  await expect(comment()).toHaveCount(0);
  expect(errors).toEqual([]);
});
