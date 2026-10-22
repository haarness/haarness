import { test, expect, _electron as electron, type ElectronApplication, type Locator, type Page } from "@playwright/test";
import { mkdtemp, mkdir, writeFile, realpath } from "node:fs/promises";
import { execFileSync } from "node:child_process";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

// The hook editor's matcher, and a question a PreToolUse hook answers, in a
// real Desktop against a real server with the echo provider.
let app: ElectronApplication, page: Page, root: string, workspace: string;
let env: Record<string, string>;
const binary = resolve(process.env.HAARNESS_BIN ?? "../../target/debug/haarness");
const errors: string[] = [];
test.describe.configure({ mode: "serial" });

async function selectValue(control: Locator, value: string) {
  await control.click();
  const id = await control.getAttribute("aria-controls");
  await control.page().locator(`[id=${JSON.stringify(id)}] [role="option"][data-value=${JSON.stringify(value)}]`).click();
}
async function api(method: string, ...args: unknown[]): Promise<any> {
  return page.evaluate(({ connection, method, args }) => window.haarness.api(connection, method, args), {
    connection: "local:" + workspace,
    method,
    args,
  });
}
async function theme(value: "light" | "dark") {
  await page.evaluate((theme) => window.haarness.preferences({ theme }), value);
  await expect(page.locator("html")).toHaveAttribute("data-theme", value);
}
async function shot(name: string) {
  await page.screenshot({ path: test.info().outputPath(name + ".png") });
}
async function openHooks() {
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  const settings = page.getByRole("dialog", { name: "Settings", exact: true });
  await settings.getByRole("button", { name: "Hooks", exact: true }).click();
  return settings;
}

test.beforeAll(async () => {
  test.setTimeout(120000);
  root = await realpath(await mkdtemp(join(tmpdir(), "haarness-hook-answers-ui-")));
  workspace = join(root, "project");
  await mkdir(join(workspace, ".haarness"), { recursive: true });
  await writeFile(join(workspace, ".haarness/config.toml"), '[defaults]\nprovider="echo"\nmodel="echo-1"\n');
  const hook = join(root, "answer.py");
  await writeFile(
    hook,
    [
      "import json, sys",
      "data = json.load(sys.stdin)",
      "if data['tool_input'].get('prompt') == 'Which test suite should I run?':",
      "    print(json.dumps({'hookSpecificOutput': {'hookEventName': 'PreToolUse',",
      "        'updatedInput': dict(data['tool_input'], answers=[['Unit tests']])}}))",
    ].join("\n"),
  );
  const command = (matcher: string) => [{ matcher, hooks: [{ type: "command", command: `python3 ${JSON.stringify(hook)}` }] }];
  await writeFile(
    join(workspace, "haarness.json"),
    JSON.stringify({ hooks: { PreToolUse: command("question"), SessionEnd: command("clear") } }, null, 1),
  );
  env = Object.fromEntries(
    Object.entries(process.env).filter(([key, value]) => value !== undefined && !key.startsWith("HAARNESS_")),
  ) as Record<string, string>;
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
  }, workspace);
  await page.getByRole("region", { name: "Project home" }).getByRole("button", { name: "Open project", exact: true }).click();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 60000 });
  await theme("light");
});
test.afterAll(async () => {
  if (app) {
    await app.evaluate(({ BrowserWindow }) => {
      for (const win of BrowserWindow.getAllWindows()) win.destroy();
    }).catch(() => {});
    await app.close().catch(() => {});
  }
  try {
    execFileSync(binary, ["--workspace", workspace, "serve", "--stop"], { env, stdio: "ignore", timeout: 30000 });
  } catch {}
  await test.info().attach("fixture", { body: root ?? "not started", contentType: "text/plain" });
});

test("the hook editor's matcher suggests what its event can match and says when a hook never runs", async () => {
  const settings = await openHooks();
  // A configured hook whose matcher its event never sends carries a quiet tag.
  const never = settings.locator(".management-card").filter({ hasText: "SessionEnd" });
  await expect(never.locator(".hook-never-tag")).toHaveText("Never runs");
  await expect(never.locator(".hook-never-tag")).toHaveAttribute("title", "Never runs: this event only matches archived.");
  await expect(settings.locator(".management-card").filter({ hasText: "PreToolUse" }).locator(".hook-never-tag")).toHaveCount(0);
  await shot("hooks-list-never-runs-light");

  await settings.getByRole("button", { name: "Add hook", exact: true }).click();
  const form = page.getByRole("dialog", { name: "Add hook", exact: true });
  const event = form.getByRole("combobox", { name: "Hook event", exact: true });
  const matcher = form.getByRole("combobox", { name: "Hook matcher", exact: true });
  // Tool events name their field for what it matches and suggest the
  // server's tools; nothing opens until it is asked for.
  await expect(form.locator(".field > span").filter({ hasText: /^Tools$/ })).toBeVisible();
  await expect(matcher).toHaveAttribute("aria-expanded", "false");
  await matcher.fill("rea");
  await expect(page.getByRole("option", { name: /^read\b/ })).toBeVisible();
  await shot("hook-matcher-tools-light");
  await matcher.press("ArrowDown");
  await matcher.press("Enter");
  await expect(matcher).toHaveValue("read");
  await expect(form).toBeVisible();
  // A suggestion completes the alternative being typed after `|`.
  await matcher.pressSequentially("|ed");
  await page.getByRole("option", { name: /^edit\b/ }).click();
  await expect(matcher).toHaveValue("read|edit");
  // Another event: what was typed stays, and the hint says it would never
  // run; the field suggests the only values that event sends, each explained.
  await selectValue(event, "PostCompact");
  await expect(matcher).toHaveValue("read|edit");
  await expect(form.getByRole("status")).toHaveText("Never runs: this event only matches manual or automatic.");
  await expect(form.locator(".field > span").filter({ hasText: /^Matcher$/ })).toBeVisible();
  await matcher.fill("");
  await matcher.press("ArrowDown");
  await expect(page.getByRole("option", { name: "manual Compacted with /compact" })).toBeVisible();
  await expect(page.getByRole("option", { name: "automatic Compacted near the context limit" })).toBeVisible();
  await shot("hook-matcher-values-light");
  // Esc closes the suggestions, not the dialog.
  await matcher.press("Escape");
  await expect(page.getByRole("option", { name: /^manual/ })).toHaveCount(0);
  await expect(form).toBeVisible();
  await matcher.fill("auto");
  // While typing, the suggestion that fits shows over the hint; closing the
  // suggestions shows why the matcher never runs.
  await expect(page.getByRole("option", { name: /^automatic/ })).toBeVisible();
  await matcher.press("Escape");
  await expect(form.getByRole("status")).toBeVisible();
  await expect(form.getByRole("status")).toHaveText("Never runs: this event only matches manual or automatic.");
  await shot("hook-matcher-never-runs-light");
  await matcher.fill("automatic");
  await expect(form.getByRole("status")).toHaveCount(0);
  await expect(form).toContainText("Pick from the list, or type a pattern.");
  await selectValue(event, "StopFailure");
  await matcher.fill("");
  await matcher.press("ArrowDown");
  await expect(page.getByRole("option", { name: "rate_limit Rate limit or quota reached" })).toBeVisible();
  await expect(page.getByRole("option", { name: "provider Any provider error" })).toBeVisible();
  await matcher.press("Escape");

  await theme("dark");
  await selectValue(event, "PostCompact");
  await matcher.fill("auto");
  await matcher.press("Escape");
  await expect(form.getByRole("status")).toBeVisible();
  await shot("hook-matcher-never-runs-dark");
  await matcher.fill("");
  await matcher.press("ArrowDown");
  await expect(page.getByRole("option", { name: /^manual/ })).toBeVisible();
  await shot("hook-matcher-values-dark");
  await matcher.press("Escape");
  await selectValue(event, "PreToolUse");
  await matcher.fill("rea");
  await expect(page.getByRole("option", { name: /^read\b/ })).toBeVisible();
  await shot("hook-matcher-tools-dark");
  await matcher.press("Escape");
  // An event with nothing to match keeps what was typed and says it is
  // ignored; with `*` it asks for no matcher at all.
  await selectValue(event, "Stop");
  await expect(matcher).toHaveValue("rea");
  await expect(form).toContainText("This event has nothing to match, so the matcher is ignored.");
  // Nothing to suggest, so no caret.
  await expect(form.locator(".combobox-toggle")).toBeDisabled();
  await expect(form.locator(".combobox-toggle")).toHaveCSS("opacity", "0");
  await shot("hook-matcher-ignored-dark");
  await matcher.fill("*");
  await expect(matcher).toHaveCount(0);
  await form.getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(form).not.toBeVisible();
  await shot("hooks-list-never-runs-dark");
  await settings.getByRole("button", { name: "Close settings" }).click();
  await theme("light");
  expect(errors).toEqual([]);
});

test("a question a hook answered shows its answer and says the hook answered it", async () => {
  await page.getByRole("button", { name: "New chat", exact: true }).click();
  const box = page.getByRole("textbox", { name: "Message", exact: true });
  await box.fill(" !tool question " + JSON.stringify({ prompt: "Which test suite should I run?", choices: ["Unit tests", "Full suite"] }));
  await page.getByRole("button", { name: /Send message|Send guidance/ }).click();
  const row = page.locator(".tool-call").filter({ hasText: "Which test suite should I run?" });
  await expect(row.locator(".tool-chip")).toHaveText("Answered by hook");
  await expect.poll(async () => (await api("activeTurns")).length, { timeout: 30000 }).toBe(0);
  // Nobody was asked.
  await expect(page.getByRole("region", { name: "A question for you" })).toHaveCount(0);
  if (!(await row.getAttribute("open"))) await row.locator("summary").click();
  await expect(row.locator(".tool-output")).toContainText("Unit tests");
  await shot("question-answered-by-hook-light");
  await theme("dark");
  await shot("question-answered-by-hook-dark");
  await theme("light");
  // A question the hook has no answer for is still the person's.
  await box.fill(" !tool question " + JSON.stringify({ prompt: "Which color?", choices: ["Blue", "Green"] }));
  await page.getByRole("button", { name: /Send message|Send guidance/ }).click();
  const card = page.getByRole("region", { name: "A question for you" });
  await expect(card).toBeVisible();
  await card.getByRole("button", { name: "Blue", exact: true }).press("Enter");
  await expect.poll(async () => (await api("activeTurns")).length, { timeout: 30000 }).toBe(0);
  await expect(page.locator(".tool-call").filter({ hasText: "Which color?" }).locator(".tool-chip")).toHaveCount(0);
  expect(errors).toEqual([]);
});
