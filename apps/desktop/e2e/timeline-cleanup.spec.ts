import { test, expect, _electron as electron } from "@playwright/test";
import { Haarness, type Item } from "@haarness/sdk";
import { mkdtemp, mkdir, writeFile, realpath } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";
import { randomUUID } from "node:crypto";
import { stopChatHome } from "./fixtures/chat-home";

test("transcript measurements release detached rows across scrolling and session switches", async ({}, info) => {
  const temporary = await realpath(await mkdtemp(join(tmpdir(), "haarness-timeline-")));
  const workspace = join(temporary, "workspace");
  await mkdir(join(workspace, ".haarness"), { recursive: true });
  await writeFile(join(workspace, ".haarness/config.toml"), '[defaults]\nprovider="echo"\nmodel="echo-1"\n');
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
    await page.context().addInitScript(() => {
      const observers = new Set<Set<Element>>();
      const Original = window.ResizeObserver;
      window.ResizeObserver = class extends Original {
        targets = new Set<Element>();
        constructor(callback: ResizeObserverCallback) { super(callback); observers.add(this.targets); }
        observe(target: Element, options?: ResizeObserverOptions) { this.targets.add(target); super.observe(target, options); }
        unobserve(target: Element) { this.targets.delete(target); super.unobserve(target); }
        disconnect() { this.targets.clear(); super.disconnect(); }
      };
      Object.defineProperty(window, "__rowObservers", { value: observers });
    });
    await page.reload();
    await app.evaluate(({ dialog }, directory) => {
      dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [directory] });
    }, workspace);
    await page.getByRole("button", { name: "Open another project", exact: true }).click();
    await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 60000 });
    const connection = JSON.parse(execFileSync(binary, ["--workspace", workspace, "--trust-project", "desktop"], { env, input: '{"action":"connect"}', encoding: "utf8", timeout: 60000 }));
    client = new Haarness({ baseUrl: connection.url, token: connection.token });
    const long = await client.createThread({ title: "Long cleanup fixture", provider: "echo", model: "echo-1" });
    const short = await client.createThread({ title: "Other cleanup fixture", provider: "echo", model: "echo-1" });
    const archive = await client.exportSessionArchive(long.id);
    archive.threads[0].items = Array.from({ length: 1000 }, (_, index): Item => ({
      id: randomUUID(), threadId: long.id, turnId: null, sequence: index + 1, createdAt: long.createdAt,
      data: index % 2 === 0 ? { type: "user_message", text: `Question ${index}: explain the fixture.`, attachments: [] } : { type: "assistant_message", text: `Answer ${index}.\n\nA measured paragraph with **formatted content** and enough height to scroll.` },
    }));
    await client.importSessionArchive({ archive });
    const choose = async (title: string) => {
      await page.locator(".sessions").getByRole("button", { name: title, exact: true }).click();
      await expect(page.locator(".session-bar")).toContainText(title);
    };
    const detached = () => page.evaluate(() => {
      const observers = (window as unknown as { __rowObservers: Set<Set<Element>> }).__rowObservers;
      return [...observers].flatMap(targets => [...targets]).filter(node => node.hasAttribute("data-item-id") && !node.isConnected).length;
    });
    await choose(long.title);
    await expect(page.locator(".transcript [data-item-id]").first()).toBeVisible();
    for (let round = 0; round < 3; round++) {
      for (const position of [0, .25, .5, .75, 1]) {
        await page.locator(".transcript").evaluate((element, position) => { element.scrollTop = position * element.scrollHeight; }, position);
        await expect.poll(detached).toBe(0);
      }
      expect(await page.locator(".transcript [data-item-id]").count()).toBeLessThan(60);
      await choose(short.title);
      await expect.poll(detached).toBe(0);
      await choose(long.title);
    }
    await page.getByRole("textbox", { name: "Message", exact: true }).fill("A reply after repeated scrolling");
    await page.getByRole("button", { name: "Send message", exact: true }).click();
    await expect(page.locator(".transcript")).toContainText("Echo: A reply after repeated scrolling");
    await expect.poll(detached).toBe(0);
    await page.screenshot({ path: info.outputPath("timeline-cleanup.png") });
    expect(errors).toEqual([]);
  } finally {
    client?.close();
    await app.close().catch(() => {});
    try { execFileSync(binary, ["--workspace", workspace, "serve", "--stop"], { env, stdio: "ignore", timeout: 30000 }); } catch {}
    stopChatHome(binary, env);
    await info.attach("fixture", { body: temporary, contentType: "text/plain" });
  }
});
