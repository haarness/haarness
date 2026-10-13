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
    await expect(page.locator(".transcript")).toContainText("content filter", { timeout: 30000 });
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

test("a pasted image the provider refuses is left out with a notice and the chat goes on", async ({}, info) => {
  test.setTimeout(150000);
  const requests: { images: string[] }[] = [];
  // Anthropic's Messages API, refusing any request that carries an image.
  const server = createServer(async (request, response) => {
    let body = "";
    for await (const chunk of request) body += chunk;
    if (request.method === "GET") {
      response.setHeader("Content-Type", "application/json");
      response.end(JSON.stringify({ data: [{ id: "claude-sonnet-4-6", type: "model" }], has_more: false }));
      return;
    }
    const messages: { content: { type: string; source?: { media_type: string } }[] | string }[] = JSON.parse(body).messages;
    const images = messages.flatMap((message, index) => Array.isArray(message.content)
      ? message.content.flatMap((block, at) => block.type === "image" ? [`messages.${index}.content.${at}|${block.source!.media_type}`] : [])
      : []);
    requests.push({ images });
    if (images.length) {
      response.statusCode = 400;
      response.setHeader("Content-Type", "application/json");
      response.end(JSON.stringify({ type: "error", error: { type: "invalid_request_error",
        message: `${images[0].split("|")[0]}.image.source.base64: image exceeds 5 MB maximum: 7340032 bytes > 5242880 bytes` } }));
      return;
    }
    response.setHeader("Content-Type", "text/event-stream");
    for (const event of [
      { type: "message_start", message: { usage: { input_tokens: 10, output_tokens: 0 } } },
      { type: "content_block_start", index: 0, content_block: { type: "text", text: "" } },
      { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: "ANSWERED_WITHOUT_THE_IMAGE" } },
      { type: "content_block_stop", index: 0 },
      { type: "message_delta", delta: { stop_reason: "end_turn" }, usage: { output_tokens: 4 } },
      { type: "message_stop" },
    ]) response.write(`data: ${JSON.stringify(event)}\n\n`);
    response.end();
  });
  await new Promise<void>(resolve => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("Missing fixture address");
  const root = await realpath(await mkdtemp(join(tmpdir(), "haarness-refused-image-")));
  const workspace = join(root, "workspace");
  await mkdir(join(workspace, ".haarness"), { recursive: true });
  await writeFile(join(workspace, ".haarness/config.toml"), '[defaults]\nprovider="anthropic"\nmodel="claude-sonnet-4-6"\n[lsp]\nenabled=false\n');
  execFileSync("git", ["init", "-q", workspace]);
  const binary = resolve(process.env.HAARNESS_BIN ?? "../../target/debug/haarness");
  const env: Record<string, string> = {
    ...process.env, HAARNESS_BIN: binary, HAARNESS_HOME: join(root, "home"), HAARNESS_DATA_DIR: join(root, "data"),
    HAARNESS_DESKTOP_DATA: join(root, "desktop"), HAARNESS_DESKTOP_TEST: "1",
    ANTHROPIC_API_KEY: "fixture-key", ANTHROPIC_BASE_URL: `http://127.0.0.1:${address.port}`,
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
    const thread = await client.createThread({ title: "Refused image proof", provider: "anthropic", model: "claude-sonnet-4-6" });
    await page.locator(".sessions").getByRole("button", { name: thread.title, exact: true }).click();
    const box = page.getByRole("textbox", { name: "Message", exact: true });
    await box.click();
    // A JPEG the clipboard calls a PNG: the bytes decide what is sent.
    await page.evaluate(async () => {
      const canvas = document.createElement("canvas");
      canvas.width = 24; canvas.height = 16;
      const context = canvas.getContext("2d")!;
      context.fillStyle = "#c33"; context.fillRect(0, 0, 24, 16);
      const jpeg = await new Promise<Blob>(done => canvas.toBlob(blob => done(blob!), "image/jpeg", 0.8));
      const transfer = new DataTransfer();
      transfer.items.add(new File([jpeg], "shot.png", { type: "image/png" }));
      document.activeElement!.dispatchEvent(new ClipboardEvent("paste", { clipboardData: transfer, bubbles: true, cancelable: true }));
    });
    await expect(page.getByRole("button", { name: "Remove shot.png", exact: true })).toBeVisible();
    await box.fill("What is in the screenshot?");
    await page.getByRole("button", { name: /Send message|Send guidance/ }).click();
    const notice = page.locator(".notice-inline.media-omitted");
    await expect(notice).toContainText("The provider refused shot.png, so it is left out of this chat from now on.", { timeout: 30000 });
    await expect(notice).toContainText("image exceeds 5 MB maximum");
    await expect(page.locator(".transcript")).toContainText("ANSWERED_WITHOUT_THE_IMAGE", { timeout: 30000 });
    await expect.poll(async () => (await client.activeTurns()).length).toBe(0);
    await page.locator(".transcript").screenshot({ path: info.outputPath("refused-image-notice.png") });
    expect(requests.map(request => request.images)).toEqual([["messages.0.content.1|image/jpeg"], []]);
    const items = await client.listItems(thread.id);
    expect(items.some(item => item.data.type === "error")).toBe(false);
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
