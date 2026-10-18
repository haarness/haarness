import { test, expect, _electron as electron, type ElectronApplication, type Page } from "@playwright/test";
import { mkdtemp, mkdir, writeFile, realpath, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";

// A selection in a Markdown file's rendered Preview names the source lines it
// came from: "Add selection to context" (and its shortcut) adds those lines,
// and a comment on them adds the same lines plus the comment to the message.
test.describe.configure({ mode: "serial" });

let app: ElectronApplication, page: Page;
let temporary: string, workspace: string, binary: string, env: Record<string, string>;
const errors: string[] = [];

const GUIDE = [
  "# Guide", //                          1
  "", //                                 2
  "Intro paragraph with *emphasis*", //  3
  "spans two lines.", //                 4
  "", //                                 5
  "- first item", //                     6
  "- second item", //                    7
  "  - nested one", //                   8
  "  - nested two", //                   9
  "- third item", //                     10
  "", //                                 11
  "| a | b |", //                        12
  "| - | - |", //                        13
  "| 1 | 2 |", //                        14
  "", //                                 15
  "```ts", //                            16
  "const a = 1;", //                     17
  "const b = 2;", //                     18
  "```", //                              19
  "",
].join("\n");

const button = (name: string) => page.getByRole("button", { name, exact: true });
const context = () => page.getByLabel("Attached context", { exact: true });
const preview = () => page.locator(".file-preview .document-preview > .markdown");

/** Selects rendered text in the Preview, from one phrase to another (or within one). */
async function select(from: string, to = from, end = true) {
  await preview().evaluate(
    (host, [from, to, end]) => {
      const walker = document.createTreeWalker(host, NodeFilter.SHOW_TEXT);
      const texts: Text[] = [];
      for (let node = walker.nextNode(); node; node = walker.nextNode()) texts.push(node as Text);
      const start = texts.find((text) => text.data.includes(from as string))!;
      const finish = texts.find((text) => text.data.includes(to as string) && (text === start || start.compareDocumentPosition(text) & Node.DOCUMENT_POSITION_FOLLOWING))!;
      const range = document.createRange();
      range.setStart(start, start.data.indexOf(from as string));
      range.setEnd(finish, finish.data.indexOf(to as string) + (end ? (to as string).length : 0));
      const selection = document.getSelection()!;
      selection.removeAllRanges();
      selection.addRange(range);
    },
    [from, to, end] as const,
  );
}

async function showFiles() {
  if (await button("Show side panel").isVisible()) await button("Show side panel").click();
  if (await button("Show files panel").isVisible()) await button("Show files panel").click();
  else if (!(await page.locator(".explorer").isVisible())) {
    await button("Open panel").click();
    await button("Open Files").click();
  }
  await expect(page.locator(".explorer")).toBeVisible();
}

test.beforeAll(async () => {
  test.setTimeout(240000);
  temporary = await realpath(await mkdtemp(join(process.env.HAARNESS_E2E_DIR ?? tmpdir(), "haarness-preview-")));
  workspace = join(temporary, "workspace");
  await mkdir(join(workspace, ".haarness"), { recursive: true });
  await mkdir(join(workspace, "docs"));
  await writeFile(join(workspace, ".haarness/config.toml"), '[defaults]\nprovider="echo"\nmodel="echo-1"\n');
  await writeFile(join(workspace, "docs/guide.md"), GUIDE);
  binary = resolve(process.env.HAARNESS_BIN ?? "../../target/debug/haarness");
  env = {
    ...(process.env as Record<string, string>),
    HAARNESS_BIN: binary,
    HAARNESS_HOME: join(temporary, "home"),
    HAARNESS_DATA_DIR: join(temporary, "data"),
    HAARNESS_DESKTOP_DATA: join(temporary, "desktop"),
    HAARNESS_TEST_PROVIDER: "1",
    HAARNESS_DESKTOP_TEST: "1",
  };
  delete env.ELECTRON_RUN_AS_NODE;
  delete env.ELECTRON_RENDERER_URL;
  app = await electron.launch({ args: ["."], env, timeout: 60000 });
  page = await app.firstWindow();
  page.on("pageerror", (error) => errors.push(error.message));
  await page.setViewportSize({ width: 1500, height: 950 });
  await app.evaluate(({ dialog }, directory) => {
    dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [directory] });
  }, workspace);
  await page.getByRole("region", { name: "Project home" }).getByRole("button", { name: "Open project", exact: true }).click();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 90000 });
});

test.afterAll(async () => {
  await app?.close();
  if (workspace)
    try {
      execFileSync(binary, ["--workspace", workspace, "--trust-project", "serve", "--stop"], { env, stdio: "ignore", timeout: 30_000 });
    } catch {}
  if (temporary) await rm(temporary, { recursive: true, force: true }).catch(() => {});
});

test("a selection in the Preview adds the source lines it came from", async ({}, info) => {
  await showFiles();
  await page.getByRole("textbox", { name: "Find files", exact: true }).fill("guide.md");
  await page.locator(".file-entry").filter({ hasText: "docs/guide.md" }).click();
  await page.locator(".file-actions").getByRole("button", { name: "Preview", exact: true }).click();
  await expect(preview()).toHaveAttribute("data-markdown-ready", "true");
  // Without a selection the whole file goes in, as before.
  await expect(button("Add file to context")).toBeVisible();
  await expect(button("Comment on selection")).toBeDisabled();

  // A nested list item is its own line.
  await select("nested two");
  await expect(button("Add selection to context")).toBeEnabled();
  await button("Add selection to context").click();
  await expect(context()).toContainText("docs/guide.md:9");

  // A phrase on one line of a paragraph is that line; the shortcut adds it too.
  await showFiles();
  await select("two lines");
  await page.keyboard.press(process.platform === "darwin" ? "Meta+Shift+L" : "Control+Shift+L");
  await expect(context()).toContainText("docs/guide.md:4");

  // A line of a fenced code block is that line.
  await showFiles();
  await select("const b");
  await button("Add selection to context").click();
  await expect(context()).toContainText("docs/guide.md:18");

  // Across blocks: from the heading's line to the table row's.
  await showFiles();
  await select("Guide", "1");
  await button("Add selection to context").click();
  await expect(context()).toContainText("docs/guide.md:1-14");
  await expect(context()).not.toContainText("docs/guide.md ·");
  await context().screenshot({ path: info.outputPath("preview-context.png") });
  expect(errors).toEqual([]);
});

test("a comment on Preview lines adds the lines and the comment to the message", async ({}, info) => {
  await showFiles();
  await select("second item");
  await button("Comment on selection").click();
  const form = page.getByRole("form", { name: "Comment on docs/guide.md:7" });
  await expect(form).toBeVisible();
  const box = form.getByRole("textbox", { name: "Comment", exact: true });
  await expect(box).toBeFocused();
  await box.fill("Say why this item comes second.");
  await page.locator(".explorer").screenshot({ path: info.outputPath("preview-comment.png") });
  await box.press("Enter");
  await expect(form).toHaveCount(0);
  await expect(page.locator(".notifications .toast")).toHaveText("Comment added to your message");
  await expect(context()).toContainText("docs/guide.md:7");
  const message = page.getByRole("textbox", { name: "Message", exact: true });
  await expect(message).toHaveValue("Comment on docs/guide.md:7:\nSay why this item comes second.");

  // Escape closes a comment without adding anything.
  await showFiles();
  await select("first item");
  await button("Comment on selection").click();
  await page.getByRole("form", { name: "Comment on docs/guide.md:6" }).getByRole("textbox", { name: "Comment", exact: true }).press("Escape");
  await expect(page.getByRole("form", { name: "Comment on docs/guide.md:6" })).toHaveCount(0);
  await expect(message).toHaveValue("Comment on docs/guide.md:7:\nSay why this item comes second.");
  expect(errors).toEqual([]);
});
