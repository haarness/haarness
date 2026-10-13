import { test, expect, _electron as electron } from "@playwright/test";
import { Haarness } from "@haarness/sdk";
import { mkdtemp, mkdir, writeFile, realpath } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";
import { createServer } from "node:http";

test("agent switch discloses model and scope; filtered responses stay visible", async ({}, info) => {
  test.setTimeout(150000);
  let blocked = false;
  const server = createServer(async (request, response) => {
    for await (const _ of request) { /* consume the request */ }
    if (request.method === "GET") {
      response.setHeader("Content-Type", "application/json");
      response.end(JSON.stringify({ data: [{ id: "gpt-5.4-mini" }] }));
      return;
    }
    response.setHeader("Content-Type", "text/event-stream");
    response.write(`data: ${JSON.stringify({ type: "response.output_text.delta", delta: blocked ? "PARTIAL_DESKTOP_PROOF" : "HANDOFF_DESKTOP_READY" })}\n\n`);
    response.end(`data: ${JSON.stringify({ type: blocked ? "response.incomplete" : "response.completed", response: {
      usage: { input_tokens: 10, output_tokens: 4 }, incomplete_details: blocked ? { reason: "content_filter" } : null,
    } })}\n\n`);
  });
  await new Promise<void>(resolve => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("Missing fixture address");
  const root = await realpath(await mkdtemp(join(tmpdir(), "haarness-provider-ui-")));
  const workspace = join(root, "workspace");
  await mkdir(join(workspace, ".haarness/agents"), { recursive: true });
  await writeFile(join(workspace, ".haarness/config.toml"), '[defaults]\nprovider="echo"\nmodel="echo-1"\n[lsp]\nenabled=false\n');
  await writeFile(join(workspace, ".haarness/agents/reviewer.md"), '---\ndescription: Fixture reviewer\nmode: primary\nmodel: openai/gpt-5.4-mini\n---\nReply briefly.\n');
  execFileSync("git", ["init", "-q", workspace]);
  const binary = resolve(process.env.HAARNESS_BIN ?? "../../target/debug/haarness");
  const env: Record<string, string> = {
    ...process.env, HAARNESS_BIN: binary, HAARNESS_HOME: join(root, "home"), HAARNESS_DATA_DIR: join(root, "data"),
    HAARNESS_DESKTOP_DATA: join(root, "desktop"), HAARNESS_TEST_PROVIDER: "1", HAARNESS_DESKTOP_TEST: "1",
    OPENAI_API_KEY: "fixture-key", OPENAI_BASE_URL: `http://127.0.0.1:${address.port}/v1`,
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
    await page.getByRole("region", { name: "Project home" }).getByRole("button", { name: "Open project", exact: true }).click();
    await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 60000 });
    const connection = JSON.parse(execFileSync(binary, ["--workspace", workspace, "--trust-project", "desktop"], { env, input: '{"action":"connect"}', encoding: "utf8", timeout: 60000 }));
    const client = new Haarness({ baseUrl: connection.url, token: connection.token });
    const thread = await client.createThread({ title: "Switch consent proof", provider: "echo", model: "echo-1" });
    await page.locator(".sessions").getByRole("button", { name: thread.title, exact: true }).click();
    const box = page.getByRole("textbox", { name: "Message", exact: true });
    await box.fill(' !tool agent_switch {"agent":"reviewer","reason":"Review these changes"}');
    await page.getByRole("button", { name: /Send message|Send guidance/ }).click();
    const permission = page.getByRole("alert", { name: "Permission request" });
    await expect(permission).toContainText("Agent: reviewer");
    await expect(permission).toContainText("Model: openai/gpt-5.4-mini");
    await expect(permission).toContainText("Previous model: echo/echo-1");
    await expect(permission).toContainText("only to this agent in this chat");
    await page.screenshot({ path: info.outputPath("agent-switch-consent.png") });
    await permission.getByRole("button", { name: "Allow once", exact: true }).click();
    await expect(page.locator(".transcript")).toContainText("HANDOFF_DESKTOP_READY", { timeout: 30000 });
    await expect.poll(async () => (await client.activeTurns()).length).toBe(0);
    blocked = true;
    await box.fill("Show the filtered response fixture");
    await page.getByRole("button", { name: /Send message|Send guidance/ }).click();
    await expect(page.locator(".transcript")).toContainText("safety check stopped", { timeout: 30000 });
    await expect(page.locator(".transcript")).toContainText("PARTIAL_DESKTOP_PROOF");
    await expect.poll(async () => (await client.activeTurns()).length).toBe(0);
    await page.screenshot({ path: info.outputPath("filtered-response.png") });
    const items = await client.listItems(thread.id);
    expect(items.some(item => item.data.type === "error" && !item.data.retryable)).toBe(true);
    expect(errors).toEqual([]);
  } finally {
    await page.screenshot({ path: info.outputPath("final.png") }).catch(() => {});
    await app.evaluate(({ BrowserWindow }) => { for (const window of BrowserWindow.getAllWindows()) window.destroy(); }).catch(() => {});
    await app.close().catch(() => {});
    try { execFileSync(binary, ["--workspace", workspace, "serve", "--stop"], { env, stdio: "ignore", timeout: 30000 }); } catch { /* fixture may have stopped */ }
    server.closeAllConnections();
    await new Promise<void>(resolve => server.close(() => resolve()));
  }
});
