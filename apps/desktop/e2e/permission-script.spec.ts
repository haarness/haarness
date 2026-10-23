import { test, expect, _electron as electron, type ElectronApplication, type Page, type Locator } from "@playwright/test";
import { chmod, mkdtemp, mkdir, writeFile, readFile, realpath } from "node:fs/promises";
import { execFileSync } from "node:child_process";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

// A shell request that runs a script carries the script, and the approval card offers it behind a
// quiet toggle under the command: closed at first, so the card keeps its size, opened in place by
// mouse or keyboard, its text in a short box that scrolls on its own.
let app: ElectronApplication, page: Page, root: string, project: string, env: Record<string, string>;
// A project with the default rules (bash runs without asking), a folder
// outside it, and the project the tests are in now.
let plain: string, outside: string, current: string;
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
    connection: "local:" + current,
    method,
    args,
  });
}
/** Sends `!tool bash` with `command`, by the composer's own markers, so it
 * works in any interface language. */
async function send(command: string) {
  await page.locator("[data-composer-input]").fill(" !tool bash " + JSON.stringify({ command }));
  await page.locator(".send-button").click();
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
  plain = join(root, "plain");
  outside = join(root, "outside");
  await mkdir(join(plain, ".haarness"), { recursive: true });
  await mkdir(outside, { recursive: true });
  await writeFile(join(plain, ".haarness/config.toml"), '[defaults]\nprovider="echo"\nmodel="echo-1"\n');
  await writeFile(join(outside, "run.sh"), "#!/bin/sh\nprintf ran > ran.txt\n");
  await chmod(join(outside, "run.sh"), 0o755);
  current = project;
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
  for (const workspace of [project, plain])
    try { execFileSync(binary, ["--workspace", workspace, "serve", "--stop"], { env, stdio: "ignore", timeout: 30000 }); } catch {}
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

  // Keyboard: from the card's title, Tab reaches the toggle and Enter
  // opens it.
  await card().locator(".prompt-title").click();
  await page.keyboard.press("Tab");
  await expect(toggle()).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(toggle()).toHaveText("Hide script");
  await expect(toggle()).toHaveAttribute("aria-expanded", "true");
  const body = card().locator(".approval-script-body");
  // Inside the card's alert, its text is read on request, not announced.
  await expect(body).toHaveAttribute("aria-live", "off");
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
  // The count of a cut file is of what was read.
  await expect(toggle()).toHaveText(/^View script · first \d+ lines$/);
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

test("a script whose text may not be what runs is named in one quiet line, with no toggle", async () => {
  await send("cat > scripts/gen.sh <<'EOF'\necho NEW\nEOF\nsh scripts/gen.sh");
  await expect(card().locator(".approval-script-note")).toHaveText(
    "This command writes scripts/gen.sh before running it, so its contents can't be shown.",
  );
  await expect(toggle()).toHaveCount(0);
  await pictures(card(), "card-written");
  await card().getByRole("button", { name: "Deny", exact: true }).click();
  await idle();
  // After a step that may change it, its own line.
  await send("git checkout -- scripts/deploy.sh && sh scripts/deploy.sh");
  await expect(card().locator(".approval-script-note")).toHaveText(
    "An earlier step of this command can change scripts/deploy.sh, so its contents aren't shown.",
  );
  await expect(toggle()).toHaveCount(0);
  await pictures(card(), "card-changeable");
  await card().getByRole("button", { name: "Deny", exact: true }).click();
  await idle();
});

test("in Arabic the card reads right to left while the script keeps left to right", async () => {
  await page.evaluate(() => window.haarness.preferences({ language: "ar" }));
  await expect.poll(() => page.evaluate(() => document.documentElement.dir)).toBe("rtl");
  await send("cd scripts && sh deploy.sh");
  await expect(toggle()).toHaveText("عرض النص البرمجي · عدد الأسطر: 12");
  await toggle().click();
  await expect(toggle()).toHaveText("إخفاء النص البرمجي");
  const body = card().locator(".approval-script-body");
  expect(
    await body
      .locator(".change-path bdi, .change-lines")
      .evaluateAll((parts) => parts.map((part) => getComputedStyle(part).direction)),
  ).toEqual(["ltr", "ltr"]);
  await expect(body.locator(".diff-line").first()).toHaveText("#!/bin/sh");
  await pictures(card(), "card-open-ar");
  await card().getByRole("button", { name: "رفض", exact: true }).click();
  await idle();
  await page.evaluate(() => window.haarness.preferences({ language: "en" }));
  await expect.poll(() => page.evaluate(() => document.documentElement.dir)).toBe("ltr");
});

test("a secret-named file, or a command that runs no script, shows no toggle", async () => {
  for (const command of ["source .env.local", "printf plain"]) {
    await send(command);
    await expect(card().locator(".approval-command")).toHaveText(command);
    await expect(toggle()).toHaveCount(0);
    await card().getByRole("button", { name: "Deny", exact: true }).click();
    await idle();
  }
});

test("a card never takes the keyboard from the message box: typing as it comes answers nothing", async () => {
  const box = page.locator("[data-composer-input]");
  await box.fill(" !tool bash " + JSON.stringify({ command: "printf keyboard > keyboard.txt" }));
  await box.press("Enter");
  // Typing straight on, as the card comes: spaces, Enter and all.
  await page.keyboard.type("also check tests");
  await expect(card()).toBeVisible();
  await page.keyboard.type(" and then the docs");
  await expect(box).toBeFocused();
  await expect(box).toHaveValue("also check tests and then the docs");
  await page.keyboard.press(" ");
  await expect(box).toHaveValue("also check tests and then the docs ");
  expect((await api("listPermissions")).length).toBe(1);
  // Deny, Always allow and Allow once share one line.
  const tops = await Promise.all(
    ["Deny", "Always allow", "Allow once"].map(async (name) =>
      Math.round((await card().getByRole("button", { name, exact: true }).boundingBox())!.y),
    ),
  );
  expect(new Set(tops).size).toBe(1);
  // Enter sends the sentence as the person's own message; the card waits.
  await page.keyboard.press("Enter");
  await expect(box).toHaveValue("");
  await page.waitForTimeout(300);
  expect((await api("listPermissions")).length).toBe(1);
  await expect(card()).toBeVisible();
  // Its answers are a Shift+Tab away; one chosen there answers it, and the
  // keyboard goes back to the message box.
  await page.keyboard.press("Shift+Tab");
  await expect(card().getByRole("button", { name: "Allow once", exact: true })).toBeFocused();
  await page.keyboard.press("Enter");
  await idle();
  await expect(card()).toHaveCount(0);
  await expect(box).toBeFocused();
  expect(await readFile(join(project, "keyboard.txt"), "utf8")).toBe("keyboard");
});

test("a card that takes the keyboard because nothing had it sends what is typed to the message box", async () => {
  const box = page.locator("[data-composer-input]");
  await box.fill(" !tool bash " + JSON.stringify({ command: "printf idle > idle.txt" }));
  await page.locator(".send-button").click();
  await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
  await expect(card()).toBeVisible();
  await expect(card()).toBeFocused();
  // The card binds no key: typing there goes to the message box, and
  // neither Space nor Enter answers it.
  await page.keyboard.type("hi there");
  await expect(box).toBeFocused();
  await expect(box).toHaveValue("hi there");
  expect((await api("listPermissions")).length).toBe(1);
  // The Always slot is as wide as the wider of its button and its menu, so
  // the answers keep their places whichever it shows.
  const widths = await card().locator(".prompt-always-slot").evaluate((slot) => [
    slot.getBoundingClientRect().width,
    ...[...slot.children].map((child) => child.getBoundingClientRect().width),
  ]);
  expect(widths).toHaveLength(3);
  expect(Math.abs(widths[0] - Math.max(widths[1], widths[2]))).toBeLessThan(1);
  await card().getByRole("button", { name: "Deny", exact: true }).click();
  await idle();
  await box.fill("");
});

test("a step opened while its turn waits on a card stays where it was clicked", async () => {
  const box = page.locator("[data-composer-input]");
  const transcript = page.locator(".transcript");
  const jump = page.getByRole("button", { name: "Jump to latest", exact: true });
  await box.fill("::long 40");
  await box.press("Enter");
  await expect(transcript).toContainText("line 40 marker-40");
  await idle();
  await box.fill(" !multi " + JSON.stringify([
    { name: "bash", arguments: { command: "echo first-step-output; seq 1 30" } },
    { name: "bash", arguments: { command: "printf second > second-step.txt" } },
  ]));
  await box.press("Enter");
  await expect(card()).toBeVisible();
  await card().getByRole("button", { name: "Allow once", exact: true }).click();
  await expect(card().locator(".approval-command")).toHaveText("printf second > second-step.txt");
  const settle = () => page.evaluate(() => new Promise((done) => setTimeout(() => requestAnimationFrame(() => requestAnimationFrame(done)), 300)));
  await settle();
  /** Where a summary sits for a second after its click: it may come up
   * once, by at most 80px, for its first lines to show at the bottom of
   * the view, and then it holds. */
  const holds = async (summary: Locator, before: number) => {
    const places: number[] = [];
    for (let sample = 0; sample < 8; sample++) {
      await page.waitForTimeout(120);
      places.push((await summary.boundingBox())!.y);
    }
    for (const place of places) {
      expect(before - place).toBeLessThanOrEqual(81);
      expect(place - before).toBeLessThan(2);
      expect(Math.abs(place - places[0])).toBeLessThan(2);
    }
  };
  // A finished step of the waiting turn: opening it moves nothing, and as its
  // output pushes the end of the chat out of view, following pauses.
  const finished = page.locator(".tool-call").filter({ hasText: "first-step-output" }).last();
  const first = finished.locator(":scope > summary");
  if ((await finished.getAttribute("open")) !== null) {
    await first.click();
    await settle();
  }
  const at = (await first.boundingBox())!.y;
  await first.click();
  await expect(finished.locator(".tool-output")).toContainText("first-step-output");
  await holds(first, at);
  await expect(jump).toBeVisible();
  // Closing it again takes following back up.
  await first.click();
  await expect(jump).toHaveCount(0);
  await settle();
  // The running step at the end: opening it moves nothing either; what it
  // writes once it runs is followed.
  const running = page.locator(".tool-call").filter({ hasText: "second-step.txt" }).last();
  const second = running.locator(":scope > summary");
  const rest = (await second.boundingBox())!.y;
  await second.click();
  await expect(running.locator(".tool-empty")).toHaveText("Waiting for output…");
  await holds(second, rest);
  await card().getByRole("button", { name: "Allow once", exact: true }).click();
  await idle();
  await expect(transcript).toContainText("All tools finished");
  await expect(jump).toHaveCount(0);
  const below = () => transcript.evaluate((element) => element.scrollHeight - element.scrollTop - element.clientHeight);
  await expect.poll(below).toBeLessThan(5);
  // End, pressed before an opening has settled, still goes back to the
  // latest: the opening's hold gives way instead of pausing again after it.
  if ((await finished.getAttribute("open")) !== null) {
    await first.click();
    await settle();
  }
  await transcript.evaluate((element) => element.scrollBy({ top: -200 }));
  await first.click();
  await page.keyboard.press("End");
  await page.waitForTimeout(700);
  await expect(finished).toHaveAttribute("open", "");
  await expect(jump).toHaveCount(0);
  await expect.poll(below).toBeLessThan(5);
});

test("under the default rules the outside folder's card offers the script that runs there", async () => {
  await app.evaluate(({ dialog }, path) => {
    dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [path] });
  }, plain);
  await page.getByRole("button", { name: "Open another project", exact: true }).click();
  current = plain;
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 60000 });
  await expect(page.locator(".titlebar")).toContainText("plain");
  await page.getByRole("button", { name: "New chat", exact: true }).click();
  await send(`cd ${outside} && ./run.sh`);
  // `bash` asks nothing here; the one card is about the folder.
  await expect(card().locator(".prompt-title")).toHaveText("Access a folder outside this project");
  await expect(toggle()).toHaveText("View script · 2 lines");
  await toggle().click();
  const path = card().locator(".approval-script-body .change-path");
  await expect(path).toHaveText(join(outside, "run.sh"));
  // A long path gives way from its start: the file's own name stays in view.
  const [box, name] = await Promise.all([path.boundingBox(), path.locator("bdi").boundingBox()]);
  expect(name!.x + name!.width).toBeLessThanOrEqual(box!.x + box!.width);
  await expect(card().locator(".approval-script-body .diff-line").last()).toHaveText("printf ran > ran.txt");
  await pictures(card(), "card-outside-open");
  await card().getByRole("button", { name: "Allow once", exact: true }).click();
  await idle();
  expect(await readFile(join(outside, "ran.txt"), "utf8")).toBe("ran");
  expect(errors).toEqual([]);
});
