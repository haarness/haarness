import { test, expect, _electron as electron, type ElectronApplication, type Page, type Locator } from "@playwright/test";
import { mkdtemp, mkdir, writeFile, rm, realpath } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";

// For a project whose configuration is not trusted, Review never downloads
// what a partial clone left out (the server sets GIT_NO_LAZY_FETCH), so a
// branch review whose base was never downloaded says how to get it, quietly,
// in the empty state Review already uses. A trusted project's Review fetches
// it, as `git diff` would.
const binary = resolve(process.env.HAARNESS_BIN ?? "../../target/debug/haarness");
let app: ElectronApplication;
let page: Page;
let temporary: string;
let workspace: string;
let env: Record<string, string>;

async function select(control: Locator, value: string) {
  await control.click();
  const id = await control.getAttribute("aria-controls");
  await page.locator(`[id=${JSON.stringify(id)}] [role="option"][data-value=${JSON.stringify(value)}]`).click();
  const done = page.locator(".select-footer").getByRole("button", { name: "Done", exact: true });
  if (await done.isVisible()) await done.click();
}

test.beforeAll(async () => {
  test.setTimeout(300000);
  temporary = await realpath(await mkdtemp(join(tmpdir(), "haarness-review-partial-")));
  const origin = join(temporary, "origin");
  workspace = join(temporary, "partial");
  await mkdir(origin);
  const git = (cwd: string, ...args: string[]) =>
    execFileSync("git", ["-c", "user.name=Fixture", "-c", "user.email=fixture@example.test", ...args], {
      cwd,
      stdio: "ignore",
    });
  git(origin, "init", "-q", "-b", "main");
  git(origin, "config", "uploadpack.allowFilter", "true");
  await writeFile(join(origin, "file.txt"), "one\n");
  git(origin, "add", "--all");
  git(origin, "commit", "-q", "-m", "one");
  git(origin, "checkout", "-q", "-b", "feature");
  await writeFile(join(origin, "file.txt"), "two\n");
  git(origin, "commit", "-q", "-am", "two");
  git(origin, "checkout", "-q", "main");
  git(temporary, "clone", "-q", "--filter=blob:none", "--branch", "feature", `file://${origin}`, workspace);
  env = Object.fromEntries(
    Object.entries(process.env).filter((entry): entry is [string, string] => entry[1] !== undefined),
  );
  Object.assign(env, {
    HAARNESS_BIN: binary,
    HAARNESS_HOME: join(temporary, "home"),
    HAARNESS_DATA_DIR: join(temporary, "state"),
    HAARNESS_DESKTOP_DATA: join(temporary, "desktop"),
    HAARNESS_TEST_PROVIDER: "1",
    HAARNESS_DESKTOP_TEST: "1",
    HAARNESS_DISABLE_PROJECT_CONFIG: "1",
  });
  delete env.ELECTRON_RUN_AS_NODE;
  app = await electron.launch({ args: ["."], env, timeout: 240000 });
  page = await app.firstWindow();
  await page.setViewportSize({ width: 1300, height: 860 });
  await app.evaluate(({ dialog }, directory) => {
    dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [directory] });
  }, workspace);
  await page
    .getByRole("region", { name: "Project home" })
    .getByRole("button", { name: "Open project", exact: true })
    .click();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 240000 });
});

test.afterAll(async () => {
  await app?.close().catch(() => {});
  if (workspace)
    execFileSync(binary, ["--workspace", workspace, "serve", "--stop"], { env, stdio: "ignore", timeout: 30000 });
  if (temporary) await rm(temporary, { recursive: true, force: true });
});

test("an untrusted project's branch review of a partial clone says how to download what it needs", async () => {
  const show = page.getByRole("button", { name: "Show side panel", exact: true });
  if (await show.isVisible()) await show.click();
  await page.getByRole("button", { name: "Open panel", exact: true }).click();
  await page
    .getByRole("region", { name: "Open a project panel" })
    .getByRole("button", { name: "Open Review", exact: true })
    .click();
  await select(page.getByRole("combobox", { name: "Diff source", exact: true }), "branch");
  const review = page.locator(".review");
  await expect(review).toContainText("Review needs files this clone hasn’t downloaded yet.", { timeout: 60000 });
  await expect(review).toContainText("Project configuration isn’t trusted, so Review doesn’t download them. Run git backfill (Git 2.49 or later), then check again.");
  await expect(review).not.toContainText("promisor");
  // The source picker stays, so the working tree is one choice away.
  await expect(page.getByRole("combobox", { name: "Diff source", exact: true })).toBeVisible();
  for (const theme of ["light", "dark"] as const) {
    await page.evaluate((theme) => window.haarness.preferences({ theme }), theme);
    await page.waitForTimeout(300);
    await review.screenshot({ path: test.info().outputPath(`partial-clone-${theme}.png`) });
  }
  // Check again reads again and keeps the explanation while nothing changed.
  await review.getByRole("button", { name: "Check again", exact: true }).click();
  await expect(review).toContainText("Review needs files this clone hasn’t downloaded yet.");
  // Once the files are downloaded, the review shows the change.
  execFileSync("git", ["backfill"], { cwd: workspace, stdio: "ignore", env: { ...process.env, GIT_TERMINAL_PROMPT: "0" } });
  await review.getByRole("button", { name: "Check again", exact: true }).click();
  await expect(page.locator(".diff-file").first()).toContainText("two", { timeout: 60000 });
});
