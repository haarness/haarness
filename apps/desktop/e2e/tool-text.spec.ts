import { test, expect, _electron as electron, type ElectronApplication, type Locator, type Page } from "@playwright/test";
import { mkdtemp, mkdir, readFile, writeFile, realpath } from "node:fs/promises";
import { execFileSync } from "node:child_process";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { stopChatHome } from "./fixtures/chat-home";
import { isolatedEnv } from "./fixtures/isolated-env";

// What a command or a server writes, as a person reads it: a row shows the
// text of a command's colours and window title, not their escape sequences,
// and an approval shows what is invisible in a command as quiet codes, in an
// RTL language too. Pictures are kept in HAARNESS_SCREENSHOTS when it is set.
let app: ElectronApplication, page: Page, root: string, project: string;
let env: Record<string, string>;
const binary = resolve(process.env.HAARNESS_BIN ?? "../../target/debug/haarness");
const shots = process.env.HAARNESS_SCREENSHOTS;
const errors: string[] = [];
test.describe.configure({ mode: "serial" });

async function api(method: string, ...args: unknown[]): Promise<any> {
  return page.evaluate(
    ({ connection, method, args }) => (window as any).haarness.api(connection, method, args),
    { connection: "local:" + project, method, args },
  );
}
async function send(text: string) {
  const box = page.getByRole("textbox", { name: "Message", exact: true });
  await box.fill(" " + text);
  await page.getByRole("button", { name: /Send message|Send guidance/ }).click();
}
async function idle() {
  await expect.poll(async () => (await api("activeTurns")).length, { timeout: 30000 }).toBe(0);
}
/** `target` light and dark, kept when HAARNESS_SCREENSHOTS names a folder. */
async function pictures(name: string, target: Locator) {
  for (const theme of ["light", "dark"] as const) {
    await page.evaluate((theme) => (window as any).haarness.preferences({ theme }), theme);
    await expect(page.locator("html")).toHaveAttribute("data-theme", theme);
    await page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done))));
    await target.screenshot({ path: shots ? join(shots, `${name}-${theme}.png`) : test.info().outputPath(`${name}-${theme}.png`) });
  }
  await page.evaluate(() => (window as any).haarness.preferences({ theme: "light" }));
}

test.beforeAll(async () => {
  test.setTimeout(120000);
  root = await realpath(await mkdtemp(join(process.env.HAARNESS_E2E_DIR ?? tmpdir(), "haarness-tool-text-")));
  project = join(root, "project");
  await mkdir(join(project, ".haarness"), { recursive: true });
  // Chats start in Auto; these approvals wait for a person, so the project asks for Review.
  await writeFile(
    join(project, ".haarness/config.toml"),
    '[defaults]\nprovider="echo"\nmodel="echo-1"\napproval_mode="review"\n[permissions]\nbash="ask"\n',
  );
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
  }, project);
  await page.getByRole("button", { name: "Open another project", exact: true }).click();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 60000 });
  await page.getByRole("button", { name: "New chat", exact: true }).click();
  await expect(page.getByRole("textbox", { name: "Message", exact: true })).toBeVisible();
});
test.afterAll(async () => {
  if (app) {
    await app.evaluate(({ BrowserWindow }) => {
      for (const window of BrowserWindow.getAllWindows()) window.destroy();
    }).catch(() => {});
    await app.close().catch(() => {});
  }
  try {
    execFileSync(binary, ["--workspace", project, "serve", "--stop"], { env, stdio: "ignore", timeout: 30000 });
  } catch {}
  stopChatHome(binary, env);
});

test("a command's colours and window title show as their text, and the model reads them as written", async () => {
  const command = "printf '\\033]0;window-title-proof\\007\\033[31mred\\033[0m plain\\n'";
  await send("!tool bash " + JSON.stringify({ command }));
  await page.getByRole("button", { name: "Allow once", exact: true }).click();
  await idle();
  const row = page.locator("details.tool-call").last();
  await row.locator("summary").click();
  const output = row.locator(".tool-output");
  await expect(output).toHaveText("red plain");
  for (const remains of ["[31m", "[0m", "]0;", "window-title-proof", "\u001b"])
    expect(await output.textContent()).not.toContain(remains);
  const [thread] = await api("listThreads");
  const items = await api("listItems", thread.id);
  const result = items.find((item: any) => item.data.type === "tool_result");
  expect(result.data.output).toContain("\u001b]0;window-title-proof\u0007\u001b[31mred\u001b[0m plain");
  await pictures("tool-output", row);
  expect(errors).toEqual([]);
});

test("an approval shows what is invisible in a command as quiet codes, right to left too", async () => {
  const command = "echo report\u202efdp.sh\u200b && printf done\r\nprintf more > ran.txt";
  await send("!tool bash " + JSON.stringify({ command }));
  const card = page.locator(".prompt-dock.approval");
  const subject = card.locator(".approval-command");
  const shown = "echo report\\u{202E}fdp.sh\\u{200B} && printf done\\u{D}\nprintf more > ran.txt";
  await expect(subject).toHaveText(shown);
  // Nothing in the card can reorder or hide a part of the command.
  expect(await subject.textContent()).not.toMatch(/[\u202e\u200b\r]/);
  await expect(subject.locator(".literal-code")).toHaveText(["\\u{202E}", "\\u{200B}", "\\u{D}"]);
  // Quieter than the command around them, in the theme's own muted color.
  const colors = await subject.evaluate((element) => {
    const probe = document.createElement("span");
    probe.style.color = "var(--muted)";
    element.append(probe);
    const muted = getComputedStyle(probe).color;
    probe.remove();
    return {
      muted,
      code: getComputedStyle(element.querySelector(".literal-code")!).color,
      text: getComputedStyle(element).color,
    };
  });
  expect(colors.code).toBe(colors.muted);
  expect(colors.code).not.toBe(colors.text);
  await pictures("approval-codes", card);
  await page.evaluate(() => (window as any).haarness.preferences({ language: "ar" }));
  try {
    await expect(page.locator("html")).toHaveAttribute("dir", "rtl");
    await expect(subject).toHaveText(shown);
    await pictures("approval-codes-rtl", card);
  } finally {
    await page.evaluate(() => (window as any).haarness.preferences({ language: "en" }));
  }
  // The call's row waiting on the card shows the same codes, on one line.
  await expect(page.locator(".tool-target").last()).toHaveText(
    "echo report\\u{202E}fdp.sh\\u{200B} && printf done\\u{D} printf more > ran.txt",
  );
  await pictures("approval-row", page.locator("details.tool-call").last());
  // The answer is the request's own: denied as asked, nothing ran.
  await page.getByRole("button", { name: "Deny", exact: true }).click();
  await idle();
  await expect(readFile(join(project, "ran.txt"), "utf8")).rejects.toThrow();
  const [thread] = await api("listThreads");
  const items = await api("listItems", thread.id);
  const call = items.filter((item: any) => item.data.type === "tool_call").at(-1);
  expect(call.data.arguments.command).toBe(command);
  expect(errors).toEqual([]);
});
