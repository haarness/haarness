import { test, expect, _electron as electron, type Page } from "@playwright/test";
import { execFileSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { mkdtemp, realpath } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

// Opt-in: one everyday coding workflow, end to end, with real models through the Desktop UI.
// HAARNESS_WORKFLOW_E2E=1 HAARNESS_WORKFLOW_PROJECT=<a git repo with one failing `python3 -m unittest` test>
// and a profile (HAARNESS_DATA_DIR) whose providers can run HAARNESS_WORKFLOW_MODELS (two models of different
// providers, comma-separated) and whose config puts chats in Review and asks before `git commit`:
//   [defaults] approval_mode = "review"     [permissions.bash] "git commit*" = "ask"
// The spec fixes the test, adds a feature, commits after a permission answer, undoes a change, switches model,
// runs a background command and fetches a docs page. It stops nothing it did not start.
const live = process.env.HAARNESS_WORKFLOW_E2E === "1" && !!process.env.HAARNESS_WORKFLOW_PROJECT;
const project = process.env.HAARNESS_WORKFLOW_PROJECT ?? "";
const [first, second] = (process.env.HAARNESS_WORKFLOW_MODELS ?? "openai/gpt-6-luna,anthropic/claude-haiku-5-5").split(",");
const git = (...args: string[]) => execFileSync("git", args, { cwd: project, encoding: "utf8" });
const tests = () => {
  try {
    execFileSync("python3", ["-m", "unittest"], { cwd: project, stdio: "pipe" });
    return true;
  } catch {
    return false;
  }
};
const listening = (port: number) => {
  try {
    execFileSync("nc", ["-z", "127.0.0.1", String(port)], { stdio: "ignore" });
    return true;
  } catch {
    return false;
  }
};

async function pick(page: Page, value: string) {
  const model = page.getByRole("combobox", { name: "Model", exact: true });
  await model.click();
  const list = page.locator(`[id=${JSON.stringify(await model.getAttribute("aria-controls"))}]`);
  // The list is virtual: search narrows it to the option before it can be clicked.
  await page.locator("input.select-search").fill(value.split("/").at(-1)!);
  await list.locator(`[role="option"][data-value=${JSON.stringify(value)}]`).click();
  const done = page.locator(".select-footer").getByRole("button", { name: "Done", exact: true });
  if (await done.isVisible()) await done.click();
  await expect(model).toContainText(/./);
}

async function ask(page: Page, text: string) {
  const box = page.getByRole("textbox", { name: "Message", exact: true });
  await box.fill(text);
  await page.getByRole("button", { name: /Send message/ }).click();
}

async function settled(page: Page, timeout = 300000) {
  await expect(page.getByRole("button", { name: "Stop", exact: true })).toBeVisible({ timeout: 30000 }).catch(() => {});
  await expect(page.getByRole("button", { name: "Stop", exact: true })).toHaveCount(0, { timeout });
}

const lastAnswer = (page: Page) => page.locator(".message.assistant").last();

test("a coding workflow runs end to end on real models", async ({}, info) => {
  test.skip(!live, "set HAARNESS_WORKFLOW_E2E=1 and HAARNESS_WORKFLOW_PROJECT");
  test.setTimeout(1_800_000);
  const root = await realpath(await mkdtemp(join(tmpdir(), "haarness-workflow-live-")));
  const env: Record<string, string> = {
    ...(process.env as Record<string, string>),
    HAARNESS_BIN: resolve(process.env.HAARNESS_BIN ?? "../../target/debug/haarness"),
    HAARNESS_DESKTOP_DATA: join(root, "desktop"),
    HAARNESS_DESKTOP_TEST: "1",
  };
  delete env.ELECTRON_RUN_AS_NODE;
  delete env.ELECTRON_RENDERER_URL;
  const app = await electron.launch({ args: [process.env.HAARNESS_DESKTOP_APP ?? "."], env, timeout: 60000 });
  const page = await app.firstWindow();
  const errors: string[] = [];
  page.on("pageerror", error => errors.push(error.message));
  const shot = (name: string) => page.screenshot({ animations: "disabled", path: info.outputPath(`${name}.png`) });
  try {
    await page.setViewportSize({ width: 1400, height: 950 });
    await app.evaluate(({ dialog }, directory) => {
      dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [directory] });
    }, project);
    await page.getByRole("button", { name: "Open another project", exact: true }).click();
    await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 60000 });
    await pick(page, first);

    // 1. Find and fix the failing test.
    await ask(page, "One test in this project fails. Find out why, fix the code, run the tests, and show me the diff.");
    await settled(page);
    await shot("01-fix");
    expect(tests(), "the tests pass after the first turn").toBe(true);
    await expect(lastAnswer(page)).toContainText(/mid/);

    // 2. A small feature with a test.
    await ask(page, "Now add a function spread(values) to numkit.py that returns the largest value minus the smallest and raises ValueError on an empty list. Add tests for it, mention it in the README, and run the tests.");
    await settled(page);
    await shot("02-feature");
    expect(readFileSync(join(project, "numkit.py"), "utf8")).toContain("def spread");
    expect(tests()).toBe(true);

    // 3. Commit, answering the permission card in Review.
    await ask(page, "Commit all of this with the message: Fix median for even counts and add spread");
    const once = page.getByRole("button", { name: "Allow once", exact: true });
    await expect(once).toBeVisible({ timeout: 180000 });
    await shot("03-permission");
    await once.click();
    await settled(page);
    expect(git("log", "-1", "--format=%s").trim()).toBe("Fix median for even counts and add spread");
    expect(git("status", "--short").trim()).toBe("");

    // 4. Undo one change.
    await ask(page, "Append the line 'Works with Python 3.9 and newer.' to the end of README.md. Don't run anything else.");
    await settled(page);
    expect(readFileSync(join(project, "README.md"), "utf8")).toContain("Works with Python 3.9");
    await ask(page, "/undo");
    await expect.poll(() => readFileSync(join(project, "README.md"), "utf8").includes("Works with Python 3.9"), { timeout: 30000 }).toBe(false);
    await shot("04-undo");
    expect(git("status", "--short").trim()).toBe("");

    // 5. Another provider's model continues the chat.
    await pick(page, second);
    await ask(page, "Run the tests with python3 -m unittest -v and tell me in one sentence how many pass.");
    await settled(page);
    await shot("05-switched");
    await expect(lastAnswer(page)).toContainText(/\b6\b|\bsix\b/i);

    // 6. A background command, checked and stopped.
    await ask(page, "Start 'python3 -m http.server 8768' as a background command (run_in_background), then run 'curl -s -o /dev/null -w %{http_code} http://127.0.0.1:8768/' to check it answers, then stop the background command and tell me the status code.");
    await settled(page);
    await shot("06-background");
    await expect(lastAnswer(page)).toContainText("200");
    await expect.poll(() => listening(8768), { timeout: 30000 }).toBe(false);

    // 7. A docs page through webfetch.
    await ask(page, "Use webfetch to read https://docs.python.org/3/library/statistics.html and tell me in one sentence what statistics.median returns for an even number of data points.");
    await settled(page);
    await shot("07-webfetch");
    await expect(lastAnswer(page)).toContainText(/two middle|middle two|mean|average/i);
    expect(errors).toEqual([]);
  } finally {
    await info.attach("transcript", { body: await page.locator("body").innerText().catch(() => ""), contentType: "text/plain" });
    await app.close();
    if (existsSync(root)) info.annotations.push({ type: "evidence", description: root });
  }
});
