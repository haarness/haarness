import { test, expect, _electron as electron, type ElectronApplication, type Page, type TestInfo } from "@playwright/test";
import { mkdtemp, mkdir, writeFile, realpath } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";

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
    await expect(page.locator(".message.assistant").last()).toContainText("Thought it through");
    await expect(page.locator(".message.reasoning")).toContainText("The second keeps the public API stable.");
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
