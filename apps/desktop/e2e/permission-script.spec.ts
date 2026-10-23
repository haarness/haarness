import { test, expect, _electron as electron, type ElectronApplication, type Page, type Locator } from "@playwright/test";
import { mkdtemp, mkdir, writeFile, readFile, realpath } from "node:fs/promises";
import { execFileSync } from "node:child_process";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

// A shell request that runs a script carries the script, and the approval card offers it behind a
// quiet toggle under the command: closed at first, so the card keeps its size, opened in place by
// mouse or keyboard, its text in a short box that scrolls on its own.
let app: ElectronApplication, page: Page, root: string, project: string, env: Record<string, string>;
const binary = resolve(process.env.HAARNESS_BIN ?? "../../target/debug/haarness");
const shots = process.env.HAARNESS_SCREENSHOTS;
const errors: string[] = [];
test.describe.configure({ mode: "serial" });

const deploy = [
  "#!/bin/sh",
  "# Build the site and publish it.",
  "set -eu",
  'out="${OUT:-dist}"',
  'rm -rf "$out" && mkdir -p "$out"',
  "for page in index about; do",
  '\tprintf "<h1>%s</h1>\\n" "$page" > "$out/$page.html"',
  "done",
  'echo "would upload $out/index.html with --fail --retry 3 to https://deploy.example.test/sites/haarness-fixture/releases/current/index.html"',
  "printf DEPLOYED > deployed.txt",
  'echo "published $(ls "$out" | wc -l) pages"',
  "exit 0",
];

async function api(method: string, ...args: unknown[]): Promise<any> {
  return page.evaluate(({ connection, method, args }) => window.haarness.api(connection, method, args), {
    connection: "local:" + project,
    method,
    args,
  });
}
async function send(command: string) {
  const box = page.getByRole("textbox", { name: "Message", exact: true });
  await box.fill(" !tool bash " + JSON.stringify({ command }));
  await page.getByRole("button", { name: /Send message|Send guidance/ }).click();
}
async function idle() {
  await expect.poll(async () => (await api("activeTurns")).length, { timeout: 30000 }).toBe(0);
}
const card = () => page.locator(".prompt-dock.approval");
const toggle = () => card().locator(".approval-script-toggle");
/** Light and dark pictures of the card, kept when HAARNESS_SCREENSHOTS names a folder. */
async function pictures(target: Locator, name: string) {
  for (const theme of ["light", "dark"] as const) {
    await page.evaluate((theme) => window.haarness.preferences({ theme }), theme);
    await expect(page.locator("html")).toHaveAttribute("data-theme", theme);
    await page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done))));
    await target.screenshot({ path: shots ? join(shots, `${name}-${theme}.png`) : test.info().outputPath(`${name}-${theme}.png`) });
  }
  await page.evaluate(() => window.haarness.preferences({ theme: "light" }));
}

test.beforeAll(async () => {
  test.setTimeout(120000);
  root = await realpath(await mkdtemp(join(tmpdir(), "haarness-permission-script-ui-")));
  project = join(root, "project");
  await mkdir(join(project, ".haarness"), { recursive: true });
  await mkdir(join(project, "scripts"), { recursive: true });
  await writeFile(
    join(project, ".haarness/config.toml"),
    '[defaults]\nprovider="echo"\nmodel="echo-1"\n[permissions]\nbash="ask"\n[execution]\nshell_timeout_ms=0\n',
  );
  await writeFile(join(project, "scripts/deploy.sh"), deploy.join("\n") + "\n");
  await writeFile(
    join(project, "scripts/big.sh"),
    Array.from({ length: 400 }, (_, line) => `echo big-${String(line + 1).padStart(4, "0")} ${"x".repeat(100)}\n`).join(""),
  );
  await writeFile(join(project, ".env.local"), "TOKEN=fixture-only\n");
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
  }, project);
  await page.getByRole("region", { name: "Project home" }).getByRole("button", { name: "Open project", exact: true }).click();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 60000 });
  await page.getByRole("button", { name: "New chat", exact: true }).click();
  await expect(page.getByRole("textbox", { name: "Message", exact: true })).toBeVisible();
});
test.afterAll(async () => {
  if (app) {
    await app.evaluate(({ BrowserWindow }) => { for (const win of BrowserWindow.getAllWindows()) win.destroy(); }).catch(() => {});
    await app.close().catch(() => {});
  }
  try { execFileSync(binary, ["--workspace", project, "serve", "--stop"], { env, stdio: "ignore", timeout: 30000 }); } catch {}
  await test.info().attach("fixture", { body: root ?? "not started", contentType: "text/plain" });
});

test("the card offers the script a command runs, closed until asked for, by mouse or keyboard", async () => {
  await send("cd scripts && sh deploy.sh");
  await expect(card()).toBeVisible();
  // Closed on first paint: the toggle is there with the card, the text is not, and the card
  // does not grow by itself.
  await expect(toggle()).toHaveText("View script · 12 lines");
  await expect(toggle()).toHaveAttribute("aria-expanded", "false");
  await expect(card().locator(".approval-script-body")).toHaveCount(0);
  const closed = (await card().boundingBox())!.height;
  await page.waitForTimeout(300);
  expect((await card().boundingBox())!.height).toBeCloseTo(closed, 0);
  await pictures(card(), "card-closed");

  // Keyboard: Tab reaches the toggle, Enter opens it.
  await toggle().focus();
  await page.keyboard.press("Enter");
  await expect(toggle()).toHaveText("Hide script");
  await expect(toggle()).toHaveAttribute("aria-expanded", "true");
  const body = card().locator(".approval-script-body");
  await expect(body.locator(".change-path")).toHaveText("scripts/deploy.sh");
  const lines = body.locator(".change-lines .diff-line");
  await expect(lines).toHaveCount(12);
  await expect(lines.first()).toHaveText("#!/bin/sh");
  await expect(lines.nth(6)).toHaveText('    printf "<h1>%s</h1>\\n" "$page" > "$out/$page.html"');
  await expect(lines.last()).toHaveText("exit 0");
  await expect(body.locator(".change-more")).toHaveCount(0);
  // Its own short box: the answers stay in view below it.
  const region = body.getByRole("region", { name: "Script", exact: true });
  expect((await region.boundingBox())!.height).toBeLessThanOrEqual(202);
  await expect(card().getByRole("button", { name: "Allow once", exact: true })).toBeInViewport();
  // A long line wraps whole: nothing of it is past the box's edge.
  expect(await region.evaluate((element) => element.scrollWidth <= element.clientWidth)).toBe(true);
  await expect(lines.nth(8)).toContainText("releases/current/index.html");
  await toggle().blur();
  await pictures(card(), "card-open");

  // The mouse closes it again; nothing else on the card changed.
  await toggle().click();
  await expect(toggle()).toHaveText("View script · 12 lines");
  await expect(card().locator(".approval-script-body")).toHaveCount(0);
  expect((await card().boundingBox())!.height).toBeCloseTo(closed, 0);

  await card().getByRole("button", { name: "Allow once", exact: true }).click();
  await idle();
  expect(await readFile(join(project, "scripts/deployed.txt"), "utf8")).toBe("DEPLOYED");
});

test("a long script scrolls in its box and says only its start is shown", async () => {
  await send("sh scripts/big.sh");
  await expect(toggle()).toBeVisible();
  await toggle().click();
  const region = card().getByRole("region", { name: "Script", exact: true });
  const size = await region.evaluate((element) => ({ client: element.clientHeight, scroll: element.scrollHeight }));
  expect(size.scroll).toBeGreaterThan(size.client);
  // Focused, the box scrolls from the keyboard.
  await region.focus();
  await page.keyboard.press("End");
  await expect.poll(() => region.evaluate((element) => element.scrollTop)).toBeGreaterThan(0);
  await expect(card().locator(".change-more")).toHaveText("Only the start of the file is shown.");
  await expect(card().locator(".approval-script-body .diff-line", { hasText: "big-0400" })).toHaveCount(0);
  await pictures(card(), "card-open-long");
  await card().getByRole("button", { name: "Deny", exact: true }).click();
  await idle();
});

test("a secret-named file, or a command that runs no script, shows no toggle", async () => {
  for (const command of ["source .env.local", "printf plain"]) {
    await send(command);
    await expect(card().locator(".approval-command")).toHaveText(command);
    await expect(toggle()).toHaveCount(0);
    await card().getByRole("button", { name: "Deny", exact: true }).click();
    await idle();
  }
  expect(errors).toEqual([]);
});
