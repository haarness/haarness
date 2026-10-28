import { test, expect, _electron as electron, type ElectronApplication, type Page, type TestInfo } from "@playwright/test";
import { mkdtemp, mkdir, writeFile, realpath } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";
import { createServer } from "node:http";
import { stopChatHome } from "./fixtures/chat-home";

/** An isolated Desktop on the echo provider, opened on a fresh project. */
async function launch(
  info: TestInfo,
  prepare?: (workspace: string) => Promise<void>,
  preferences?: Record<string, unknown>,
) {
  const temporary = await realpath(await mkdtemp(join(tmpdir(), "haarness-composer-")));
  const workspace = join(temporary, "workspace");
  await mkdir(join(workspace, ".haarness"), { recursive: true });
  await writeFile(join(workspace, ".haarness/config.toml"), '[defaults]\nprovider="echo"\nmodel="echo-1"\n');
  await prepare?.(workspace);
  // Preferences an earlier Desktop saved.
  if (preferences) {
    await mkdir(join(temporary, "desktop"), { recursive: true });
    await writeFile(join(temporary, "desktop/desktop.json"), JSON.stringify({ preferences }));
  }
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
  await page.getByRole("button", { name: "Open another project", exact: true }).click();
  await ready(page);
  return {
    app,
    page,
    errors,
    workspace,
    /** Stops the project's server, as an update or another client may. */
    stopServer: () =>
      execFileSync(binary, ["--workspace", workspace, "serve", "--stop"], { env, stdio: "ignore", timeout: 30000 }),
    async close() {
      await app.close().catch(() => {});
      try {
        execFileSync(binary, ["--workspace", workspace, "serve", "--stop"], { env, stdio: "ignore", timeout: 30000 });
      } catch {}
      stopChatHome(binary, env);
      await info.attach("fixture", { body: temporary, contentType: "text/plain" });
    },
  };
}

const ready = (page: Page) =>
  expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 60000 });

/** Make the project a Git checkout, which Undo's checkpoints need. */
function gitCheckout(workspace: string) {
  for (const args of [["init", "-b", "main"], ["add", "."], ["-c", "user.name=Fixture", "-c", "user.email=fixture@example.test", "commit", "-m", "Fixture"]])
    execFileSync("git", args, { cwd: workspace, stdio: "ignore" });
}

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
  const { app, page, errors, close } = await launch(info, async (workspace) => gitCheckout(workspace));
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
    // Undo takes the text out and the chip comes back; Redo swaps them again.
    await page.keyboard.press("ControlOrMeta+z");
    await expect(input).toHaveValue("");
    await expect(chips).toHaveCount(1);
    await expect(chips).toContainText("Pasted text · 30 lines");
    await page.keyboard.press("ControlOrMeta+Shift+z");
    await expect(input).toHaveValue(log);
    await expect(context).toHaveCount(0);

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
    // Undo brings the prompt back as it was written: its text, and its paste
    // as a chip.
    await expect(page.locator(".composer")).not.toHaveClass(/running/);
    await input.fill("/undo");
    await page.getByRole("button", { name: "Send message", exact: true }).click();
    await expect(input).toHaveValue("Why does this fail?");
    await expect(chips).toHaveCount(1);
    await expect(chips).toContainText("Pasted text · 30 lines");
    expect(errors).toEqual([]);
  } finally {
    await close();
  }
});

test("a paste-only prompt is named by its paste, a big paste says its size, and queued pastes stay chips", async ({}, info) => {
  const { page, errors, close } = await launch(info);
  try {
    const input = page.getByRole("textbox", { name: "Message", exact: true });
    const chips = page.getByLabel("Attached context", { exact: true }).locator(".prompt-context-item");
    await input.fill("First prompt");
    await input.press("Enter");
    await expect(page.locator(".message.assistant").last()).toContainText("Echo: First prompt");
    // A message that is only a paste: the outline names it by the paste.
    await paste(page, lines(9, "ONLY_PASTE"));
    await expect(chips).toHaveCount(1);
    await page.getByRole("button", { name: "Send message", exact: true }).click();
    await expect(page.locator(".message.assistant").last()).toContainText("ONLY_PASTE line 9");
    await expect(page.locator(".outline-dash")).toHaveCount(2);
    await expect
      .poll(async () => {
        const box = (await page.locator(".outline-dash").first().boundingBox())!;
        await page.mouse.move(box.x + 4, box.y + 3);
        await page.mouse.move(box.x + 4, box.y + 1);
        return page.locator(".outline").getAttribute("data-state");
      })
      .toBe("open");
    await expect(page.locator(".outline-row")).toHaveText(["First prompt", "Pasted text · 9 lines"]);
    await page.mouse.move(5, 5);

    // A paste too large to keep across a reload says so, once, and its chip
    // says its size; it is still sent.
    const big = Array.from({ length: 30_000 }, (_, index) => `big paste line ${index} ${"x".repeat(80)}`).join("\n");
    await paste(page, big);
    await expect(chips).toHaveCount(1);
    await expect(chips).toContainText(/Pasted text · 30,000 lines · \d(\.\d)? MB/);
    await expect(page.locator(".toast")).toHaveText("This paste is sent with your message, but it is too large to keep if Haarness reloads.");
    await expect(page.locator(".error-banner")).toHaveCount(0);
    for (const value of ["light", "dark"] as const) {
      await theme(page, value);
      await page.locator(".composer").screenshot({ path: info.outputPath(`paste-big-${value}.png`) });
    }
    await theme(page, "light");
    await page.getByRole("button", { name: /Remove context Pasted text · 30,000 lines/ }).click();
    await expect(chips).toHaveCount(0);

    // Queued while work runs, a message with a paste reads by its label and
    // comes back to edit as text and a chip.
    await input.fill("!linger 15000 slow work");
    await input.press("Enter");
    await expect(page.locator(".composer")).toHaveClass(/running/);
    await input.fill("Queued note");
    await paste(page, lines(12, "QUEUED_PASTE"));
    await expect(chips).toHaveCount(1);
    await input.press("Alt+Enter");
    const row = page.locator(".input-queue-row").first();
    await expect(row.locator(".input-queue-text")).toHaveText("Queued note · Pasted text · 12 lines");
    await expect(chips).toHaveCount(0);
    for (const value of ["light", "dark"] as const) {
      await theme(page, value);
      await page.locator(".composer-area").screenshot({ path: info.outputPath(`queued-paste-${value}.png`) });
    }
    await theme(page, "light");
    await row.getByRole("button", { name: "Edit queued message" }).click();
    await expect(input).toHaveValue("Queued note");
    await expect(chips).toHaveCount(1);
    await expect(chips).toContainText("Pasted text · 12 lines");
    await input.fill("Queued note, edited");
    await page.getByRole("button", { name: "Save queued message", exact: true }).click();
    await expect(row.locator(".input-queue-text")).toHaveText("Queued note, edited · Pasted text · 12 lines");
    await page.getByRole("button", { name: "Stop response", exact: true }).click().catch(() => {});
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
    // A prompt hook that takes a while holds the command's expansion back,
    // as a slow hook or a busy machine would.
    await writeFile(join(workspace, ".haarness/haarness.json"), JSON.stringify({
      hooks: { UserPromptSubmit: [{ hooks: [{ type: "command", command: "sleep 2" }] }] },
    }));
    // Undo walks the project's checkpoints, which a Git checkout has.
    gitCheckout(workspace);
  });
  try {
    const input = page.getByRole("textbox", { name: "Message", exact: true });
    await input.fill("/review-file src/foo.rs");
    await expect(page.getByRole("listbox", { name: "Slash commands" })).toBeHidden();
    await input.press("Enter");
    const message = page.locator(".message.user").last();
    const bubble = message.locator(".bubble");
    // Until the command has expanded, its prompt row is already there at the
    // template's size, so the bubble does not grow when the prompt arrives.
    const held = bubble.locator(".command-prompt-toggle");
    await expect(held).toBeDisabled();
    await expect(held).toHaveText("Show prompt · 4 lines");
    const before = (await bubble.boundingBox())!.height;
    await expect(held).toBeEnabled({ timeout: 20000 });
    expect((await bubble.boundingBox())!.height).toBe(before);
    await expect(page.locator(".message.assistant").last()).toContainText("REVIEW_TEMPLATE_END");
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

const settings = (page: Page) => page.getByRole("dialog", { name: "Settings", exact: true });
async function chooseThinking(page: Page, value: "Compact" | "Show" | "Hide") {
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  await settings(page).getByRole("textbox", { name: "Filter settings" }).fill("Thinking");
  await settings(page).getByRole("button", { name: "Thinking display Settings › General", exact: true }).click();
  const select = settings(page).getByRole("combobox", { name: "Thinking display", exact: true });
  await select.click();
  await page.getByRole("option", { name: value, exact: true }).click();
  await expect(select).toContainText(value);
  await settings(page).getByRole("button", { name: "Close settings", exact: true }).click();
}

test("thinking is Compact by default: one ticking activity line, then a Thought step that opens in place", async ({}, info) => {
  const { page, errors, close } = await launch(info);
  try {
    const input = page.getByRole("textbox", { name: "Message", exact: true });
    await input.fill("::thinkslow");
    await input.press("Enter");
    const activity = page.locator(".assistant-activity");
    const ticker = activity.locator(".activity-thought");
    // The newest line of thought ticks on the activity row, which keeps its
    // height, and nothing else of the thought is drawn while it streams.
    await expect(ticker).toContainText("and the files it names.");
    const before = await activity.boundingBox();
    await expect(activity).toContainText("Thinking");
    for (const value of ["light", "dark"] as const) {
      await theme(page, value);
      await activity.screenshot({ path: info.outputPath(`thinking-live-${value}.png`) });
    }
    await theme(page, "light");
    await expect(ticker).toContainText("Weighing two ways to fix the parser.");
    const after = await activity.boundingBox();
    expect(after?.height).toBe(before?.height);
    await expect(page.locator(".message.reasoning")).toHaveCount(0);
    await expect(page.locator(".transcript")).not.toContainText("Reading the request");

    // Settled: one quiet step, closed, and the reply under it.
    const reply = page.locator(".message.assistant").last();
    await expect(reply).toContainText("Thought it through");
    const thought = page.locator(".tool-call.thought");
    await expect(thought).toHaveCount(1);
    await expect(thought.locator("summary")).toHaveText(/^Thought for \d+s$/);
    await expect(thought).not.toHaveAttribute("open", "");
    await expect(page.locator(".transcript")).not.toContainText("keeps the public API stable");
    await expect(ticker).toHaveCount(0);
    const step = await thought.locator("summary").boundingBox();
    expect(step!.height).toBeLessThanOrEqual(32);
    for (const value of ["light", "dark"] as const) {
      await theme(page, value);
      await page.locator(".transcript").screenshot({ path: info.outputPath(`thought-folded-${value}.png`) });
    }
    await theme(page, "light");
    // It opens in place from the keyboard, and closes again.
    await thought.locator("summary").focus();
    await page.keyboard.press("Enter");
    await expect(thought.locator(".thought-body")).toContainText("The second keeps the public API stable.");
    for (const value of ["light", "dark"] as const) {
      await theme(page, value);
      await page.locator(".transcript").screenshot({ path: info.outputPath(`thought-open-${value}.png`) });
    }
    await theme(page, "light");
    await page.keyboard.press("Enter");
    await expect(thought.locator(".thought-body")).toHaveCount(0);
    // Find looks through a closed thought too, and opens it on a match.
    await page.keyboard.press("ControlOrMeta+f");
    await page.locator(".conversation-find input").fill("public API stable");
    await expect(page.locator(".conversation-find [role=status]")).toHaveText("1 of 1 messages");
    await expect(thought.locator(".thought-body")).toContainText("The second keeps the public API stable.");
    await page.keyboard.press("Escape");
    // Closing Find gives the keyboard back to what had it.
    await expect(thought.locator("summary")).toBeFocused();

    // Show writes the thought out in full; Hide leaves it out.
    await chooseThinking(page, "Show");
    await expect(page.locator(".message.reasoning")).toContainText("The second keeps the public API stable.");
    await expect(page.locator(".tool-call.thought")).toHaveCount(0);
    await chooseThinking(page, "Hide");
    await expect(page.locator(".message.reasoning")).toHaveCount(0);
    await expect(page.locator(".tool-call.thought")).toHaveCount(0);
    await expect(reply).toContainText("Thought it through");
    await input.fill("::thinkslow");
    await input.press("Enter");
    await expect(activity).toContainText("Thinking");
    await expect(activity.locator(".activity-thought")).toHaveCount(0);
    await expect(page.locator(".message.assistant").last()).toContainText("Thought it through");
    await expect(page.locator(".transcript")).not.toContainText("Weighing two ways");
    expect((await page.evaluate(() => window.haarness.boot())).preferences.thinking).toBe("hide");
    expect(errors).toEqual([]);
  } finally {
    await close();
  }
});

test("someone who had turned Show reasoning on keeps seeing thinking in full", async ({}, info) => {
  const { page, errors, close } = await launch(info, undefined, { showReasoning: true });
  try {
    expect((await page.evaluate(() => window.haarness.boot())).preferences.thinking).toBe("show");
    const input = page.getByRole("textbox", { name: "Message", exact: true });
    await input.fill("::thinkslow");
    await input.press("Enter");
    await expect(page.locator(".message.reasoning").first()).toContainText("Reading the request");
    // The thought being written says Thinking; the activity line keeps its
    // place without saying it a second time.
    await expect(page.locator(".assistant-activity")).toHaveClass(/activity-said/);
    await expect(page.locator(".message.assistant").last()).toContainText("Thought it through");
    await expect(page.locator(".message.reasoning")).toContainText("The second keeps the public API stable.");
    expect(errors).toEqual([]);
  } finally {
    await close();
  }
});

test("the message box keeps the keyboard through a send, and what is typed right after Enter is the next message", async ({}, info) => {
  const { app, page, errors, close } = await launch(info);
  try {
    const input = page.getByRole("textbox", { name: "Message", exact: true });
    const transcript = page.locator(".transcript");
    // A first message makes its chat and the view moves to it: the box keeps
    // the keyboard, and what was typed straight after Enter goes along.
    await input.fill("first message of the chat");
    await input.press("Enter");
    await page.keyboard.type("typed straight after");
    await expect(transcript).toContainText("Echo: first message of the chat");
    await expect(input).toBeFocused();
    await expect(input).toHaveValue("typed straight after");
    await expect(transcript).not.toContainText("typed straight after");
    // The new-chat page starts empty again; the chat kept its draft.
    await page.getByRole("button", { name: "New chat", exact: true }).click();
    await expect(input).toHaveValue("");
    await page.locator(".sessions").getByRole("button", { name: /first message of the chat/ }).first().click();
    await expect(input).toHaveValue("typed straight after");

    // In a chat: the box empties as Enter goes, keeps the keyboard, and the
    // next message is typed into it while the first one is still on its way.
    await input.fill("second message");
    await input.press("Enter");
    await expect(input).toHaveValue("");
    await expect(input).toBeEnabled();
    await page.keyboard.type("third, typed at once");
    await expect(transcript).toContainText("Echo: second message");
    await expect(input).toBeFocused();
    await expect(input).toHaveValue("third, typed at once");
    // Enter pressed again while a message is on its way sends the next one
    // once it has gone, in order and only once.
    await input.fill("fourth message");
    await input.press("Enter");
    await page.keyboard.type("fifth, sent right after");
    await page.keyboard.press("Enter");
    await page.keyboard.press("Enter");
    await expect(transcript).toContainText("Echo: fifth, sent right after");
    await expect(input).toHaveValue("");
    const sent = await page.locator(".message.user").allInnerTexts();
    expect(sent.filter((text) => text.includes("fifth, sent right after"))).toHaveLength(1);
    expect(sent.findIndex((text) => text.includes("fourth message"))).toBeLessThan(
      sent.findIndex((text) => text.includes("fifth, sent right after")),
    );

    // Enter in an empty box while work runs does nothing: no empty message,
    // no error, and Stop stays.
    await input.fill("!linger 8000 slow task");
    await input.press("Enter");
    const stop = page.getByRole("button", { name: "Stop response", exact: true });
    await expect(stop).toBeVisible();
    await expect(input).toBeFocused();
    const bubbles = await page.locator(".message.user").count();
    await input.press("Enter");
    await page.waitForTimeout(400);
    await expect(page.locator(".message.user")).toHaveCount(bubbles);
    await expect(page.getByRole("button", { name: "Dismiss error", exact: true })).toHaveCount(0);
    await expect(stop).toBeVisible();
    // A menu of the box takes Esc first: it closes, and the work goes on.
    await page.getByRole("button", { name: "Attach", exact: true }).click();
    await expect(page.getByRole("menu")).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(page.getByRole("menu")).toHaveCount(0);
    await expect(stop).toBeVisible();
    // So does the slash list (the box is not empty then).
    await input.fill("/");
    await expect(page.locator("#slash-commands")).toBeVisible();
    await input.press("Escape");
    await expect(page.locator("#slash-commands")).toHaveCount(0);
    await expect(stop).toHaveCount(0);
    await input.fill("");
    await expect(stop).toBeVisible();
    // Esc in the empty box stops the work, as Stop does.
    await input.focus();
    await input.press("Escape");
    await expect(page.locator(".turn-stopped")).toHaveCount(1);
    await expect(stop).toHaveCount(0);
    await expect(input).toBeFocused();

    // File → Stop response does the same, and with nothing running nothing.
    const menuStop = () =>
      app.evaluate(({ Menu, BrowserWindow }) => {
        const find = (items: Electron.MenuItem[]): Electron.MenuItem | undefined => {
          for (const item of items) {
            if (item.label === "Stop response") return item;
            const inner = item.submenu ? find(item.submenu.items) : undefined;
            if (inner) return inner;
          }
        };
        const item = find(Menu.getApplicationMenu()!.items);
        item?.click(undefined, BrowserWindow.getAllWindows()[0], undefined as never);
        return item?.accelerator;
      });
    expect(await menuStop()).toBe("CmdOrCtrl+.");
    await page.waitForTimeout(300);
    await input.fill("!linger 8000 another slow task");
    await input.press("Enter");
    await expect(stop).toBeVisible();
    await menuStop();
    await expect(page.locator(".turn-stopped")).toHaveCount(2);
    await expect(stop).toHaveCount(0);
    // Its key stops work from the message box too, though it is also the Next
    // agent key there: while work runs it stops it and leaves the agent as it is.
    const agent = page.getByRole("combobox", { name: "Agent", exact: true });
    const chosen = await agent.getAttribute("data-value");
    await input.fill("!linger 8000 a third slow task");
    await input.press("Enter");
    await expect(stop).toBeVisible();
    await expect(input).toBeFocused();
    await page.keyboard.press("ControlOrMeta+Period");
    await expect(page.locator(".turn-stopped")).toHaveCount(3);
    await expect(stop).toHaveCount(0);
    await expect(agent).toHaveAttribute("data-value", chosen ?? "");

    // A message that cannot go (the chat changed as it was sent) comes back
    // where it was typed, and the chat does not get it.
    const chat = page.locator(".sessions").getByRole("button", { name: /first message of the chat/ }).first();
    await page.getByRole("button", { name: "New chat", exact: true }).click();
    await input.fill("another chat");
    await input.press("Enter");
    await expect(transcript).toContainText("Echo: another chat");
    await chat.click();
    await expect(transcript).toContainText("Echo: second message");
    await input.fill("not lost");
    await page.evaluate(() => {
      const box = document.querySelector<HTMLTextAreaElement>("[data-composer-input]")!;
      box.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }));
      [...document.querySelectorAll<HTMLElement>(".sessions button")].find((button) => /another chat/.test(button.textContent ?? ""))!.click();
    });
    await expect(transcript).toContainText("Echo: another chat");
    await page.waitForTimeout(500);
    // Depending on how far it got, it ends quietly or says the chat changed.
    const dismiss = page.getByRole("button", { name: "Dismiss error", exact: true });
    if (await dismiss.count()) await dismiss.click();
    await chat.click();
    await expect(transcript).toContainText("Echo: second message");
    await page.waitForTimeout(500);
    // Depending on how far it got, it went or it came back: never both,
    // never neither.
    const back = (await input.inputValue()) === "not lost";
    const went = (await transcript.innerText()).includes("Echo: not lost");
    expect(back !== went).toBe(true);
    await input.fill("");
    expect(errors).toEqual([]);
  } finally {
    await close();
  }
});

test("every slash command row keeps its description, and the list scrolls", async ({}, info) => {
  const { page, errors, close } = await launch(info);
  try {
    const input = page.getByRole("textbox", { name: "Message", exact: true });
    await input.fill("/");
    const list = page.locator("#slash-commands");
    const rows = list.getByRole("option");
    await expect(rows.first()).toBeVisible();
    expect(await rows.count()).toBeGreaterThan(6);
    for (const row of await rows.all()) {
      const description = row.locator("span");
      const height = await description.evaluate((element) => element.getBoundingClientRect().height);
      expect(height, await row.locator("strong").innerText()).toBeGreaterThan(10);
    }
    expect(await list.evaluate((element) => element.scrollHeight > element.clientHeight)).toBe(true);
    // The last row scrolls into view when the selection wraps to it.
    await input.press("ArrowUp");
    await expect(rows.last()).toHaveAttribute("aria-selected", "true");
    await expect(rows.last()).toBeInViewport();
    await shot(page, [list, page.locator(".composer")], info.outputPath("slash-rows.png"));
    await input.press("Escape");
    expect(errors).toEqual([]);
  } finally {
    await close();
  }
});

test("a step opened at the end of the chat stays where it was clicked, and following pauses", async ({}, info) => {
  const { page, errors, close } = await launch(info, async (workspace) => {
    await writeFile(join(workspace, "README.md"), "# Fixture\n\nA README the agent reads.\n");
  });
  try {
    const input = page.getByRole("textbox", { name: "Message", exact: true });
    const transcript = page.locator(".transcript");
    await input.fill("::long 60");
    await input.press("Enter");
    await expect(transcript).toContainText("line 60 marker-60");
    await input.fill('!tool read {"path":"README.md"}');
    await input.press("Enter");
    await expect(transcript).toContainText("Tool read =>");
    const row = page.locator(".tool-call").filter({ hasText: "README.md" }).last();
    const summary = row.locator(":scope > summary");
    const settle = () => page.evaluate(() => new Promise((done) => setTimeout(() => requestAnimationFrame(() => requestAnimationFrame(done)), 300)));
    await settle();
    const at = async () => (await summary.boundingBox())!.y;
    const before = await at();
    await summary.click();
    await expect(row.locator(".tool-body")).toBeVisible();
    await settle();
    expect(Math.abs((await at()) - before)).toBeLessThan(2);
    // Its body pushed the newest output down, so following has paused.
    await expect(page.getByRole("button", { name: "Jump to latest", exact: true })).toBeVisible();
    // Closing it from the keyboard keeps it in place too.
    await summary.press("Enter");
    await expect(row.locator(".tool-body")).toHaveCount(0);
    await settle();
    expect(Math.abs((await at()) - before)).toBeLessThan(2);

    // While a reply streams below it, opening it holds it as well: the
    // stream is not followed past it, and Jump to latest offers the way on.
    await input.fill("::scrollstream");
    await input.press("Enter");
    await expect(transcript).toContainText("STREAM_REFLOW_STAGE_ONE");
    const streaming = await at();
    await summary.click();
    const places: number[] = [];
    for (let sample = 0; sample < 8; sample++) {
      await page.waitForTimeout(120);
      places.push(await at());
    }
    await expect(transcript).toContainText("STREAM_REFLOW_DONE");
    places.push(await at());
    for (const place of places) expect(Math.abs(place - streaming)).toBeLessThan(2);
    await expect(page.getByRole("button", { name: "Jump to latest", exact: true })).toBeVisible();
    expect(errors).toEqual([]);
  } finally {
    await close();
  }
});

test("a turn's reply has one footer, under its last text, with its full time; Copy takes the whole reply", async ({}, info) => {
  const { app, page, errors, close } = await launch(info, async (workspace) => {
    await writeFile(join(workspace, "README.md"), "# Fixture\n\nA README the agent reads.\n");
  });
  try {
    const input = page.getByRole("textbox", { name: "Message", exact: true });
    await input.fill('!preface read {"path":"README.md"}');
    await input.press("Enter");
    const replies = page.locator(".message.assistant");
    await expect(replies.last()).toContainText("Tool read =>");
    await expect(replies).toHaveCount(2);
    const footers = replies.locator(".message-footer");
    // The earlier text keeps the footer's height with nothing in it.
    await expect(footers.first().locator("time")).toHaveCount(0);
    await expect(footers.first().getByRole("button")).toHaveCount(0);
    const heights = await Promise.all([footers.first(), footers.last()].map(async (footer) => (await footer.boundingBox())!.height));
    expect(Math.abs(heights[0] - heights[1])).toBeLessThan(1);
    const time = footers.last().locator("time");
    await expect(time).toHaveAttribute("datetime", /^\d{4}-\d{2}-\d{2}T/);
    await expect(time).toHaveAttribute("title", new RegExp(String(new Date().getFullYear())));
    const text = await copied(app, page, footers.last().getByRole("button", { name: "Copy message" }));
    expect(text).toMatch(/^I’ll inspect the repository first\.\n\nTool read => /);
    await expect(footers.last().getByRole("button", { name: "Fork from message" })).toHaveCount(1);
    expect(errors).toEqual([]);
  } finally {
    await close();
  }
});

test("an edit's preview opens its file in Review, and an added file reads in one column", async ({}, info) => {
  const { page, errors, close } = await launch(info, async (workspace) => {
    await mkdir(join(workspace, "src"), { recursive: true });
    await writeFile(join(workspace, "src", "kept.ts"), "export const kept = 1;\n");
    gitCheckout(workspace);
  });
  try {
    await page.evaluate(() => window.haarness.preferences({ diffLayout: "split" } as never));
    const input = page.getByRole("textbox", { name: "Message", exact: true });
    for (const path of ["src/first.ts", "src/second.ts"]) {
      await input.fill('!tool write {"path":"' + path + '","content":"export const value = 1;\\n"}');
      await input.press("Enter");
      await expect(page.locator(".tool-call").filter({ hasText: path })).toHaveAttribute("data-status", "done");
    }
    const row = page.locator(".tool-call").filter({ hasText: "src/first.ts" });
    if ((await row.getAttribute("open")) === null) await row.locator(":scope > summary").click();
    // The preview's gutter says added, beside the code.
    await expect(row.locator(".diff-line.add .diff-sign").first()).toHaveText("+");
    await row.getByRole("button", { name: "Show in Review", exact: true }).click();
    const file = page.locator('.diff-file[data-file="src/first.ts"]');
    await expect(file).toBeVisible();
    // Added, it has one side: no empty half beside it, though the layout is Split.
    await expect(file.locator(".split-row")).toHaveCount(0);
    await expect(file.locator(".diff-line.add")).toHaveCount(1);
    await page.screenshot({ path: info.outputPath("show-in-review.png") });
    expect(errors).toEqual([]);
  } finally {
    await close();
  }
});

test("the queue's Steer now has an arrow of its own, and Remove hands the keyboard on", async ({}, info) => {
  const { page, errors, close } = await launch(info);
  try {
    const input = page.getByRole("textbox", { name: "Message", exact: true });
    await input.fill("!linger 15000 long running task");
    await input.press("Enter");
    await expect(page.getByRole("button", { name: "Stop response", exact: true })).toBeVisible();
    for (const text of ["first queued", "second queued"]) {
      await input.fill(text);
      await input.press("Alt+Enter");
      await expect(page.locator(".input-queue")).toContainText(text);
    }
    const tray = page.locator(".input-queue");
    const steer = tray.getByRole("button", { name: "Steer now", exact: true }).first();
    const send = page.locator(".send-button svg");
    await input.fill("draft");
    expect(await steer.locator("svg").innerHTML()).not.toBe(await send.innerHTML());
    await input.fill("");
    const remove = tray.getByRole("button", { name: "Remove queued message", exact: true });
    await remove.first().focus();
    await page.keyboard.press("Enter");
    await expect(tray.locator(".input-queue-row")).toHaveCount(1);
    await expect(remove.first()).toBeFocused();
    await page.keyboard.press("Enter");
    await expect(tray).toHaveCount(0);
    await expect(input).toBeFocused();
    await page.getByRole("button", { name: "Stop response", exact: true }).click();
    expect(errors).toEqual([]);
  } finally {
    await close();
  }
});

test("the new-chat heading holds still as the box grows, @ lists as wide as the box, and replies keep task boxes", async ({}, info) => {
  const { page, errors, close } = await launch(info);
  try {
    const input = page.getByRole("textbox", { name: "Message", exact: true });
    const heading = page.locator(".conversation-empty h2");
    await expect(heading).toBeVisible();
    // Let the hero's entrance finish before measuring where it rests.
    await page.evaluate(() => Promise.all(document.getAnimations().map((animation) => animation.finished.catch(() => {}))));
    const rest = (await heading.boundingBox())!.y;
    await page.evaluate(() => {
      const transfer = new DataTransfer();
      const png = Uint8Array.from(atob("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg=="), (c) => c.charCodeAt(0));
      transfer.items.add(new File([png], "pixel.png", { type: "image/png" }));
      document.querySelector(".conversation")!.dispatchEvent(new DragEvent("drop", { dataTransfer: transfer, bubbles: true, cancelable: true }));
    });
    await expect(page.locator(".composer-attachments")).toBeVisible();
    expect(Math.abs((await heading.boundingBox())!.y - rest)).toBeLessThan(1);
    await page.getByRole("button", { name: "Remove pixel.png", exact: true }).click();

    // The @ list is as wide as the message box it completes.
    await input.fill("@");
    const popover = page.locator(".mention-popover");
    await expect(popover).toBeVisible();
    const [list, box] = await Promise.all([popover.boundingBox(), page.locator(".composer").boundingBox()]);
    expect(Math.abs(list!.width - (box!.width - 2))).toBeLessThan(3);
    await input.press("Escape");

    // A reply's task list keeps its boxes, ticked as written and read-only;
    // its table heads read in the reply's own case.
    await input.fill("Checklist\n\n- [ ] open task\n- [x] finished task\n\n| Name | Status |\n| --- | --- |\n| parser | done |");
    await input.press("Enter");
    const reply = page.locator(".message.assistant").last();
    await expect(reply).toContainText("finished task");
    const boxes = reply.locator("input.task-box");
    await expect(boxes).toHaveCount(2);
    await expect(boxes.nth(0)).not.toBeChecked();
    await expect(boxes.nth(1)).toBeChecked();
    await expect(boxes.nth(1)).toBeDisabled();
    await expect(reply.locator("th").first()).toHaveCSS("text-transform", "none");
    await expect(reply.locator("th").first()).toHaveText("Name");
    expect(errors).toEqual([]);
  } finally {
    await close();
  }
});

test("a project whose server started again with another configuration offers its new level and commands", async ({}, info) => {
  const configured = (level: string) => `[defaults]\nprovider="echo"\nmodel="echo-1"\nreasoning_effort="${level}"\n`;
  const { page, errors, close, workspace, stopServer } = await launch(info, (folder) =>
    writeFile(join(folder, ".haarness/config.toml"), configured("high")),
  );
  try {
    // Echo lists no levels, so the pill shows the configured one.
    const effort = page.locator(".composer-model .model-effort-summary");
    await expect(effort).toHaveText("High");
    // A configuration the server cannot read keeps it down after it stops,
    // so what changes next reaches no reload: the window starts the server
    // again itself once it can.
    await writeFile(join(workspace, ".haarness/config.toml"), "[defaults\n");
    stopServer();
    await page.waitForTimeout(1500);
    await mkdir(join(workspace, ".haarness/commands"), { recursive: true });
    await writeFile(join(workspace, ".haarness/commands/fresh-review.md"), "---\ndescription: Review again\n---\nReview $ARGUMENTS.\n");
    await writeFile(join(workspace, ".haarness/config.toml"), configured("low"));
    await expect(effort).toHaveText("Low", { timeout: 60000 });
    const input = page.getByRole("textbox", { name: "Message", exact: true });
    await input.fill("/fresh");
    await expect(page.locator("#slash-commands").getByRole("option")).toContainText(["fresh-review"]);
    await input.press("Escape");
    expect(errors).toEqual([]);
  } finally {
    await close();
  }
});

test("a configured reasoning level is the one in use until Model default overrides it, and each request says so", async ({}, info) => {
  // An OpenAI-compatible model with three levels, which records every turn
  // it is sent (a chat's title is asked for in a request of its own).
  const turns: Record<string, unknown>[] = [];
  const provider = createServer(async (request, response) => {
    if (request.method === "GET") {
      response.writeHead(200, { "Content-Type": "application/json" });
      response.end(JSON.stringify({ data: [{ id: "effort-model", supported_parameters: ["tools", "reasoning_effort"], reasoning_efforts: ["low", "medium", "high"] }] }));
      return;
    }
    const chunks: Buffer[] = [];
    for await (const chunk of request) chunks.push(Buffer.from(chunk));
    const body = JSON.parse(Buffer.concat(chunks).toString());
    if (!JSON.stringify(body.messages).includes("Write a title")) turns.push(body);
    response.writeHead(200, { "Content-Type": "text/event-stream" });
    response.end("data: " + JSON.stringify({ choices: [{ index: 0, delta: { content: "effort-reply" }, finish_reason: null }] })
      + "\n\ndata: " + JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: "stop" }] }) + "\n\ndata: [DONE]\n\n");
  });
  await new Promise<void>((done) => provider.listen(0, "127.0.0.1", done));
  const address = provider.address();
  if (!address || typeof address === "string") throw Error("Provider fixture did not bind");
  const { page, errors, close } = await launch(info, async (workspace) => {
    await writeFile(
      join(workspace, ".haarness/config.toml"),
      `[defaults]\nprovider="effort-fixture"\nmodel="effort-model"\nreasoning_effort="high"\n[providers.effort-fixture]\nname="Effort fixture"\nprotocol="openai-chat-completions"\nbase_url="http://127.0.0.1:${address.port}/v1"\nrequires_auth=false\ntools=true\nreasoning=true\n`,
    );
    await mkdir(join(workspace, ".haarness/commands"), { recursive: true });
    await writeFile(join(workspace, ".haarness/commands/review-code.md"), "---\ndescription: Review code\n---\nReview $ARGUMENTS.\n");
  });
  try {
    const model = page.getByRole("combobox", { name: "Model", exact: true });
    const slider = page.getByRole("slider", { name: "Reasoning effort", exact: true });
    const input = page.getByRole("textbox", { name: "Message", exact: true });
    const heading = page.locator(".reasoning-current strong");
    const done = page.locator(".select-footer").getByRole("button", { name: "Done", exact: true });
    async function ask(text: string, replies: number) {
      await input.fill(text);
      await input.press("Enter");
      await expect(page.locator(".message.assistant")).toHaveCount(replies);
      await expect.poll(() => turns.length).toBe(replies);
      await expect(page.getByRole("button", { name: "Stop response", exact: true })).toHaveCount(0);
      return turns.at(-1)!;
    }
    // The thumb moves at once, so each picture shows where it stops.
    await page.emulateMedia({ reducedMotion: "reduce" });
    await expect(model).toHaveAttribute("data-value", "effort-fixture/effort-model");
    // No choice made: the configured level is the one shown, on the pill too,
    // and the picker marks it as the default.
    await expect(model.locator(".model-effort-summary")).toHaveText("High");
    await model.click();
    await expect(slider).toHaveAttribute("aria-valuetext", "High");
    await expect(heading).toHaveText("High default");
    await expect(page.locator(".reasoning-scale span").first()).toHaveText("Model default");
    const picker = page.locator(".select-popover");
    for (const value of ["light", "dark"] as const) {
      await theme(page, value);
      await shot(page, [picker, model], info.outputPath(`configured-level-${value}.png`));
    }
    await done.click();
    await page.evaluate(() => window.haarness.preferences({ language: "ar" }));
    await expect(page.locator("html")).toHaveAttribute("dir", "rtl");
    // The pill's own name is Arabic now; its class is not. The thumb stands
    // inside the track at its far end, which reads leftward.
    const pill = page.locator(".composer-model");
    await pill.click();
    await expect(page.locator(".reasoning-default")).toHaveText("افتراضي");
    const [track, thumb] = await Promise.all([page.locator(".reasoning-slider").boundingBox(), page.locator(".reasoning-thumb").boundingBox()]);
    expect(thumb!.x).toBeGreaterThanOrEqual(track!.x);
    expect(thumb!.x - track!.x).toBeLessThan(10);
    for (const value of ["light", "dark"] as const) {
      await theme(page, value);
      await shot(page, [picker, pill], info.outputPath(`configured-level-ar-${value}.png`));
    }
    await page.keyboard.press("Escape");
    await expect(picker).toHaveCount(0);
    await page.evaluate(() => window.haarness.preferences({ language: "en" }));
    await expect(page.locator("html")).not.toHaveAttribute("dir", "rtl");
    expect((await ask("first at the configured level", 1)).reasoning_effort).toBe("high");

    // Model default is a choice of its own: the request names no effort, and
    // a slash command runs at it too.
    const reset = page.getByRole("button", { name: "Reset to default", exact: true });
    await model.click();
    await slider.focus();
    await slider.press("Home");
    await expect(slider).toHaveAttribute("aria-valuetext", "Model default");
    await expect(heading).toHaveText("Model default");
    await expect(reset).toBeEnabled();
    for (const value of ["light", "dark"] as const) {
      await theme(page, value);
      await shot(page, [picker, model], info.outputPath(`model-default-${value}.png`));
    }
    await done.click();
    await expect(model.locator(".model-effort-summary")).toHaveCount(0);
    expect(await ask("second at the model default", 2)).not.toHaveProperty("reasoning_effort");
    expect(await ask("/review-code at the model default", 3)).not.toHaveProperty("reasoning_effort");

    // Reset follows the configuration again: nothing is named, and the turn
    // runs at the configured level.
    await model.click();
    await reset.click();
    await expect(slider).toHaveAttribute("aria-valuetext", "High");
    await expect(heading).toHaveText("High default");
    await expect(reset).toBeDisabled();
    await done.click();
    expect((await ask("fourth back on the configured level", 4)).reasoning_effort).toBe("high");

    // A level picked goes out as it is, for a message and a slash command.
    await model.click();
    await slider.focus();
    await slider.press("Home");
    await slider.press("ArrowRight");
    await expect(slider).toHaveAttribute("aria-valuetext", "Low");
    await expect(heading).toHaveText("Low");
    await done.click();
    expect((await ask("fifth at low", 5)).reasoning_effort).toBe("low");
    expect((await ask("/review-code at low", 6)).reasoning_effort).toBe("low");

    // A model that lists no levels is sent the configured one as written, so
    // the picker shows it, with Model default to override it.
    await model.click();
    await picker.locator('[role="option"][data-value="echo/echo-1"]').click();
    await expect(model).toHaveAttribute("data-value", "echo/echo-1");
    await expect(slider).toHaveAttribute("max", "1");
    await expect(slider).toHaveAttribute("aria-valuetext", "High");
    await expect(heading).toHaveText("High default");
    await expect(reset).toBeDisabled();
    for (const value of ["light", "dark"] as const) {
      await theme(page, value);
      await shot(page, [picker, model], info.outputPath(`no-levels-${value}.png`));
    }
    await done.click();
    await page.evaluate(() => window.haarness.preferences({ language: "ar" }));
    await expect(page.locator("html")).toHaveAttribute("dir", "rtl");
    await pill.click();
    await expect(page.locator(".reasoning-default")).toHaveText("افتراضي");
    for (const value of ["light", "dark"] as const) {
      await theme(page, value);
      await shot(page, [picker, pill], info.outputPath(`no-levels-ar-${value}.png`));
    }
    // The slider's own name is Arabic too.
    const range = page.locator(".reasoning-slider input");
    await range.focus();
    await range.press("Home");
    await expect(page.locator(".reasoning-default")).toHaveCount(0);
    for (const value of ["light", "dark"] as const) {
      await theme(page, value);
      await shot(page, [picker, pill], info.outputPath(`model-default-ar-${value}.png`));
    }
    await page.keyboard.press("Escape");
    await expect(picker).toHaveCount(0);
    await page.evaluate(() => window.haarness.preferences({ language: "en" }));
    await expect(page.locator("html")).not.toHaveAttribute("dir", "rtl");
    expect(errors).toEqual([]);
  } finally {
    await close();
    await new Promise<void>((resolve) => provider.close(() => resolve()));
  }
});
