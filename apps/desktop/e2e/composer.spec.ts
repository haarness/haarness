import { test, expect, _electron as electron, type ElectronApplication, type Page, type TestInfo } from "@playwright/test";
import { mkdtemp, mkdir, writeFile, realpath } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";

/** An isolated Desktop on the echo provider, opened on a fresh project. */
async function launch(info: TestInfo, prepare?: (workspace: string) => Promise<void>) {
  const temporary = await realpath(await mkdtemp(join(tmpdir(), "haarness-composer-")));
  const workspace = join(temporary, "workspace");
  await mkdir(join(workspace, ".haarness"), { recursive: true });
  await writeFile(join(workspace, ".haarness/config.toml"), '[defaults]\nprovider="echo"\nmodel="echo-1"\n');
  await prepare?.(workspace);
  const binary = resolve(process.env.HAARNESS_BIN ?? "../../target/debug/haarness");
  const env: Record<string, string> = {
    ...process.env,
    HAARNESS_BIN: binary,
    HAARNESS_HOME: join(temporary, "home"),
    HAARNESS_DATA_DIR: join(temporary, "data"),
    HAARNESS_DESKTOP_DATA: join(temporary, "desktop"),
    HAARNESS_TEST_PROVIDER: "1",
    HAARNESS_DESKTOP_TEST: "1",
  };
  delete env.ELECTRON_RUN_AS_NODE;
  delete env.ELECTRON_RENDERER_URL;
  const app = await electron.launch({ args: ["."], env, timeout: 60000 });
  const page = await app.firstWindow();
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await app.evaluate(({ dialog }, directory) => {
    dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [directory] });
  }, workspace);
  await page.getByRole("region", { name: "Project home" }).getByRole("button", { name: "Open project", exact: true }).click();
  await ready(page);
  return {
    app,
    page,
    errors,
    async close() {
      await app.close().catch(() => {});
      try {
        execFileSync(binary, ["--workspace", workspace, "serve", "--stop"], { env, stdio: "ignore", timeout: 30000 });
      } catch {}
      await info.attach("fixture", { body: temporary, contentType: "text/plain" });
    },
  };
}

const ready = (page: Page) =>
  expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 60000 });

/** A Cmd+V as the focused element receives it. Nothing is inserted natively,
 * so text the app does not take is inserted the way a paste would. */
async function paste(page: Page, text: string) {
  await page.getByRole("textbox", { name: "Message", exact: true }).focus();
  await page.evaluate((text) => {
    const transfer = new DataTransfer();
    transfer.setData("text/plain", text);
    const target = document.activeElement ?? document.body;
    const event = new ClipboardEvent("paste", { clipboardData: transfer, bubbles: true, cancelable: true });
    if (target.dispatchEvent(event)) document.execCommand("insertText", false, text);
  }, text);
}

/** One picture of several parts of the window, such as a popover and the
 * box it opens from. */
async function shot(page: Page, parts: ReturnType<Page["locator"]>[], path: string) {
  const boxes = (await Promise.all(parts.map((part) => part.boundingBox()))).filter((box) => !!box);
  const x = Math.min(...boxes.map((box) => box.x)) - 8;
  const y = Math.min(...boxes.map((box) => box.y)) - 8;
  const right = Math.max(...boxes.map((box) => box.x + box.width)) + 8;
  const bottom = Math.max(...boxes.map((box) => box.y + box.height)) + 8;
  await page.screenshot({ path, clip: { x, y, width: right - x, height: bottom - y } });
}

async function theme(page: Page, value: "light" | "dark") {
  await page.evaluate((theme) => window.haarness.preferences({ theme }), value);
  await expect(page.locator("html")).toHaveAttribute("data-theme", value);
}

/** Reads what Copy wrote, with the window in front as a real click has it,
 * and puts the clipboard back as it was. */
async function copied(app: ElectronApplication, page: Page, button: ReturnType<Page["getByRole"]>) {
  const before = await app.evaluate(({ clipboard }) => clipboard.readText());
  await app.evaluate(({ app, BrowserWindow }) => {
    app.focus({ steal: true });
    BrowserWindow.getAllWindows()[0].focus();
  });
  await expect.poll(() => page.evaluate(() => document.hasFocus())).toBe(true);
  await button.focus();
  await button.press("Enter");
  let text = "";
  await expect.poll(async () => {
    text = await app.evaluate(({ clipboard }) => clipboard.readText());
    return text !== before;
  }).toBe(true);
  await app.evaluate(({ clipboard }, value) => clipboard.writeText(value), before);
  return text;
}

const lines = (count: number, mark: string) =>
  Array.from({ length: count }, (_, index) => `${mark} line ${index + 1}`).join("\n");

test("a long paste is a chip that survives a reload, previews, goes back to text, and is sent whole as a folded block", async ({}, info) => {
  const { app, page, errors, close } = await launch(info);
  try {
    const input = page.getByRole("textbox", { name: "Message", exact: true });
    const context = page.getByLabel("Attached context", { exact: true });
    const chips = context.locator(".prompt-context-item");
    const log = lines(30, "PASTE_MARK");
    // Up to the TUI's size a paste is ordinary text.
    await paste(page, lines(8, "short"));
    await expect(input).toHaveValue(lines(8, "short"));
    await expect(context).toHaveCount(0);
    await input.fill("");
    await paste(page, log);
    await expect(chips).toHaveCount(1);
    await expect(chips).toContainText("Pasted text · 30 lines");
    await expect(input).toHaveValue("");
    await expect(input).toBeFocused();
    // The draft keeps its chip across a reload.
    await page.reload();
    await ready(page);
    await expect(chips).toHaveCount(1);
    await expect(chips).toContainText("Pasted text · 30 lines");

    // Its preview scrolls through the whole text and closes with Escape.
    const preview = page.locator(".paste-preview");
    await chips.locator("summary").click();
    await expect(preview).toBeVisible();
    await expect(preview.locator("pre")).toContainText("PASTE_MARK line 30");
    for (const value of ["light", "dark"] as const) {
      await theme(page, value);
      await shot(page, [preview, page.locator(".composer")], info.outputPath(`paste-preview-${value}.png`));
    }
    await theme(page, "light");
    await page.keyboard.press("Escape");
    await expect(preview).toBeHidden();
    await expect(chips.locator("summary")).toBeFocused();

    // Edit as text puts it back in the message box, where Cmd+Z can take it out.
    await chips.locator("summary").click();
    await preview.getByRole("button", { name: "Edit as text", exact: true }).click();
    await expect(input).toHaveValue(log);
    await expect(context).toHaveCount(0);
    await expect(input).toBeFocused();

    // Several pastes make several chips; × removes one.
    await input.fill("Why does this fail?");
    await paste(page, log);
    await paste(page, lines(12, "SECOND_MARK"));
    await expect(chips).toHaveCount(2);
    await expect(input).toHaveValue("Why does this fail?");
    for (const value of ["light", "dark"] as const) {
      await theme(page, value);
      await page.locator(".composer").screenshot({ path: info.outputPath(`paste-chips-${value}.png`) });
    }
    await theme(page, "light");
    await page.getByRole("button", { name: "Remove context Pasted text · 12 lines", exact: true }).click();
    await expect(chips).toHaveCount(1);
    await expect(input).toBeFocused();

    await input.press("Enter");
    await expect(context).toHaveCount(0);
    const bubble = page.locator(".message.user .bubble").last();
    await expect(bubble).toContainText("Why does this fail?");
    const block = bubble.locator(".sent-paste");
    await expect(block).toContainText("Pasted text · 30 lines");
    await expect(bubble.locator(".sent-paste-text")).toHaveCount(0);
    // The model got all of it: the echo answers with every line.
    const reply = page.locator(".message.assistant").last();
    await expect(reply).toContainText("PASTE_MARK line 30");
    await expect(page.locator(".transcript")).not.toContainText("SECOND_MARK");
    for (const value of ["light", "dark"] as const) {
      await theme(page, value);
      await bubble.screenshot({ path: info.outputPath(`sent-paste-folded-${value}.png`) });
    }
    await theme(page, "light");
    // It opens in place.
    await block.getByRole("button", { name: "Pasted text · 30 lines" }).click();
    await expect(bubble.locator(".sent-paste-text")).toContainText("PASTE_MARK line 30");
    for (const value of ["light", "dark"] as const) {
      await theme(page, value);
      await bubble.screenshot({ path: info.outputPath(`sent-paste-open-${value}.png`) });
    }
    await theme(page, "light");
    // Copy gives the text as written and pasted, without the block it went in.
    const message = page.locator(".message.user").last();
    await message.hover();
    expect(await copied(app, page, message.getByRole("button", { name: "Copy message", exact: true })))
      .toBe("Why does this fail?\n\n" + log);
    expect(errors).toEqual([]);
  } finally {
    await close();
  }
});

test("a long message folds after twelve lines with a fade and Show more, and Find opens it", async ({}, info) => {
  const { page, errors, close } = await launch(info);
  try {
    const input = page.getByRole("textbox", { name: "Message", exact: true });
    // Typed, not pasted, so it stays text.
    await input.fill("Short message stays whole");
    await input.press("Enter");
    await expect(page.locator(".message.assistant").last()).toContainText("Echo: Short message stays whole");
    await expect(page.locator(".message.user").last().getByRole("button", { name: "Show more" })).toHaveCount(0);
    await input.fill(lines(30, "TYPED_MARK"));
    await input.press("Enter");
    await expect(page.locator(".message.assistant").last()).toContainText("TYPED_MARK line 30");
    const message = page.locator(".message.user").last();
    const more = message.getByRole("button", { name: "Show more", exact: true });
    await expect(more).toBeVisible();
    await expect(message.locator(".bubble-body")).toHaveClass(/folded/);
    const height = await message.locator(".bubble-body").evaluate((element) => element.getBoundingClientRect().height);
    const line = await message.locator(".bubble-body").evaluate((element) => parseFloat(getComputedStyle(element).lineHeight));
    expect(height).toBeLessThanOrEqual(line * 8 + 1);
    for (const value of ["light", "dark"] as const) {
      await theme(page, value);
      await message.screenshot({ path: info.outputPath(`long-message-folded-${value}.png`) });
    }
    await theme(page, "light");
    await more.click();
    await expect(message.getByRole("button", { name: "Show less", exact: true })).toBeVisible();
    await expect(message.locator(".bubble-body")).not.toHaveClass(/folded/);
    await message.getByRole("button", { name: "Show less", exact: true }).click();
    await expect(more).toBeVisible();
    // A Find match inside the fold opens it.
    await page.keyboard.press("ControlOrMeta+f");
    await page.locator(".conversation-find input").fill("TYPED_MARK line 29");
    await expect(message.locator(".bubble-body")).not.toHaveClass(/folded/);
    await page.keyboard.press("Escape");
    expect(errors).toEqual([]);
  } finally {
    await close();
  }
});

test("a slash command shows the line typed, folds the prompt it expanded to, and copies either", async ({}, info) => {
  const template = "REVIEW_TEMPLATE_START\nReview $ARGUMENTS for correctness.\nList each problem with its line.\nREVIEW_TEMPLATE_END";
  const { app, page, errors, close } = await launch(info, async (workspace) => {
    await mkdir(join(workspace, ".haarness/commands"), { recursive: true });
    await writeFile(join(workspace, ".haarness/commands/review-file.md"), `---\ndescription: Review one file\n---\n${template}\n`);
    // Undo walks the project's checkpoints, which a Git checkout has.
    for (const args of [["init", "-b", "main"], ["add", "."], ["-c", "user.name=Fixture", "-c", "user.email=fixture@example.test", "commit", "-m", "Fixture"]])
      execFileSync("git", args, { cwd: workspace, stdio: "ignore" });
  });
  try {
    const input = page.getByRole("textbox", { name: "Message", exact: true });
    await input.fill("/review-file src/foo.rs");
    await expect(page.getByRole("listbox", { name: "Slash commands" })).toBeHidden();
    await input.press("Enter");
    await expect(page.locator(".message.assistant").last()).toContainText("REVIEW_TEMPLATE_END");
    const message = page.locator(".message.user").last();
    const bubble = message.locator(".bubble");
    await expect(bubble.locator(".bubble-body")).toHaveText("/review-file src/foo.rs");
    const toggle = bubble.getByRole("button", { name: /Show prompt/ });
    await expect(toggle).toContainText("4 lines");
    await expect(bubble).not.toContainText("REVIEW_TEMPLATE_START");
    for (const value of ["light", "dark"] as const) {
      await theme(page, value);
      await message.screenshot({ path: info.outputPath(`command-folded-${value}.png`) });
    }
    await theme(page, "light");
    await toggle.press("Enter");
    await expect(bubble.locator(".command-prompt-text")).toContainText("Review src/foo.rs for correctness.");
    await expect(bubble.getByRole("button", { name: "Hide prompt" })).toHaveAttribute("aria-expanded", "true");
    for (const value of ["light", "dark"] as const) {
      await theme(page, value);
      await message.screenshot({ path: info.outputPath(`command-open-${value}.png`) });
    }
    await theme(page, "light");
    // The message copies the line typed; the prompt has its own copy.
    expect(await copied(app, page, bubble.getByRole("button", { name: "Copy prompt", exact: true })))
      .toContain("Review src/foo.rs for correctness.");
    await message.hover();
    expect(await copied(app, page, message.getByRole("button", { name: "Copy message", exact: true })))
      .toBe("/review-file src/foo.rs");
    // The prompt outline and Find read the line typed too.
    await page.keyboard.press("ControlOrMeta+f");
    await page.locator(".conversation-find input").fill("review-file src/foo");
    await expect(page.locator(".conversation-find [role=status]")).toHaveText("1 of 1 messages");
    await page.keyboard.press("Escape");
    // Undo brings back the line typed, so sending again runs the command.
    await expect(page.locator(".composer")).not.toHaveClass(/running/);
    await input.fill("/undo");
    await page.getByRole("button", { name: "Send message", exact: true }).click();
    await expect(input).toHaveValue("/review-file src/foo.rs");
    expect(errors).toEqual([]);
  } finally {
    await close();
  }
});
