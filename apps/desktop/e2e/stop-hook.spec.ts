import { test, expect, _electron as electron, type ElectronApplication, type Page } from "@playwright/test";
import { mkdtemp, mkdir, writeFile, realpath } from "node:fs/promises";
import { execFileSync } from "node:child_process";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { stopChatHome } from "./fixtures/chat-home";
import { isolatedEnv } from "./fixtures/isolated-env";

// A Stop hook that blocks keeps the turn going: the conversation shows its
// feedback as one quiet line between the replies, in a real Desktop against
// a real server with the echo provider.
let app: ElectronApplication, page: Page, root: string, workspace: string;
let env: Record<string, string>;
const binary = resolve(process.env.HAARNESS_BIN ?? "../../target/debug/haarness");
const errors: string[] = [];
test.describe.configure({ mode: "serial" });

async function api(method: string, ...args: unknown[]): Promise<any> {
  return page.evaluate(({ connection, method, args }) => window.haarness.api(connection, method, args), {
    connection: "local:" + workspace,
    method,
    args,
  });
}
async function shot(name: string) {
  // Theme and language changes fade in.
  await page.waitForTimeout(400);
  await page.screenshot({ path: test.info().outputPath(name + ".png") });
}
async function send(text: string) {
  await page.getByRole("button", { name: "New chat", exact: true }).click();
  const box = page.getByRole("textbox", { name: "Message", exact: true });
  await box.fill(text);
  await page.getByRole("button", { name: /Send message|Send guidance/ }).click();
  await expect.poll(async () => (await api("activeTurns")).length, { timeout: 30000 }).toBe(0);
}

test.beforeAll(async () => {
  test.setTimeout(120000);
  root = await realpath(await mkdtemp(join(process.env.HAARNESS_E2E_DIR ?? tmpdir(), "haarness-stop-hook-ui-")));
  workspace = join(root, "project");
  await mkdir(join(workspace, ".haarness"), { recursive: true });
  await writeFile(join(workspace, ".haarness/config.toml"), '[defaults]\nprovider="echo"\nmodel="echo-1"\n');
  // Blocks once a turn with what feedback.txt says.
  const hook = join(root, "stop.py");
  await writeFile(
    hook,
    [
      "import json, pathlib, sys",
      "data = json.load(sys.stdin)",
      "if not data['stop_hook_active']:",
      `    print(pathlib.Path(${JSON.stringify(join(root, "feedback.txt"))}).read_text(), file=sys.stderr)`,
      "    sys.exit(2)",
    ].join("\n"),
  );
  await writeFile(join(root, "feedback.txt"), "run the tests before you finish");
  await writeFile(
    join(workspace, "haarness.json"),
    JSON.stringify({ hooks: { Stop: [{ hooks: [{ type: "command", command: `python3 ${JSON.stringify(hook)}` }] }] } }, null, 1),
  );
  env = isolatedEnv();
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
  await page.getByRole("button", { name: "Open another project", exact: true }).click();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 60000 });
  await page.evaluate(() => window.haarness.preferences({ theme: "light" }));
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
  stopChatHome(binary, env);
  await test.info().attach("fixture", { body: root ?? "not started", contentType: "text/plain" });
});

test("a Stop hook's feedback shows as one quiet line between the replies it came between", async () => {
  await send("fix the bug");
  const messages = page.locator(".transcript article.message");
  const note = page.locator(".hook-note");
  await expect(note).toHaveCount(1);
  await expect(note).toHaveText(/^Stop hook\s*·\s*run the tests before you finish$/);
  await expect(note).toHaveAttribute("title", "run the tests before you finish");
  // The note stands for the message the model read, which is never shown as
  // a message of the person's (the echo model repeats it in its reply).
  await expect(page.locator(".message.user")).toHaveCount(1);
  await expect(page.locator(".message.user")).toHaveText(/fix the bug/);
  const kinds = await messages.evaluateAll((rows) =>
    rows.map((row) => (row.querySelector(".hook-note") ? "note" : row.getAttribute("data-type"))),
  );
  expect(kinds).toEqual(["user_message", "assistant_message", "note", "assistant_message"]);
  // Theme tokens only: the muted colour and the small size of other notes.
  const [noteStyle, muted] = await note.evaluate((element) => {
    const style = getComputedStyle(element);
    const probe = document.createElement("span");
    probe.style.color = "var(--muted)";
    document.body.append(probe);
    const color = getComputedStyle(probe).color;
    probe.remove();
    return [{ color: style.color, size: style.fontSize }, color];
  });
  expect(noteStyle.color).toBe(muted);
  await shot("stop-hook-note-light");
  await page.evaluate(() => window.haarness.preferences({ theme: "dark" }));
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await shot("stop-hook-note-dark");
  await page.evaluate(() => window.haarness.preferences({ theme: "light", language: "ar" }));
  await expect(page.locator("html")).toHaveAttribute("dir", "rtl");
  await expect(note).toContainText("خطاف Stop");
  await shot("stop-hook-note-rtl");
  await page.evaluate(() => window.haarness.preferences({ language: "en" }));
  await expect(page.locator("html")).not.toHaveAttribute("dir", "rtl");
  expect(errors).toEqual([]);
});

test("long feedback stays one line, with all of it in the note's title", async () => {
  const long = ["The suite failed:", ...Array.from({ length: 12 }, (_, index) => `  test ${index + 1} expected a different value from the parser`)].join("\n");
  await writeFile(join(root, "feedback.txt"), long);
  await send("tidy up");
  const note = page.locator(".hook-note");
  await expect(note).toHaveCount(1);
  await expect(note).toHaveAttribute("title", long);
  const text = note.locator(".hook-note-text");
  const [height, line, overflowing] = await text.evaluate((element) => [
    element.getBoundingClientRect().height,
    parseFloat(getComputedStyle(element).lineHeight),
    element.scrollWidth > element.clientWidth,
  ]);
  expect(height).toBeLessThanOrEqual(line + 1);
  expect(overflowing).toBe(true);
  await shot("stop-hook-note-long-light");
  await page.evaluate(() => window.haarness.preferences({ theme: "dark" }));
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await shot("stop-hook-note-long-dark");
  await page.evaluate(() => window.haarness.preferences({ theme: "light" }));
  expect(errors).toEqual([]);
});
