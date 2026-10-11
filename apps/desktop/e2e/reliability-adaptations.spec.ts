import { test, expect, _electron as electron } from "@playwright/test";
import { Haarness } from "@haarness/sdk";
import { mkdtemp, mkdir, writeFile, readFile, realpath, rename, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";

test("review drafts survive navigation, reload and failed delivery; recall prioritizes conversations", async ({}, info) => {
  test.setTimeout(180000);
  const temporary = await realpath(await mkdtemp(join(tmpdir(), "haarness-reliability-")));
  const workspace = join(temporary, "workspace");
  await mkdir(join(workspace, ".haarness"), { recursive: true });
  await writeFile(join(workspace, ".haarness/config.toml"), '[defaults]\nprovider="echo"\nmodel="echo-1"\n');
  await writeFile(join(workspace, "sample.txt"), "before\n");
  for (const args of [["init", "-b", "main"], ["add", "."], ["-c", "user.name=Fixture", "-c", "user.email=fixture@example.test", "commit", "-m", "Initial fixture"]])
    execFileSync("git", args, { cwd: workspace, stdio: "ignore" });
  await writeFile(join(workspace, "sample.txt"), "after\n");
  const binary = resolve(process.env.HAARNESS_BIN ?? "../../target/debug/haarness");
  const env: Record<string, string> = { ...process.env, HAARNESS_BIN: binary, HAARNESS_HOME: join(temporary, "home"), HAARNESS_DATA_DIR: join(temporary, "data"), HAARNESS_DESKTOP_DATA: join(temporary, "desktop"), HAARNESS_TEST_PROVIDER: "1", HAARNESS_DESKTOP_TEST: "1" };
  delete env.ELECTRON_RUN_AS_NODE;
  delete env.ELECTRON_RENDERER_URL;
  const app = await electron.launch({ args: ["."], env, timeout: 60000 });
  const page = await app.firstWindow();
  const errors: string[] = [];
  page.on("pageerror", error => errors.push(error.message));
  let client: Haarness | undefined;
  try {
    await page.setViewportSize({ width: 1500, height: 950 });
    await app.evaluate(({ dialog }, directory) => { dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [directory] }); }, workspace);
    await page.getByRole("region", { name: "Project home" }).getByRole("button", { name: "Open project", exact: true }).click();
    await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 60000 });
    const connection = JSON.parse(execFileSync(binary, ["--workspace", workspace, "--trust-project", "desktop"], { env, input: '{"action":"connect"}', encoding: "utf8", timeout: 60000 }));
    client = new Haarness({ baseUrl: connection.url, token: connection.token });
    const first = await client.createThread({ title: "Draft fixture first", provider: "echo", model: "echo-1" });
    const second = await client.createThread({ title: "Draft fixture second", provider: "echo", model: "echo-1" });
    const choose = async (title: string) => {
      await page.locator(".sessions").getByRole("button", { name: title, exact: true }).click();
      await expect(page.locator(".session-bar")).toContainText(title);
    };
    await choose(first.title);
    const show = page.getByRole("button", { name: "Show side panel", exact: true });
    if (await show.isVisible()) await show.click();
    await page.getByRole("button", { name: "Open panel", exact: true }).click();
    await page.getByRole("region", { name: "Open a project panel" }).getByRole("button", { name: "Open Review", exact: true }).click();
    const line = page.getByRole("button", { name: "Comment on updated line 1", exact: true });
    const comment = page.getByRole("textbox", { name: "Review comment", exact: true });
    await line.click();
    await comment.fill("Keep this comment through navigation.");
    await choose(second.title);
    await line.click();
    await expect(comment).toHaveValue("");
    await choose(first.title);
    await line.click();
    await expect(comment).toHaveValue("Keep this comment through navigation.");
    await page.reload();
    await line.click();
    await expect(comment).toHaveValue("Keep this comment through navigation.");
    await page.screenshot({ path: info.outputPath("restored-review-draft.png") });

    // Real IPC + filesystem failure after a durable source draft. Restore the
    // filesystem, then retry through the visible Add to message control.
    const delivery = await page.evaluate(async thread => {
      const connection = await window.haarness.storage("active-project") as string;
      const scope = JSON.stringify([connection, thread, "working"]);
      const saved = await window.haarness.review({ action: "load", scope });
      const [anchor, draft] = Object.entries(saved.drafts)[0];
      draft.target = { connection, thread };
      await window.haarness.review({ action: "pin", scope, anchor, draft });
      return { action: "deliver" as const, scope, anchor, draft, note: `Review comment on sample.txt:1 (updated):\n${draft.text}`, current: "" };
    }, first.id);
    const settings = join(temporary, "desktop", "desktop.json");
    await rename(settings, settings + ".saved");
    await mkdir(settings);
    const failure = await page.evaluate(async request => {
      try { await window.haarness.review(request); return "unexpected success"; }
      catch (error) { return String(error); }
    }, delivery);
    expect(failure).not.toBe("unexpected success");
    await rm(settings, { recursive: true });
    await rename(settings + ".saved", settings);
    await page.getByRole("button", { name: "Add to message", exact: true }).click();
    const message = page.getByRole("textbox", { name: "Message", exact: true });
    await expect(message).toHaveValue(delivery.note);
    await page.reload();
    await expect(message).toHaveValue(delivery.note);
    const disk = JSON.parse(await readFile(settings, "utf8"));
    expect(disk.values["draft:" + delivery.draft.target!.connection + ":" + first.id]).toBe(delivery.note);
    await page.getByRole("button", { name: "Send message", exact: true }).click();
    // Replies keep their line breaks as <br>, which only rendered text shows.
    await expect(page.locator(".transcript")).toContainText("Echo: " + delivery.note, { useInnerText: true });
    await expect.poll(async () => (await client!.activeTurns()).length).toBe(0);
    await choose(second.title);
    await expect(message).toHaveValue("");

    const send = async (text: string) => {
      await message.fill(text);
      await page.getByRole("button", { name: "Send message", exact: true }).click();
      await expect(page.locator(".transcript")).toContainText("Echo: " + text);
      await expect.poll(async () => (await client!.activeTurns()).length).toBe(0);
    };
    await choose(first.title);
    await send("first conversation recall");
    await choose(second.title);
    await send("second conversation recall");
    await choose(first.title);
    await message.fill("unfinished draft");
    await message.evaluate(element => (element as HTMLTextAreaElement).setSelectionRange(0, 0));
    await message.press("ArrowUp");
    await expect(message).toHaveValue("unfinished draft");
    await page.reload();
    await expect(message).toHaveValue("unfinished draft");
    // Empty drafts retain conversation-first ordering across the workspace.
    await message.fill("");
    await message.press("ArrowUp");
    await expect(message).toHaveValue("first conversation recall");
    await message.press("ArrowUp");
    await expect(message).toHaveValue(delivery.note);
    await message.press("ArrowUp");
    await expect(message).toHaveValue("second conversation recall");
    await message.press("ArrowDown");
    await expect(message).toHaveValue(delivery.note);
    await message.press("ArrowDown");
    await expect(message).toHaveValue("first conversation recall");
    await message.press("ArrowDown");
    await expect(message).toHaveValue("");
    await send("git status");
    await send("npm test");
    await send("git diff");
    await message.fill("git ");
    await message.evaluate(element => (element as HTMLTextAreaElement).setSelectionRange(0, 0));
    await message.press("ArrowUp");
    await expect(message).toHaveValue("git diff");
    await message.press("ArrowUp");
    await expect(message).toHaveValue("git status");
    await message.press("ArrowUp");
    await expect(message).toHaveValue("git status");
    await message.press("ArrowDown");
    await expect(message).toHaveValue("git diff");
    await message.press("ArrowDown");
    await expect(message).toHaveValue("git ");
    // Editing a recall starts a fresh prefix without losing the edited text.
    await message.evaluate(element => (element as HTMLTextAreaElement).setSelectionRange(0, 0));
    await message.press("ArrowUp");
    await message.fill("git diff --stat");
    await message.evaluate(element => (element as HTMLTextAreaElement).setSelectionRange(0, 0));
    await message.press("ArrowUp");
    await expect(message).toHaveValue("git diff --stat");
    await page.screenshot({ path: info.outputPath("conversation-history.png") });
    expect(errors).toEqual([]);
  } finally {
    client?.close();
    await app.close().catch(() => {});
    try { execFileSync(binary, ["--workspace", workspace, "serve", "--stop"], { env, stdio: "ignore", timeout: 30000 }); } catch {}
    await info.attach("fixture", { body: temporary, contentType: "text/plain" });
  }
});
