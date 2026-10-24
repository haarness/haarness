import {
  test,
  expect,
  _electron as electron,
  type ElectronApplication,
  type Page,
  type Locator,
} from "@playwright/test";
import {
  mkdtemp,
  mkdir,
  writeFile,
  readFile,
  rm,
  realpath,
  rename,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";
import { Haarness } from "@haarness/sdk";
import { stopChatHome } from "./fixtures/chat-home";

const binary = resolve(
  process.env.HAARNESS_BIN ?? "../../target/debug/haarness",
);
test.describe.configure({ mode: "serial" });
const baseline = process.env.REVIEW_BASELINE === "1";
let app: ElectronApplication;
let page: Page;
let temporary: string;
let workspace: string;
let env: Record<string, string>;
let client: Haarness;
const errors: string[] = [];
const extra: string[] = [];
// Include the four special files within the server's 1,000-file summary cap.
const count = Number(process.env.REVIEW_FILES ?? 996);
const big = "000-large.txt";
const fileName = (i: number) => `src/file-${String(i).padStart(5, "0")}.txt`;
const content = (version: string, size: number) =>
  Array.from({ length: size }, (_, i) => `${version} line ${i + 1}\n`).join("");

async function select(control: Locator, value: string) {
  await control.click();
  const id = await control.getAttribute("aria-controls");
  await page
    .locator(
      `[id=${JSON.stringify(id)}] [role="option"][data-value=${JSON.stringify(value)}]`,
    )
    .click();
  const done = page
    .locator(".select-footer")
    .getByRole("button", { name: "Done", exact: true });
  if (await done.isVisible()) await done.click();
}

test.beforeAll(async () => {
  test.setTimeout(300000);
  temporary = await realpath(
    await mkdtemp(join(tmpdir(), "haarness-review-scaling-")),
  );
  workspace = join(temporary, "workspace");
  await mkdir(join(workspace, "src"), { recursive: true });
  await mkdir(join(workspace, ".haarness"));
  await mkdir(join(temporary, "desktop"));
  await writeFile(
    join(workspace, ".haarness/config.toml"),
    '[defaults]\nprovider="echo"\nmodel="echo-1"\n',
  );
  await writeFile(join(workspace, big), content("before", 10000));
  // Real Git objects and changes, never intercepted API responses.
  for (let i = 0; i < count; i++)
    await writeFile(join(workspace, fileName(i)), content("before", 50));
  await writeFile(join(workspace, "deleted.txt"), "removed\n");
  await writeFile(join(workspace, "binary.bin"), Buffer.from([0, 1, 2]));
  for (const args of [
    ["init", "-b", "main"],
    ["add", "."],
    [
      "-c",
      "user.name=Review Fixture",
      "-c",
      "user.email=review@example.test",
      "commit",
      "-m",
      "Initial review fixture",
    ],
    ["checkout", "-b", "review-fixture"],
  ])
    execFileSync("git", args, { cwd: workspace, stdio: "ignore" });
  await writeFile(join(workspace, big), content("after", 10000));
  for (let i = 0; i < count; i++)
    await writeFile(join(workspace, fileName(i)), content("after", 50));
  await rm(join(workspace, "deleted.txt"));
  await writeFile(join(workspace, "binary.bin"), Buffer.from([0, 3, 4]));
  await writeFile(
    join(workspace, "untracked [literal].txt"),
    "new file\nsecond line\n",
  );
  env = Object.fromEntries(
    Object.entries(process.env).filter(
      (entry): entry is [string, string] => entry[1] !== undefined,
    ),
  );
  Object.assign(env, {
    HAARNESS_BIN: binary,
    HAARNESS_HOME: join(temporary, "home"),
    HAARNESS_DATA_DIR: join(temporary, "state"),
    HAARNESS_DESKTOP_DATA: join(temporary, "desktop"),
    HAARNESS_TEST_PROVIDER: "1",
    HAARNESS_DESKTOP_TEST: "1",
  });
  delete env.ELECTRON_RUN_AS_NODE;
  app = await electron.launch({ args: ["."], env, timeout: 240000 });
  page = await app.firstWindow();
  page.on("pageerror", (error) => errors.push(error.message));
  await page.setViewportSize({ width: 1500, height: 950 });
  await app.evaluate(({ dialog }, directory) => {
    dialog.showOpenDialog = async () => ({
      canceled: false,
      filePaths: [directory],
    });
  }, workspace);
  await page
    .getByRole("button", { name: "Open another project", exact: true })
    .click();
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
    { timeout: 240000 },
  );
  const connection = JSON.parse(
    execFileSync(
      binary,
      ["--workspace", workspace, "--trust-project", "desktop"],
      { env, input: '{"action":"connect"}', encoding: "utf8", timeout: 240000 },
    ),
  );
  client = new Haarness({ baseUrl: connection.url, token: connection.token });
});

test.afterAll(async () => {
  client?.close();
  await app?.close().catch(() => {});
  if (workspace)
    execFileSync(binary, ["--workspace", workspace, "serve", "--stop"], {
      env,
      stdio: "ignore",
      timeout: 30000,
    });
  for (const directory of extra)
    execFileSync(binary, ["--workspace", directory, "serve", "--stop"], {
      env,
      stdio: "ignore",
      timeout: 30000,
    });
  stopChatHome(binary, env);
  if (temporary) await rm(temporary, { recursive: true, force: true });
});

test("large review scaling on a real repository", async () => {
  test.setTimeout(300000);
  await page.evaluate(() => {
    const metrics = {
      frames: [] as number[],
      tasks: [] as number[],
      last: performance.now(),
      running: true,
    };
    (window as any).reviewMetrics = metrics;
    const tick = (now: number) => {
      metrics.frames.push(now - metrics.last);
      metrics.last = now;
      if (metrics.running) requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
    new PerformanceObserver((list) =>
      metrics.tasks.push(...list.getEntries().map((e) => e.duration)),
    ).observe({ type: "longtask", buffered: false });
  });
  const started = Date.now();
  const show = page.getByRole("button", {
    name: "Show side panel",
    exact: true,
  });
  if (await show.isVisible()) await show.click();
  await page.getByRole("button", { name: "Open panel", exact: true }).click();
  await page
    .getByRole("region", { name: "Open a project panel" })
    .getByRole("button", { name: "Open Review", exact: true })
    .click();
  await expect(page.locator(".diff-file").first()).toContainText(
    "after line 1",
    { timeout: 120000 },
  );
  const readyMs = Date.now() - started;
  const metrics = await page.evaluate(() => {
    const m = (window as any).reviewMetrics;
    m.running = false;
    return {
      maxFrameMs: Math.max(...m.frames),
      longTasks: m.tasks,
      fileNodes: document.querySelectorAll(".review .file-entry").length,
      lineNodes: document.querySelectorAll(
        ".review .diff-line, .review .split-row",
      ).length,
    };
  });
  const result = { count, readyMs, ...metrics };
  console.log("REVIEW_SCALING", JSON.stringify(result));
  await writeFile(
    test.info().outputPath("review-scaling.json"),
    JSON.stringify(result, null, 2),
  );
  await test.info().attach("review-scaling.json", {
    body: JSON.stringify(result, null, 2),
    contentType: "application/json",
  });
  await page.screenshot({ path: test.info().outputPath("large-review.png") });
  if (baseline) return;
  expect(metrics.fileNodes).toBeLessThan(100);
  expect(metrics.lineNodes).toBeLessThan(150);
  expect(readyMs).toBeLessThan(15000);
  const summary = await client.vcsDiff({ summary: true, context: 4 });
  // The server deliberately caps review summaries. Account for its explicit
  // omission metadata instead of requiring more rows than the server allows.
  const omitted = (summary[0] as (typeof summary)[number] & { omittedFiles?: number })?.omittedFiles ?? 0;
  expect(summary.length + omitted).toBe(count + 4);
  expect(summary).toHaveLength(Math.min(count + 4, 1000));
  expect(
    summary.every((file) => !file.patch && !file.before && !file.after),
  ).toBe(true);
  const detail = await client.vcsDiff({
    path: "untracked [literal].txt",
    context: 4,
  });
  expect(detail).toHaveLength(1);
  expect(detail[0].patch).toContain("+second line");
  expect(errors).toEqual([]);
});

test("large review navigation, comments, layouts and refresh remain usable", async () => {
  test.skip(baseline);
  test.setTimeout(180000);
  const pane = page.locator(".diff-content");
  await pane.evaluate((element) => {
    element.scrollTop = element.scrollHeight;
  });
  await expect(page.locator(".diff-file")).toContainText("after line 10000");
  expect(await page.locator(".split-row").count()).toBeLessThan(150);
  await pane.evaluate((element) => {
    element.scrollTop = 0;
  });
  await expect(
    page.getByRole("button", {
      name: "Comment on updated line 1",
      exact: true,
    }),
  ).toBeVisible();
  await page
    .getByRole("button", { name: "Comment on updated line 1", exact: true })
    .click();
  const comment = page.getByRole("textbox", {
    name: "Review comment",
    exact: true,
  });
  await comment.fill("Preserve this review draft.");
  await select(
    page.getByRole("combobox", { name: "Diff layout", exact: true }),
    "unified",
  );
  await expect(comment).toHaveValue("Preserve this review draft.");
  await page
    .getByRole("button", { name: "Refresh changes", exact: true })
    .click();
  await expect(comment).toHaveValue("Preserve this review draft.");
  await comment.press("Enter");
  await expect(
    page.getByRole("textbox", { name: "Message", exact: true }),
  ).toHaveValue(/000-large.txt:1 \(updated\).*\nPreserve this review draft\./);
  await page
    .getByRole("button", { name: "Toggle changed files", exact: true })
    .click();
  const filter = page.getByRole("searchbox", { name: "Filter changed files" });
  await expect(filter).toBeVisible();
  await filter.fill(fileName(count - 1));
  await expect(page.locator(".file-entry")).toHaveCount(1);
  await select(
    page.getByRole("combobox", { name: "Diff layout", exact: true }),
    "split",
  );
  await page
    .locator(".file-entry")
    .filter({ hasText: fileName(count - 1) })
    .click();
  await expect(page.locator(".diff-file header strong")).toHaveText(
    fileName(count - 1),
  );
  await expect(page.locator(".diff-file")).toContainText("after line 1");
  await page
    .locator(".diff-file")
    .getByRole("checkbox", { name: "Reviewed" })
    .check();
  await page.getByRole("button", { name: "Copy patch", exact: true }).click();
  await expect
    .poll(() =>
      app.evaluate(async ({ clipboard }) => await clipboard.readText()),
    )
    .toContain("+after line 50");
  await writeFile(
    join(workspace, fileName(count - 1)),
    "fresh review content\n",
  );
  await page
    .getByRole("button", { name: "Refresh changes", exact: true })
    .click();
  await expect(page.locator(".diff-file")).toContainText(
    "fresh review content",
  );
  await expect(
    page.locator(".diff-file").getByRole("checkbox", { name: "Reviewed" }),
  ).toBeChecked();
  await select(
    page.getByRole("combobox", { name: "Diff source", exact: true }),
    "branch",
  );
  await expect(page.locator(".diff-file")).toContainText(
    "fresh review content",
  );
  await select(
    page.getByRole("combobox", { name: "Context lines", exact: true }),
    "10000",
  );
  await expect(page.locator(".diff-file")).toContainText(
    "fresh review content",
  );
  await filter.fill("binary");
  await page.locator(".file-entry").filter({ hasText: "binary.bin" }).click();
  await expect(page.locator(".diff-file")).toContainText("Binary file changed");
  await filter.fill("deleted");
  await page.locator(".file-entry").filter({ hasText: "deleted.txt" }).click();
  await expect(page.locator(".diff-file")).toContainText("removed");
  await filter.fill("untracked");
  await page
    .locator(".file-entry")
    .filter({ hasText: "untracked [literal].txt" })
    .click();
  await expect(page.locator(".diff-file")).toContainText("second line");
  await filter.fill("src/");
  await page.getByRole("checkbox", { name: "Show every file at once" }).check();
  await expect(page.locator(".diff-file").first()).toContainText(
    "after line 1",
  );
  await pane.evaluate((element) => {
    element.scrollTop = element.scrollHeight;
  });
  await expect(
    page.locator(".diff-file").last().locator("header strong"),
  ).toHaveText(fileName(count - 1));
  await expect(page.locator(".diff-file").last()).toContainText(
    "before line 50",
  );
  async function visibleEnd(scroll = true) {
    await expect
      .poll(async () => {
        if (scroll)
          await pane.evaluate((element) => {
            element.scrollTop = element.scrollHeight;
          });
        if (await page.getByText("Loading changes…", { exact: true }).count())
          return false;
        return page
          .locator(".diff-file")
          .last()
          .locator(".diff-cell code")
          .filter({ hasText: /^before line 50$/ })
          .evaluateAll((elements) =>
            elements.some((element) => {
              const rect = element.getBoundingClientRect();
              const owner = element
                .closest(".diff-content")!
                .getBoundingClientRect();
              const hit = document.elementFromPoint(
                rect.left + rect.width / 2,
                rect.top + rect.height / 2,
              );
              return (
                rect.top >= owner.top &&
                rect.bottom <= owner.bottom &&
                !!hit &&
                element.contains(hit)
              );
            }),
          );
      })
      .toBe(true);
  }
  await visibleEnd();
  await page.waitForTimeout(1000);
  await visibleEnd(false);
  expect(await page.locator(".file-entry").count()).toBeLessThan(100);
  expect(await page.locator(".diff-line, .split-row").count()).toBeLessThan(
    200,
  );
  await page.screenshot({
    path: test.info().outputPath("all-files-bottom.png"),
  });
  await page.setViewportSize({ width: 1000, height: 700 });
  await select(
    page.getByRole("combobox", { name: "Diff layout", exact: true }),
    "split",
  );
  await visibleEnd();
  await page.waitForTimeout(2000);
  await visibleEnd(false);
  await page.screenshot({ path: test.info().outputPath("review-narrow.png") });
  expect(errors).toEqual([]);
});

test("last-turn review and a second repository keep content and comments isolated", async () => {
  test.skip(baseline);
  test.setTimeout(240000);
  await page.setViewportSize({ width: 1500, height: 950 });
  await page
    .getByRole("checkbox", { name: "Show every file at once" })
    .uncheck();
  await page.getByRole("searchbox", { name: "Filter changed files" }).fill("");
  await page.getByRole("button", { name: "New chat", exact: true }).click();
  const message = page.getByRole("textbox", { name: "Message", exact: true });
  const approval = page.getByRole("button", {
    name: /^Approvals: (Review|Auto)$/,
  });
  if ((await approval.getAttribute("aria-label")) !== "Approvals: Auto")
    await approval.click();
  await message.fill(
    '!tool write {"path":"snapshot-only.txt","content":"captured review content\\n"}',
  );
  await page
    .getByRole("button", { name: /Send message|Send guidance/ })
    .click();
  await expect
    .poll(() =>
      readFile(join(workspace, "snapshot-only.txt"), "utf8").catch(() => ""),
    )
    .toBe("captured review content\n");
  await expect.poll(async () => (await client.activeTurns()).length).toBe(0);
  await select(
    page.getByRole("combobox", { name: "Diff source", exact: true }),
    "last",
  );
  await expect(page.locator(".diff-file")).toContainText(
    "captured review content",
  );
  await writeFile(join(workspace, "snapshot-only.txt"), "later human edit\n");
  await page
    .getByRole("button", { name: "Refresh changes", exact: true })
    .click();
  await expect(page.locator(".diff-file")).toContainText(
    "captured review content",
  );
  await expect(page.locator(".diff-file")).not.toContainText(
    "later human edit",
  );
  await page.getByRole("button", { name: "Copy patch", exact: true }).click();
  await expect
    .poll(() =>
      app.evaluate(async ({ clipboard }) => await clipboard.readText()),
    )
    .toContain("+captured review content");
  await select(
    page.getByRole("combobox", { name: "Diff source", exact: true }),
    "working",
  );
  await page
    .getByRole("searchbox", { name: "Filter changed files" })
    .fill("snapshot-only");
  await page
    .locator(".file-entry")
    .filter({ hasText: "snapshot-only.txt" })
    .click();
  await expect(page.locator(".diff-file")).toContainText("later human edit");

  const other = join(temporary, "second repository 日本");
  extra.push(other);
  await mkdir(join(other, ".haarness"), { recursive: true });
  await writeFile(
    join(other, ".haarness/config.toml"),
    '[defaults]\nprovider="echo"\nmodel="echo-1"\n',
  );
  await writeFile(join(other, big), "second repository before\n");
  await writeFile(join(other, "wide.txt"), "before ".repeat(2000) + "TAIL\n");
  for (const args of [
    ["init", "-b", "main"],
    ["add", "."],
    [
      "-c",
      "user.name=Review Fixture",
      "-c",
      "user.email=review@example.test",
      "commit",
      "-m",
      "Second fixture",
    ],
  ])
    execFileSync("git", args, { cwd: other, stdio: "ignore" });
  await writeFile(join(other, big), "second repository after\n");
  await writeFile(join(other, "wide.txt"), "after ".repeat(2000) + "TAIL\n");
  for (let i = 0; i < 200; i++)
    await writeFile(join(other, `new-${i}.txt`), "new repository file\n");
  await writeFile(join(other, "huge.txt"), "x".repeat(10_001_024));
  await app.evaluate(({ dialog }, directory) => {
    dialog.showOpenDialog = async () => ({
      canceled: false,
      filePaths: [directory],
    });
  }, other);
  await page
    .getByRole("button", { name: "Open another project", exact: true })
    .click();
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
  );
  await expect(page.locator(".review")).toBeVisible();
  const toggle = page.getByRole("button", {
    name: "Toggle changed files",
    exact: true,
  });
  if ((await toggle.getAttribute("aria-pressed")) !== "true")
    await toggle.click();
  await page.getByRole("searchbox", { name: "Filter changed files" }).fill(big);
  await page.locator(".file-entry").filter({ hasText: big }).click();
  await expect(page.locator(".diff-file")).toContainText(
    "second repository after",
  );
  await expect(page.locator(".diff-file")).not.toContainText("after line 1");
  await page
    .getByRole("searchbox", { name: "Filter changed files" })
    .fill("huge");
  await page.locator(".file-entry").filter({ hasText: "huge.txt" }).click();
  await expect(page.locator(".diff-file")).toContainText(
    "truncated by the server",
  );
  await page.screenshot({
    path: test.info().outputPath("bounded-huge-file.png"),
  });
  await page
    .getByRole("searchbox", { name: "Filter changed files" })
    .fill("wide");
  await page.locator(".file-entry").filter({ hasText: "wide.txt" }).click();
  await expect(page.locator(".diff-cell.add code")).toContainText("TAIL");
  expect(
    await page
      .locator(".diff-content")
      .evaluate((element) => element.scrollWidth <= element.clientWidth),
  ).toBe(true);
  await page.screenshot({
    path: test.info().outputPath("long-line-wrapping.png"),
  });
  const indexPath = join(other, ".git/index");
  const savedIndex = await readFile(indexPath);
  await writeFile(indexPath, "temporary invalid index for recovery test");
  await page
    .getByRole("button", { name: "Refresh changes", exact: true })
    .click();
  await expect(page.locator(".review")).toContainText(
    "Changes are unavailable.",
  );
  await expect(
    page.getByRole("button", { name: "Refresh changes", exact: true }),
  ).toBeVisible();
  await writeFile(indexPath, savedIndex);
  await page
    .getByRole("button", { name: "Refresh changes", exact: true })
    .click();
  await expect(page.locator(".diff-file")).toContainText("TAIL");
  await rename(join(other, ".git"), join(temporary, "held-git"));
  await page
    .getByRole("button", { name: "Refresh changes", exact: true })
    .click();
  await expect(page.locator(".review")).toContainText("isn’t a Git repository");
  await rename(join(temporary, "held-git"), join(other, ".git"));
  await page.getByRole("button", { name: "Check again", exact: true }).click();
  await expect(page.locator(".diff-file")).toContainText("TAIL");
  execFileSync("git", ["add", "."], { cwd: other, stdio: "ignore" });
  execFileSync(
    "git",
    [
      "-c",
      "user.name=Review Fixture",
      "-c",
      "user.email=review@example.test",
      "commit",
      "-m",
      "Clean review fixture",
    ],
    { cwd: other, stdio: "ignore" },
  );
  await page
    .getByRole("button", { name: "Refresh changes", exact: true })
    .click();
  await expect(page.locator(".review")).toContainText("A clean slate.");
  expect(errors).toEqual([]);
});
