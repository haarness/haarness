import { test as base, expect, _electron, type ElectronApplication, type Page } from "@playwright/test";
import { mkdtemp, mkdir, realpath, writeFile, readFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { tmpdir } from "node:os";
import { execFileSync } from "node:child_process";

type Fixture = { app: ElectronApplication; page: Page; a: string; b: string; temp: string };
const test = base.extend<{ desktop: Fixture }>({
  desktop: async ({}, use, info) => {
    const temp = await realpath(await mkdtemp(join(tmpdir(), "haarness-regressions-")));
    const a = join(temp, "project-a"), b = join(temp, "project-b");
    for (const directory of [a, b]) {
      await mkdir(join(directory, ".haarness"), { recursive: true });
      await writeFile(join(directory, ".haarness/config.toml"), '[defaults]\nprovider="echo"\nmodel="echo-1"\n[permissions]\nbash="ask"\n');
    }
    for (const name of ["alpha", "beta", "gamma", "delta", "epsilon", "slow", "fast"])
      await writeFile(join(a, name + ".txt"), "Original " + name);
    await writeFile(join(b, "alpha.txt"), "Other project alpha");
    await writeFile(join(b, "second.txt"), "Second project");
    await writeFile(join(a, "proof.png"), Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Wl6Z9kAAAAASUVORK5CYII=", "base64"));
    const binary = resolve("../../target/debug/haarness");
    const env: Record<string, string> = { ...process.env, HAARNESS_BIN: binary, HAARNESS_HOME: join(temp, "home"), HAARNESS_DATA_DIR: join(temp, "data"), HAARNESS_DESKTOP_DATA: join(temp, "desktop"), HAARNESS_TEST_PROVIDER: "1", HAARNESS_DESKTOP_TEST: "1" };
    delete env.ELECTRON_RUN_AS_NODE;
    delete env.ELECTRON_RENDERER_URL;
    const app = await _electron.launch({ args: ["."], env, timeout: 60000 });
    // Install before creating SDK clients, which capture their fetch function.
    await app.evaluate(() => {
      const state = globalThis as any;
      const fetch = globalThis.fetch;
      globalThis.fetch = async (...args) => {
        if (state.match && String(args[0]).includes(state.match) && (args[1]?.method ?? "GET") === state.method) {
          state.held++;
          await state.gate;
        }
        return fetch(...args);
      };
    });
    const page = await app.firstWindow();
    page.setDefaultTimeout(10000);
    const errors: string[] = [];
    page.on("pageerror", (error) => errors.push(error.message));
    await page.context().tracing.start({ screenshots: true, snapshots: true, sources: true });
    try {
      await choose(app, a);
      await page.getByRole("region", { name: "Project home" }).getByRole("button", { name: "Open project", exact: true }).click();
      await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 60000 });
      await use({ app, page, a, b, temp });
      expect(errors).toEqual([]);
    } finally {
      await page.screenshot({ path: info.outputPath("final.png") }).catch(() => {});
      await page.context().tracing.stop({ path: info.outputPath("trace.zip") }).catch(() => {});
      // The scenario has finished; destroy only this fixture's windows so a
      // deliberate unsaved draft cannot enter Electron's native unload prompt.
      await app.evaluate(({ BrowserWindow }) => {
        for (const win of BrowserWindow.getAllWindows()) win.destroy();
      }).catch(() => {});
      await app.close().catch(() => {});
      for (const directory of [a, b]) {
        try { execFileSync(binary, ["--workspace", directory, "serve", "--stop"], { env, stdio: "ignore", timeout: 30000 }); } catch {}
      }
      await info.attach("fixture", { body: temp, contentType: "text/plain" });
    }
  },
});
const button = (page: Page, name: string) => page.getByRole("button", { name, exact: true });
const editor = (page: Page) => page.locator(".cm-content");
const file = (page: Page, name: string) => page.locator(".file-entry").filter({ hasText: name });
async function choose(app: ElectronApplication, path: string) {
  await app.evaluate(({ dialog }, path) => { dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [path] }); }, path);
}
async function files(page: Page) {
  if (await button(page, "Show side panel").isVisible()) await button(page, "Show side panel").click();
  if (await button(page, "Show files panel").isVisible()) await button(page, "Show files panel").click();
  else if (!await page.locator(".explorer").isVisible()) {
    await button(page, "Open panel").click(); await button(page, "Open Files").click();
  }
}
async function switchProject(desktop: Fixture) {
  await choose(desktop.app, desktop.b); await button(desktop.page, "Open another project").click();
  const confirm = desktop.page.getByRole("dialog", { name: "Discard unsaved file edits?" });
  if (await confirm.isVisible()) await confirm.getByRole("button", { name: "Discard edits", exact: true }).click();
  await expect(desktop.page.locator(".titlebar")).toContainText("project-b");
  await expect(desktop.page.locator(".application")).toHaveAttribute("data-project-ready", "true");
}
/** Hold real network operations at a deterministic gate, without faking data. */
async function hold(app: ElectronApplication, method: string, match = "/v1/files/content") {
  await app.evaluate((_, { method, match }) => {
    const state = globalThis as any;
    state.method = method;
    state.match = match;
    state.held = 0;
    state.gate = new Promise<void>((resolve) => state.release = resolve);
  }, { method, match });
}
async function release(app: ElectronApplication) {
  await app.evaluate(() => { (globalThis as any).match = ""; (globalThis as any).release(); });
}
async function held(app: ElectronApplication, count = 1) {
  await expect.poll(() => app.evaluate(() => (globalThis as any).held)).toBe(count);
}

test("audit: saving retains focus, undo and redo; switching files retains separate histories", async ({ desktop: { page, a } }) => {
  await files(page); await file(page, "alpha.txt").click();
  await editor(page).fill("Edited alpha"); await editor(page).press("Meta+s");
  await expect(button(page, "Save")).toBeDisabled();
  await expect(editor(page)).toBeFocused();
  await editor(page).press("Meta+z"); await expect(editor(page)).toHaveText("Original alpha");
  await editor(page).press("Meta+Shift+z"); await expect(editor(page)).toHaveText("Edited alpha");
  await file(page, "beta.txt").click(); await editor(page).fill("Edited beta");
  await file(page, "alpha.txt").click(); await editor(page).press("Meta+z"); await expect(editor(page)).toHaveText("Original alpha");
  await file(page, "beta.txt").click(); await editor(page).press("Meta+z"); await expect(editor(page)).toHaveText("Original beta");
  expect(await readFile(join(a, "alpha.txt"), "utf8")).toBe("Edited alpha");
});

test("audit: pending saves retain newer edits and reject external conflicts without losing history", async ({ desktop: { app, page, a } }) => {
  await files(page); await file(page, "alpha.txt").click(); await editor(page).fill("Saved version");
  await hold(app, "PUT"); await editor(page).press("Meta+s"); await held(app);
  await editor(page).fill("Newer unsaved version"); await editor(page).press("Meta+s"); await held(app);
  await release(app);
  await expect.poll(() => readFile(join(a, "alpha.txt"), "utf8")).toBe("Saved version");
  await expect(editor(page)).toHaveText("Newer unsaved version"); await expect(button(page, "Save")).toBeEnabled();
  await editor(page).press("Meta+s"); await expect(button(page, "Save")).toBeDisabled();
  expect(await readFile(join(a, "alpha.txt"), "utf8")).toBe("Newer unsaved version");
  await writeFile(join(a, "alpha.txt"), "External edit");
  await editor(page).fill("Local edit"); await editor(page).press("Meta+s");
  await expect(page.getByRole("alert")).toContainText(/changed|conflict/i);
  await expect(editor(page)).toHaveText("Local edit"); await expect(button(page, "Save")).toBeEnabled();
  expect(await readFile(join(a, "alpha.txt"), "utf8")).toBe("External edit");
});

test("audit: duplicate opens share one read and slow reads respect latest selection", async ({ desktop: { app, page } }) => {
  await files(page); await hold(app, "GET"); await file(page, "alpha.txt").dblclick(); await held(app);
  await release(app); await expect(button(page, "Close alpha.txt")).toHaveCount(1);
  await hold(app, "GET", "slow.txt"); await file(page, "slow.txt").click(); await held(app);
  await file(page, "fast.txt").click(); await expect(editor(page)).toHaveText("Original fast");
  await release(app); await expect(button(page, "Close slow.txt")).toHaveCount(1);
  await expect(editor(page)).toHaveText("Original fast");
  await hold(app, "GET", "beta.txt"); await file(page, "beta.txt").click(); await held(app);
  await page.locator(".file-tabs").getByRole("button", { name: "alpha.txt", exact: true }).click();
  await release(app); await expect(button(page, "Close beta.txt")).toHaveCount(1);
  await expect(editor(page)).toHaveText("Original alpha");
});

test("audit: a workspace switch discards stale reads, writes and search results", async ({ desktop }) => {
  const { app, page, a } = desktop;
  await files(page); await file(page, "alpha.txt").click(); await editor(page).fill("Saved in A");
  await hold(app, "PUT"); await editor(page).press("Meta+s"); await held(app);
  await page.getByRole("textbox", { name: "Find files", exact: true }).fill("beta");
  await expect(page.locator(".file-tree")).toContainText("beta.txt");
  await choose(app, desktop.b); await button(page, "Open another project").click();
  await page.getByRole("dialog", { name: "Discard unsaved file edits?" }).getByRole("button", { name: "Discard edits" }).click();
  await expect(page.locator(".titlebar")).toContainText("project-b");
  await expect(page.getByRole("textbox", { name: "Find files", exact: true })).toHaveValue("");
  await expect(page.locator(".file-tree")).not.toContainText("beta.txt");
  await file(page, "alpha.txt").click(); await expect(editor(page)).toHaveText("Other project alpha");
  await release(app);
  await expect.poll(() => readFile(join(a, "alpha.txt"), "utf8")).toBe("Saved in A");
  await expect(editor(page)).toHaveText("Other project alpha");
  await expect(button(page, "Save")).toBeDisabled();
});

test("audit: invalid create and rename retain input, retry successfully and preserve undo", async ({ desktop: { page, a } }) => {
  await files(page); await button(page, "New file").click();
  const form = page.getByRole("dialog", { name: "New file", exact: true });
  const input = form.getByRole("textbox", { name: "Path relative to this folder" });
  await input.fill("../notes.txt"); await form.getByRole("button", { name: "Create", exact: true }).click();
  await expect(form.getByRole("alert")).toContainText("Path must be relative"); await expect(input).toHaveValue("../notes.txt");
  await input.fill("created.txt"); await form.getByRole("button", { name: "Create", exact: true }).click();
  await expect(form).toHaveCount(0); await expect(button(page, "Close created.txt")).toHaveCount(1);
  await editor(page).fill("Unsaved rename draft"); await button(page, "Rename").click();
  const rename = page.getByRole("dialog", { name: "Rename file", exact: true });
  await rename.getByRole("textbox", { name: "New path" }).fill("alpha.txt"); await rename.getByRole("button", { name: "Save", exact: true }).click();
  await expect(rename.getByRole("alert")).toBeVisible(); await expect(rename.getByRole("textbox")).toHaveValue("alpha.txt");
  await rename.getByRole("textbox").fill("renamed.txt"); await rename.getByRole("button", { name: "Save", exact: true }).click();
  await expect(rename).toHaveCount(0); await expect(editor(page)).toHaveText("Unsaved rename draft");
  await editor(page).press("Meta+z"); await expect(editor(page)).toHaveText("");
  expect(await readFile(join(a, "alpha.txt"), "utf8")).toBe("Original alpha");
  expect(await readFile(join(a, "renamed.txt"), "utf8")).toBe("");
});

test("audit: Save remains clickable with many tabs at narrow and normal window sizes", async ({ desktop: { page, a } }) => {
  await files(page);
  for (const name of ["alpha", "beta", "gamma", "delta", "epsilon"]) await file(page, name + ".txt").click();
  for (const width of [1280, 1024, 720]) {
    await page.setViewportSize({ width, height: 800 });
    await editor(page).fill("Saved at " + width);
    const save = button(page, "Save");
    const visible = await save.evaluate((element) => {
      const box = element.getBoundingClientRect();
      return box.left >= 0 && box.right <= innerWidth && element.contains(document.elementFromPoint(box.x + box.width / 2, box.y + box.height / 2));
    });
    expect(visible).toBe(true); await save.click(); await expect(save).toBeDisabled();
    expect(await readFile(join(a, "epsilon.txt"), "utf8")).toBe("Saved at " + width);
  }
});

test("audit: dismissing a pending modal never closes its replacement when the write finishes", async ({ desktop: { app, page, a } }) => {
  const dismiss = async (name: string) => {
    const dialog = page.getByRole("dialog", { name, exact: true });
    const box = await dialog.boundingBox();
    if (!box || box.x < 8) throw Error("Expected an exposed modal backdrop");
    await page.mouse.click(box.x / 2, box.y + box.height / 2);
    await expect(dialog).toHaveCount(0);
  };
  await files(page);
  await button(page, "New file").click();
  const fileForm = page.getByRole("dialog", { name: "New file", exact: true });
  const path = fileForm.getByRole("textbox", { name: "Path relative to this folder" });
  await path.fill("pending.txt");
  await hold(app, "PUT");
  await fileForm.getByRole("button", { name: "Create", exact: true }).click();
  await held(app);
  await dismiss("New file");
  await button(page, "New file").click();
  await path.fill("new-draft.txt");
  await release(app);
  await expect.poll(() => readFile(join(a, "pending.txt"), "utf8").catch(() => null)).toBe("");
  await expect(path).toHaveValue("new-draft.txt");
  await expect(fileForm.getByRole("button", { name: "Create", exact: true })).toBeEnabled();
  await dismiss("New file");

  // Provider forms share a separate dialog component from file prompts.
  await button(page, "Settings").click();
  const settings = page.getByRole("dialog", { name: "Settings", exact: true });
  await settings.getByRole("button", { name: "Providers", exact: true }).click();
  await settings.getByRole("button", { name: "Add custom provider", exact: true }).click();
  const provider = page.getByRole("dialog", { name: "Custom provider", exact: true });
  await provider.getByLabel("Provider ID", { exact: true }).fill("pending-provider");
  await provider.getByLabel("Base URL", { exact: true }).fill("http://127.0.0.1:1/v1");
  await provider.getByRole("combobox", { name: "Authentication", exact: true }).click();
  await page.getByRole("option").filter({ hasText: "No authentication" }).click();
  await provider.getByRole("combobox", { name: "Models", exact: true }).click();
  await page.getByRole("option").filter({ hasText: "Choose model IDs" }).click();
  await provider.getByRole("button", { name: "Add model", exact: true }).click();
  await provider.getByRole("textbox", { name: "Model ID 1", exact: true }).fill("fixture-model");
  await hold(app, "POST", "/v1/providers/refresh");
  await provider.getByRole("button", { name: "Add provider", exact: true }).click();
  await held(app);
  await dismiss("Custom provider");
  await expect(settings).toBeVisible();
  await settings.getByRole("button", { name: "Add custom provider", exact: true }).click();
  await provider.getByLabel("Provider ID", { exact: true }).fill("replacement-draft");
  await release(app);
  await expect(settings.locator('[data-setting="provider:pending-provider"]')).toHaveCount(1);
  await expect(provider.getByLabel("Provider ID", { exact: true })).toHaveValue("replacement-draft");
  await expect(provider.getByRole("button", { name: "Add provider", exact: true })).toBeEnabled();
  await dismiss("Custom provider");
  await dismiss("Settings");
});

test("audit: Settings owns initial focus, traps keyboard, restores trigger and nests dialogs", async ({ desktop: { page } }) => {
  await button(page, "Settings").click();
  const settings = page.getByRole("dialog", { name: "Settings", exact: true });
  expect(await settings.evaluate((element) => element.contains(document.activeElement))).toBe(true);
  for (const key of ["Shift+Tab", ...Array(40).fill("Tab")]) {
    await page.keyboard.press(key);
    expect(await settings.evaluate((element) => element.contains(document.activeElement))).toBe(true);
  }
  await settings.getByRole("button", { name: "Schedules", exact: true }).click();
  await settings.getByRole("button", { name: "Create schedule", exact: true }).click();
  const nested = page.getByRole("dialog", { name: "Create schedule", exact: true });
  await expect(nested).toBeVisible(); await page.keyboard.press("Escape"); await expect(nested).toHaveCount(0); await expect(settings).toBeVisible();
  await page.keyboard.press("Escape"); await expect(settings).toHaveCount(0); await expect(button(page, "Settings")).toBeFocused();
});

test("audit: Tab moves focus everywhere, agent keys stay in the message box and dialogs start on their field", async ({ desktop: { page } }) => {
  const agent = page.getByRole("combobox", { name: "Agent", exact: true });
  const message = page.getByRole("textbox", { name: "Message", exact: true });
  // The focused control's name, and whether focus moved since the last call.
  const focused = () => page.evaluate(() => {
    const state = window as unknown as { lastFocus?: Element | null };
    const element = document.activeElement, moved = element !== state.lastFocus;
    state.lastFocus = element;
    return { moved, name: element?.getAttribute("aria-label") ?? element?.textContent?.trim() };
  });
  await message.fill(' !tool bash {"command":"echo keyboard"}');
  await message.press("Enter");
  await expect(button(page, "Allow once")).toBeVisible();
  // From the sidebar, Tab walks every control down to the approval dock and
  // never touches the chat's agent.
  await button(page, "Search").focus();
  await focused();
  const visited: { moved: boolean; name?: string | null }[] = [];
  for (let i = 0; i < 40 && visited.at(-1)?.name !== "Allow once"; i++) {
    await page.keyboard.press("Tab");
    visited.push(await focused());
  }
  expect(visited.at(-1)?.name).toBe("Allow once");
  expect(visited.filter((step) => !step.moved)).toEqual([]);
  await expect(agent).toHaveAttribute("data-value", "build");
  await page.keyboard.press("Shift+Tab");
  await expect(button(page, "Allow once")).not.toBeFocused();
  await button(page, "Allow once").click();
  await expect(page.locator(".transcript")).toContainText("keyboard");
  // Tab leaves the message box; the agent chords switch and are announced.
  await message.focus();
  await page.keyboard.press("Tab");
  await expect(message).not.toBeFocused();
  await expect(agent).toHaveAttribute("data-value", "build");
  await message.focus();
  await page.keyboard.press("ControlOrMeta+Period");
  await expect(agent).toHaveAttribute("data-value", "plan");
  await expect(page.locator(".composer [role=status]").filter({ hasText: "Agent: plan" })).toHaveCount(1);
  await expect(message).toBeFocused();
  await page.keyboard.press("ControlOrMeta+Shift+Period");
  await expect(agent).toHaveAttribute("data-value", "build");
  await button(page, "Search").focus();
  await page.keyboard.press("ControlOrMeta+Period");
  await expect(agent).toHaveAttribute("data-value", "build");
  // Rename opens on its field with the title selected and refuses a blank one.
  await button(page, "Session actions").click();
  await page.getByRole("menuitem", { name: "Rename", exact: true }).click();
  const rename = page.getByRole("dialog", { name: "Rename session", exact: true });
  const title = rename.getByRole("textbox", { name: "Title", exact: true });
  await expect(title).toBeFocused();
  expect(await title.evaluate((input: HTMLInputElement) => input.selectionEnd! - input.selectionStart!)).toBe((await title.inputValue()).length);
  await page.keyboard.type("   ");
  await page.keyboard.press("Enter");
  await expect(rename.getByRole("alert")).toHaveText("Enter a title.");
  await expect(title).toBeFocused();
  await title.fill("  Keyboard proof  ");
  await page.keyboard.press("Enter");
  await expect(rename).toHaveCount(0);
  await expect(page.locator(".sidebar").getByRole("button", { name: "Keyboard proof", exact: true })).toBeVisible();
});

test("audit: unsent image drafts survive reload and project switching and removal stays removed", async ({ desktop }) => {
  const { app, page, a } = desktop;
  await page.getByRole("textbox", { name: "Message", exact: true }).fill("Retained draft");
  await choose(app, join(a, "proof.png")); await button(page, "Attach files").click();
  await expect(button(page, "Remove proof.png")).toBeVisible();
  await page.reload(); await expect(button(page, "Remove proof.png")).toBeVisible();
  await expect(page.getByRole("textbox", { name: "Message", exact: true })).toHaveValue("Retained draft");
  await switchProject(desktop); await expect(button(page, "Remove proof.png")).toHaveCount(0);
  await choose(app, a); await button(page, "Open another project").click(); await expect(button(page, "Remove proof.png")).toBeVisible();
  await button(page, "Remove proof.png").click(); await page.reload(); await expect(button(page, "Remove proof.png")).toHaveCount(0);
});

test("audit: Japanese scheduling translates controls, validation, nested pickers and saved descriptions", async ({ desktop: { page } }) => {
  await button(page, "Settings").click();
  const language = page.getByRole("combobox", { name: "Language", exact: true });
  await language.click(); await page.getByRole("option").filter({ hasText: "日本語" }).click();
  await button(page, "スケジュール").click(); await button(page, "スケジュールを作成").click();
  const form = page.getByRole("dialog", { name: "スケジュールを作成", exact: true });
  for (const label of ["頻度", "毎日", "時刻", "スケジュールを有効にする", "保存"]) await expect(form).toContainText(label);
  await expect(form).not.toContainText("Runs even while");
  await form.getByRole("textbox", { name: "名前", exact: true }).fill("日本語のテスト");
  await form.getByRole("textbox", { name: "プロンプト", exact: true }).fill("Return schedule proof");
  const frequency = form.getByRole("combobox", { name: "頻度", exact: true });
  await frequency.click(); await page.locator('[role="option"][data-value="interval"]').click();
  await form.getByRole("combobox", { name: "間隔の単位", exact: true }).click();
  await page.locator('[role="option"][data-value="1"]').click();
  await form.getByRole("button", { name: "保存", exact: true }).click();
  await expect(form.getByRole("alert")).toHaveText("1分以上の間隔を選択してください。");
  await frequency.click(); await page.locator('[role="option"][data-value="daily"]').click();
  await form.getByRole("button", { name: "保存", exact: true }).click();
  await expect(form).toHaveCount(0); await expect(page.locator(".schedule-card")).toContainText("毎日");
  await expect(page.locator(".schedule-card")).toContainText("無効");
});

test("audit: editor selection and undo survive media previews; closed files release history", async ({ desktop: { page } }) => {
  await files(page); await file(page, "alpha.txt").click(); await editor(page).fill("Edited alpha");
  await editor(page).press("Home"); await editor(page).press("Shift+ArrowRight");
  await file(page, "proof.png").click(); await expect(page.locator(".media-preview img")).toBeVisible();
  await file(page, "alpha.txt").click(); await expect(button(page, "Add selection to context")).toBeVisible();
  await editor(page).press("Meta+z"); await expect(editor(page)).toHaveText("Original alpha");
  await button(page, "Close alpha.txt").click(); await file(page, "alpha.txt").click();
  await editor(page).press("Meta+Shift+z"); await expect(editor(page)).toHaveText("Original alpha");
});

test("audit: reads and searches started in a previous workspace cannot publish after switching back", async ({ desktop }) => {
  const { app, page, a } = desktop;
  await files(page); await hold(app, "GET"); await file(page, "slow.txt").click(); await held(app);
  await switchProject(desktop); await file(page, "second.txt").click();
  // The second project's read is held too; switch back before either returns.
  await held(app, 2); await choose(app, a); await button(page, "Open another project").click();
  await expect(page.locator(".titlebar")).toContainText("project-a");
  await release(app); await file(page, "fast.txt").click();
  await expect(editor(page)).toHaveText("Original fast"); await expect(button(page, "Close slow.txt")).toHaveCount(0);
  await expect(button(page, "Close second.txt")).toHaveCount(0);
  await hold(app, "GET", "/v1/files/search");
  await page.getByRole("textbox", { name: "Find files", exact: true }).fill("beta"); await held(app);
  await switchProject(desktop); await release(app);
  await expect(page.locator(".file-tree")).toContainText("second.txt");
  await expect(page.locator(".file-tree")).not.toContainText("beta.txt");
});

test("audit: attachment drafts larger than preferences storage survive an immediate reload", async ({ desktop: { app, page, a } }) => {
  const path = join(a, "large.png");
  await writeFile(path, Buffer.alloc(3 * 1024 * 1024, 7));
  await choose(app, path); await button(page, "Attach files").click();
  await expect(button(page, "Remove large.png")).toBeVisible(); await page.reload();
  await expect(button(page, "Remove large.png")).toBeVisible();
  const value = await page.evaluate(async () => {
    const data = await window.haarness.boot();
    const key = "new:" + (data.recent[0].project?.id ?? data.recent[0].id);
    return (await window.haarness.attachmentDraft(key)).map((item) => ({ name: item.name, bytes: item.data.split(",")[1].length * 0.75 }));
  });
  expect(value).toEqual([{ name: "large.png", bytes: 3 * 1024 * 1024 }]);
});

test("audit: rejected Settings changes remain visible and dismissible inside the modal", async ({ desktop: { page } }) => {
  await button(page, "Settings").click();
  const settings = page.getByRole("dialog", { name: "Settings", exact: true });
  await settings.getByRole("button", { name: "Keyboard shortcuts", exact: true }).click();
  const shortcut = settings.getByRole("textbox", { name: "Shortcut new", exact: true });
  const original = await shortcut.inputValue();
  await shortcut.fill("invalid shortcut"); await shortcut.press("Tab");
  await expect(settings.getByRole("alert")).toContainText("Use unique shortcuts");
  await settings.getByRole("button", { name: "Dismiss error", exact: true }).click();
  await expect(settings.getByRole("alert")).toHaveCount(0);
  await shortcut.fill(original); await shortcut.press("Tab");
});

test("audit: overlapping canvas previews keep the latest mode and cannot land in another chat", async ({ desktop: { app, page, a } }) => {
  await writeFile(join(a, 'race.html'), '<h1>First page</h1>');
  const send = async (mode: string) => {
    await page.getByRole('textbox', { name: 'Message', exact: true }).fill(`!tool preview {"filePath":"race.html","mode":"${mode}"}`);
    await page.getByRole('button', { name: 'Send message', exact: true }).click();
  };
  await hold(app, 'GET', '/v1/files/content');
  await send('preview');
  await expect(page.locator('.preview-banner')).toHaveCount(1);
  await held(app, 1);
  await send('code');
  await expect(page.locator('.preview-banner')).toHaveCount(2);
  await held(app, 2);
  await release(app);
  await expect(editor(page)).toContainText('First page');
  await writeFile(join(a, 'race.html'), '<h1>Late page</h1>');
  await hold(app, 'GET', '/v1/files/content');
  await send('preview');
  await expect(page.locator('.preview-banner')).toHaveCount(3);
  await held(app);
  await button(page, 'New session').click();
  await expect(page.locator('.preview-banner')).toHaveCount(0);
  await release(app);
  await expect(editor(page)).toContainText('First page');
  await expect(page.locator('.file-preview .html-preview')).toHaveCount(0);
});
