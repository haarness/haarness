import { test, expect, _electron as electron, type ElectronApplication, type Locator, type Page } from "@playwright/test";
import { mkdtemp, mkdir, writeFile, realpath } from "node:fs/promises";
import { execFileSync } from "node:child_process";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { stopChatHome } from "./fixtures/chat-home";
import { isolatedEnv } from "./fixtures/isolated-env";

// A reply the provider's safety check stopped: the server leaves the turn out
// of later requests, and the error row offers one way on, Edit request, which
// undoes the turn and puts the message back in the box (in a folder Undo has
// no checkpoints for, it puts the message back all the same). Against a real
// server with the echo provider, which ends a reply with any finish reason it
// is told. Pictures are kept in HAARNESS_SCREENSHOTS when it is set.
let app: ElectronApplication, page: Page, root: string, workspace: string, repository: string, plain: string;
let env: Record<string, string>;
const binary = resolve(process.env.HAARNESS_BIN ?? "../../target/debug/haarness");
const shots = process.env.HAARNESS_SCREENSHOTS;
const errors: string[] = [];
const STOPPED = "::finish refusal partial words";
test.describe.configure({ mode: "serial" });

async function api(method: string, ...args: unknown[]): Promise<any> {
  return page.evaluate(({ connection, method, args }) => window.haarness.api(connection, method, args), {
    connection: "local:" + workspace,
    method,
    args,
  });
}

/** `target` in light and dark, and right to left in Arabic. */
async function pictures(name: string, target: Locator, rtl?: (target: Locator) => Promise<void>) {
  const settle = () => page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done))));
  const path = (suffix: string) =>
    shots ? join(shots, `${name}-${suffix}.png`) : test.info().outputPath(`${name}-${suffix}.png`);
  for (const theme of ["light", "dark"] as const) {
    await page.evaluate((theme) => window.haarness.preferences({ theme }), theme);
    await expect(page.locator("html")).toHaveAttribute("data-theme", theme);
    await page.waitForTimeout(400);
    await settle();
    await target.screenshot({ path: path(theme) });
  }
  if (rtl) {
    await page.evaluate(() => window.haarness.preferences({ theme: "light", language: "ar" }));
    await expect(page.locator("html")).toHaveAttribute("dir", "rtl");
    await rtl(target);
    await page.waitForTimeout(400);
    await settle();
    await target.screenshot({ path: path("rtl") });
    await page.evaluate(() => window.haarness.preferences({ language: "en" }));
    await expect(page.locator("html")).not.toHaveAttribute("dir", "rtl");
  }
  await page.evaluate(() => window.haarness.preferences({ theme: "light" }));
}

/** Sends `text` and waits for its turn to end: its reply, or its error. */
async function send(text: string, newChat = false) {
  if (newChat) await page.getByRole("button", { name: "New chat", exact: true }).click();
  const before = await page.locator(".message.user").count();
  const ends = text.startsWith("::finish")
    ? page.locator(".inline-error")
    : page.locator(".message.assistant").filter({ hasText: "Echo: " + text });
  const ended = await ends.count();
  const box = page.getByRole("textbox", { name: "Message", exact: true });
  await box.fill(text);
  await page.getByRole("button", { name: /Send message/ }).click();
  await expect(page.locator(".message.user")).toHaveCount(before + 1);
  await expect(ends).toHaveCount(ended + 1, { timeout: 30000 });
  await expect.poll(async () => (await api("activeTurns")).length, { timeout: 30000 }).toBe(0);
}

test.beforeAll(async () => {
  test.setTimeout(120000);
  root = await realpath(await mkdtemp(join(process.env.HAARNESS_E2E_DIR ?? tmpdir(), "haarness-safety-stop-")));
  workspace = repository = join(root, "project");
  plain = join(root, "plain-folder");
  for (const folder of [workspace, plain]) {
    await mkdir(join(folder, ".haarness"), { recursive: true });
    await writeFile(join(folder, ".haarness/config.toml"), '[defaults]\nprovider="echo"\nmodel="echo-1"\n');
  }
  // A repository, which Undo checkpoints; the plain folder has none.
  await writeFile(join(workspace, "README.md"), "fixture\n");
  for (const args of [
    ["init", "-q"],
    ["add", "."],
    ["-c", "user.name=Fixture", "-c", "user.email=fixture@example.test", "commit", "-q", "-m", "Fixture"],
  ])
    execFileSync("git", args, { cwd: workspace, stdio: "ignore" });
  if (shots) await mkdir(shots, { recursive: true });
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
  for (const folder of [repository, plain])
    try {
      execFileSync(binary, ["--workspace", folder, "serve", "--stop"], { env, stdio: "ignore", timeout: 30000 });
    } catch {}
  stopChatHome(binary, env);
  await test.info().attach("fixture", { body: root ?? "not started", contentType: "text/plain" });
});

test("a safety stop says its request is left out and offers Edit request, which puts it back in the box", async () => {
  await send("first question", true);
  await send(STOPPED);
  const row = page.locator(".inline-error");
  await expect(row).toHaveCount(1);
  await expect(row).toContainText(
    "Stopped by the provider's safety check (refusal). The model won't see this request again, so you can edit it or pick another model.",
  );
  // The chat still shows what streamed before the stop.
  await expect(page.locator(".message.assistant").filter({ hasText: "partial words" })).toHaveCount(1);
  const edit = row.getByRole("button", { name: "Edit request", exact: true });
  await expect(edit).toBeVisible();
  // The action sits on the row as Open providers does: theme tokens only.
  const [color, text] = await edit.evaluate((element) => {
    const probe = document.createElement("span");
    probe.style.color = "var(--text)";
    document.body.append(probe);
    const text = getComputedStyle(probe).color;
    probe.remove();
    return [getComputedStyle(element).color, text];
  });
  expect(color).toBe(text);
  await pictures("safety-stop-error-row", row, async (target) => {
    await expect(target.getByRole("button", { name: "تحرير الطلب", exact: true })).toBeVisible();
  });

  await edit.click();
  const box = page.getByRole("textbox", { name: "Message", exact: true });
  await expect(box).toHaveValue(STOPPED);
  await expect(page.locator(".inline-error")).toHaveCount(0);
  await expect(page.getByText("Turn undone. Sending a new message replaces the undone history.")).toBeVisible();
  // Reworded, it is a turn like any other, and the stopped one is gone.
  await box.fill("a reworded question");
  await page.getByRole("button", { name: /Send message/ }).click();
  await expect(page.locator(".message.assistant").last()).toContainText("Echo: a reworded question", { timeout: 30000 });
  await expect.poll(async () => (await api("activeTurns")).length, { timeout: 30000 }).toBe(0);
  await expect(page.locator(".inline-error")).toHaveCount(0);
  await expect(page.locator(".message.user")).toHaveText([/first question/, /a reworded question/]);
  expect(errors).toEqual([]);
});

test("only the stop that ended the latest turn offers it, and other errors do not", async () => {
  await send(STOPPED, true);
  const edit = page.getByRole("button", { name: "Edit request", exact: true });
  await expect(edit).toHaveCount(1);
  // Once another message is sent, Undo would take that turn back instead.
  await send("something else");
  await expect(page.locator(".inline-error")).toHaveCount(1);
  await expect(edit).toHaveCount(0);
  // A plain Stop is no failure, and offers nothing either.
  await page.getByRole("textbox", { name: "Message", exact: true }).fill("!linger 8000 slow task");
  await page.getByRole("button", { name: /Send message/ }).click();
  await page.getByRole("button", { name: "Stop response", exact: true }).click();
  await expect(page.locator(".turn-stopped")).toHaveCount(1);
  await expect(edit).toHaveCount(0);
  expect(errors).toEqual([]);
});

test("where Undo has no checkpoints, Edit request still puts the message back in the box", async () => {
  await app.evaluate(({ dialog }, path) => {
    dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [path] });
  }, plain);
  await page.getByRole("button", { name: "Open another project", exact: true }).click();
  await expect(page.locator(".titlebar")).toContainText("plain-folder");
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 60000 });
  workspace = plain;
  await send(STOPPED, true);
  const row = page.locator(".inline-error");
  await row.getByRole("button", { name: "Edit request", exact: true }).click();
  const box = page.getByRole("textbox", { name: "Message", exact: true });
  await expect(box).toHaveValue(STOPPED);
  // The stopped turn stays in view, and nothing says Undo failed.
  await expect(row).toHaveCount(1);
  await expect(page.getByText(/snapshots are disabled/i)).toHaveCount(0);
  await box.fill("reworded in a plain folder");
  await page.getByRole("button", { name: /Send message/ }).click();
  await expect(page.locator(".message.assistant").last()).toContainText("Echo: reworded in a plain folder", { timeout: 30000 });
  await expect.poll(async () => (await api("activeTurns")).length, { timeout: 30000 }).toBe(0);
  await expect(row.getByRole("button", { name: "Edit request", exact: true })).toHaveCount(0);
  expect(errors).toEqual([]);
});
