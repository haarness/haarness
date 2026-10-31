import { test, expect, _electron as electron } from "@playwright/test";
import { mkdtemp, mkdir, writeFile, realpath } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";
import { stopChatHome } from "./fixtures/chat-home";

// Opt-in: real GitHub Copilot models through the Desktop picker and chat.
// HAARNESS_COPILOT_E2E=1 GITHUB_COPILOT_TOKEN=… (a token of an account with
// Copilot). HAARNESS_COPILOT_E2E_MODELS picks the models, one per API family
// by default, so a run costs a few premium requests.
const live = process.env.HAARNESS_COPILOT_E2E === "1" && !!process.env.GITHUB_COPILOT_TOKEN;

test("Copilot models answer with tools from the Desktop picker", async ({}, info) => {
  test.skip(!live, "set HAARNESS_COPILOT_E2E=1 and GITHUB_COPILOT_TOKEN");
  test.setTimeout(600000);
  const models = (process.env.HAARNESS_COPILOT_E2E_MODELS ?? "gpt-6-luna,claude-haiku-5.5,grok-4.7").split(",");
  const root = await realpath(await mkdtemp(join(tmpdir(), "haarness-copilot-live-")));
  const workspace = join(root, "workspace");
  await mkdir(join(workspace, ".haarness"), { recursive: true });
  await writeFile(join(workspace, ".haarness/config.toml"), `[defaults]\nprovider="github-copilot"\nmodel="${models[0]}"\n[permissions]\nread="allow"\n[lsp]\nenabled=false\n`);
  await writeFile(join(workspace, "notes.txt"), "code word: lantern-417\n");
  execFileSync("git", ["init", "-q", workspace]);
  const binary = resolve(process.env.HAARNESS_BIN ?? "../../target/debug/haarness");
  const env: Record<string, string> = {
    ...(process.env as Record<string, string>), HAARNESS_BIN: binary, HAARNESS_HOME: join(root, "home"),
    HAARNESS_DATA_DIR: join(root, "data"), HAARNESS_DESKTOP_DATA: join(root, "desktop"), HAARNESS_DESKTOP_TEST: "1",
  };
  delete env.ELECTRON_RUN_AS_NODE;
  delete env.ELECTRON_RENDERER_URL;
  const app = await electron.launch({ args: ["."], env, timeout: 60000 });
  const page = await app.firstWindow();
  const errors: string[] = [];
  page.on("pageerror", error => errors.push(error.message));
  try {
    await page.setViewportSize({ width: 1400, height: 950 });
    await app.evaluate(({ dialog }, directory) => { dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [directory] }); }, workspace);
    await page.getByRole("button", { name: "Open another project", exact: true }).click();
    await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 60000 });
    const model = page.getByRole("combobox", { name: "Model", exact: true });
    for (const id of models) {
      await model.click();
      const list = page.locator(`[id=${JSON.stringify(await model.getAttribute("aria-controls"))}]`);
      await list.locator(`[role="option"][data-value=${JSON.stringify(`github-copilot/${id}`)}]`).click();
      const slider = page.getByRole("slider", { name: "Reasoning effort", exact: true });
      if (await slider.count()) {
        await page.screenshot({ animations: "disabled", path: info.outputPath(`picker-${id}.png`) });
        await slider.focus();
        await slider.press("Home");
        await slider.press("ArrowRight");
      }
      const done = page.locator(".select-footer").getByRole("button", { name: "Done", exact: true });
      if (await done.isVisible()) await done.click();
      await page.getByRole("textbox", { name: "Message", exact: true }).fill("Read notes.txt with your file tool, then reply with only the code word.");
      await page.getByRole("button", { name: "Send message", exact: true }).click();
      await expect(page.locator(".message.assistant").filter({ hasText: "lantern-417" }).last()).toBeVisible({ timeout: 180000 });
      await expect(page.getByRole("button", { name: "Stop", exact: true })).toHaveCount(0, { timeout: 180000 });
      await page.screenshot({ animations: "disabled", path: info.outputPath(`chat-${id}.png`) });
      await page.getByRole("button", { name: "New chat", exact: true }).click();
    }
    expect(errors).toEqual([]);
  } finally {
    await app.close();
    stopChatHome(binary, env);
  }
});
