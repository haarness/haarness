import { test, expect, _electron as electron, type ElectronApplication, type Locator, type Page } from "@playwright/test";
import { mkdtemp, mkdir, readFile, writeFile, realpath } from "node:fs/promises";
import { execFileSync } from "node:child_process";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { stopChatHome } from "./fixtures/chat-home";
import { isolatedEnv } from "./fixtures/isolated-env";

// What a command or a server writes, as someone reading the chat sees it: a
// row shows the text of a command's colours and window title, not their
// escape sequences, and an approval shows what is invisible in a command as
// quiet codes, in an RTL language too, and in the lines an edit changes.
// Pictures are kept in HAARNESS_SCREENSHOTS when it is set.
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

/** How `element`'s first code is set apart: on a chip of the theme's word
 * tint, as one left-to-right isolate. */
async function chip(element: Locator) {
  return element.evaluate((element) => {
    const code = getComputedStyle(element.querySelector(".spelled-code")!);
    const probe = document.createElement("span");
    probe.style.background = "var(--word-change)";
    element.append(probe);
    const tint = getComputedStyle(probe).backgroundColor;
    probe.remove();
    return { tinted: code.backgroundColor === tint, bidi: code.unicodeBidi, direction: code.direction };
  });
}
const CHIP = { tinted: true, bidi: "isolate", direction: "ltr" };

test.beforeAll(async () => {
  test.setTimeout(120000);
  root = await realpath(await mkdtemp(join(process.env.HAARNESS_E2E_DIR ?? tmpdir(), "haarness-tool-text-")));
  project = join(root, "project");
  await mkdir(join(project, ".haarness"), { recursive: true });
  // Chats start in Auto; these approvals wait for a person, so the project asks for Review.
  await writeFile(
    join(project, ".haarness/config.toml"),
    '[defaults]\nprovider="echo"\nmodel="echo-1"\napproval_mode="review"\n[permissions]\nbash="ask"\nedit="ask"\n',
  );
  await writeFile(join(project, "access.js"), 'if (role != "user") {\n  allow();\n}\n');
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
  await expect(subject.locator(".spelled-code")).toHaveText(["\\u{202E}", "\\u{200B}", "\\u{D}"]);
  // Quieter than the command around them, in the theme's own muted color.
  const colors = await subject.evaluate((element) => {
    const probe = document.createElement("span");
    probe.style.color = "var(--muted)";
    element.append(probe);
    const muted = getComputedStyle(probe).color;
    probe.remove();
    return {
      muted,
      code: getComputedStyle(element.querySelector(".spelled-code")!).color,
      text: getComputedStyle(element).color,
    };
  });
  expect(colors.code).toBe(colors.muted);
  expect(colors.code).not.toBe(colors.text);
  expect(await chip(subject)).toEqual(CHIP);
  await pictures("approval-codes", card);
  await page.evaluate(() => (window as any).haarness.preferences({ language: "ar" }));
  try {
    await expect(page.locator("html")).toHaveAttribute("dir", "rtl");
    await expect(subject).toHaveText(shown);
    await pictures("approval-codes-rtl", card);
  } finally {
    await page.evaluate(() => (window as any).haarness.preferences({ language: "en" }));
  }
  // The call's row waiting on the card shows the same codes, on one line,
  // set apart as on the card and with nothing added to what a copy takes.
  const target = page.locator(".tool-target").last();
  await expect(target).toHaveText("echo report\\u{202E}fdp.sh\\u{200B} && printf done\\u{D} printf more > ran.txt");
  await expect(target.locator(".spelled-code")).toHaveText(["\\u{202E}", "\\u{200B}", "\\u{D}"]);
  expect(await target.textContent()).not.toMatch(/[\u2066-\u2069]/);
  expect(await chip(target)).toEqual(CHIP);
  await pictures("approval-row", page.locator("details.tool-call").last());
  // The answer is the request's own: denied as asked, nothing ran.
  await page.getByRole("button", { name: "Deny", exact: true }).click();
  await idle();
  await expect(readFile(join(project, "ran.txt"), "utf8")).rejects.toThrow();
  // Opened, its Shell panel draws the command the same way.
  const denied = page.locator("details.tool-call").last();
  await denied.locator("summary").click();
  const input = denied.locator(".tool-input");
  await expect(input).toHaveText("$ " + shown);
  await expect(input.locator(".spelled-code")).toHaveText(["\\u{202E}", "\\u{200B}", "\\u{D}"]);
  expect(await chip(input)).toEqual(CHIP);
  // In the text color, on its chip: muted over the panel would read too faint.
  const legible = await input.evaluate((element) => {
    const probe = document.createElement("span");
    probe.style.color = "var(--text)";
    element.append(probe);
    const color = getComputedStyle(probe).color;
    probe.remove();
    return getComputedStyle(element.querySelector(".spelled-code")!).color === color;
  });
  expect(legible).toBe(true);
  await pictures("shell-panel", denied);
  const [thread] = await api("listThreads");
  const items = await api("listItems", thread.id);
  const call = items.filter((item: any) => item.data.type === "tool_call").at(-1);
  expect(call.data.arguments.command).toBe(command);
  expect(errors).toEqual([]);
});

test("an edit's card shows what turns a changed line around as a quiet code, and the file's own text as written", async () => {
  const line = 'if (role != "user\u202e \u2066// admin\u2069 \u2066") {';
  await send(
    "!tool edit " +
      JSON.stringify({ path: "access.js", old: 'if (role != "user") {', new: line + '\n  const greeting = "مرحبا\u200f";' }),
  );
  const card = page.locator(".prompt-dock.approval");
  const changes = card.locator(".approval-changes");
  await expect(changes).toContainText('if (role != "user\\u{202E} \\u{2066}// admin\\u{2069} \\u{2066}") {');
  await expect(changes.locator(".spelled-code")).toHaveText(["\\u{202E}", "\\u{2066}", "\\u{2069}", "\\u{2066}"]);
  // Right-to-left text and its mark belong to the file: no code there.
  await expect(changes).toContainText('const greeting = "مرحبا\u200f";');
  expect(await changes.textContent()).not.toMatch(/[\u202a-\u202e\u2066-\u2069]/);
  // In a changed line a code keeps the line's color, on its chip.
  expect(await chip(changes.locator(".diff-line.add").first())).toEqual(CHIP);
  await pictures("approval-edit", card);
  await page.getByRole("button", { name: "Deny", exact: true }).click();
  await idle();
  expect(await readFile(join(project, "access.js"), "utf8")).toBe('if (role != "user") {\n  allow();\n}\n');
  expect(errors).toEqual([]);
});

test("a right-to-left name keeps its code in one piece on its row and in its panel", async () => {
  const name = "ملف\u200f.txt";
  await writeFile(join(project, name), "سطر\n");
  await send("!tool read " + JSON.stringify({ path: name }));
  await idle();
  // Opened once its result is in (a row drawn again without it starts
  // closed), and before the language changes.
  const row = page.locator("details.tool-call").filter({ hasText: "200F" }).last();
  await expect(row).toHaveAttribute("data-status", "done");
  await row.locator("summary").click();
  const input = row.locator(".tool-input");
  await expect(input.locator(".spelled-code")).toHaveText("\\u{200F}");
  await page.evaluate(() => (window as any).haarness.preferences({ language: "ar" }));
  try {
    await expect(page.locator("html")).toHaveAttribute("dir", "rtl");
    await expect(row.locator(".tool-target")).toHaveText("ملف\\u{200F}.txt");
    await expect(input.locator(".spelled-code")).toHaveText("\\u{200F}");
    expect(await chip(input)).toEqual(CHIP);
    // The code reads left to right as written, after the name, not torn
    // around it.
    const order = await row.locator(".tool-target .spelled-code").evaluate((element) => {
      const range = document.createRange();
      const text = element.firstChild!;
      const at = (index: number) => {
        range.setStart(text, index);
        range.setEnd(text, index + 1);
        return range.getBoundingClientRect().left;
      };
      const content = text.textContent!;
      const backslash = content.indexOf("\\");
      return { backslash: at(backslash), brace: at(content.indexOf("}")) };
    });
    expect(order.backslash).toBeLessThan(order.brace);
    await pictures("rtl-target", row);
  } finally {
    await page.evaluate(() => (window as any).haarness.preferences({ language: "en" }));
  }
  expect(errors).toEqual([]);
});

test("a script a command runs, and each file of a patch, are named with their codes on the card", async () => {
  const script = "run\u200b.sh";
  await writeFile(join(project, script), "echo hi\n");
  await send("!tool bash " + JSON.stringify({ command: "sh " + script }));
  const card = page.locator(".prompt-dock.approval");
  await card.locator(".approval-script-toggle").click();
  const path = card.locator(".approval-script-body .change-path");
  await expect(path).toHaveText("run\\u{200B}.sh");
  expect(await chip(path)).toEqual(CHIP);
  await page.getByRole("button", { name: "Deny", exact: true }).click();
  await idle();
  const patch = "*** Begin Patch\n*** Add File: a\u200b.txt\n+one\n*** Add File: b.txt\n+two\n*** End Patch\n";
  await send("!tool apply_patch " + JSON.stringify({ input: patch }));
  const headers = card.locator(".approval-changes .change-path");
  await expect(headers).toHaveText(["a\\u{200B}.txt", "b.txt"]);
  expect(await chip(headers.first())).toEqual(CHIP);
  await pictures("approval-files", card);
  await page.getByRole("button", { name: "Deny", exact: true }).click();
  await idle();
  expect(errors).toEqual([]);
});
