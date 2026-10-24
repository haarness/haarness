import { test, expect, _electron as electron, type ElectronApplication, type Page } from "@playwright/test";
import { Haarness } from "@haarness/sdk";
import { mkdtemp, mkdir, realpath, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";
import { stopChatHome } from "./fixtures/chat-home";

// Switching to another chat of the open project hid the conversation and the
// message box until the chat's history arrived, so every switch blanked the
// window for a moment (longer on a slow server). The box and the frame now
// stay; only the transcript waits, and the loading mark fades in over it
// after the same delay as before.
test.describe.configure({ mode: "serial" });

let app: ElectronApplication, page: Page, client: Haarness;
let temporary: string, workspace: string, binary: string, env: Record<string, string>;
const errors: string[] = [];
const shots = process.env.HAARNESS_SCREENSHOTS;
const ids: Record<string, string> = {};

test.beforeAll(async () => {
  test.setTimeout(240000);
  temporary = await realpath(await mkdtemp(join(tmpdir(), "haarness-chat-switch-")));
  workspace = join(temporary, "workspace");
  await mkdir(join(workspace, ".haarness"), { recursive: true });
  await writeFile(join(workspace, ".haarness/config.toml"), '[defaults]\nprovider="echo"\nmodel="echo-1"\n');
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
  // A chat's history read can be held at a gate, as a slow server would hold
  // it. Installed before the project's client exists, which keeps its fetch.
  await app.evaluate(() => {
    const state = globalThis as any;
    const fetch = globalThis.fetch;
    state.held = 0;
    globalThis.fetch = async (...args) => {
      if (state.match && String(args[0]).includes(state.match) && (args[1]?.method ?? "GET") === "GET") {
        state.held++;
        await state.gate;
      }
      return fetch(...args);
    };
  });
  page = await app.firstWindow();
  page.on("pageerror", (error) => errors.push(error.message));
  await page.setViewportSize({ width: 1400, height: 900 });
  await app.evaluate(({ dialog }, directory) => {
    dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [directory] });
  }, workspace);
  await page.getByRole("button", { name: "Open another project", exact: true }).click();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 90000 });
  const connection = JSON.parse(
    execFileSync(binary, ["--workspace", workspace, "--trust-project", "desktop"], {
      env,
      input: '{"action":"connect"}',
      encoding: "utf8",
      timeout: 120000,
    }),
  );
  client = new Haarness({ baseUrl: connection.url, token: connection.token });
  for (const title of ["Switch one", "Switch two"]) {
    const thread = await client.createThread({ title, provider: "echo", model: "echo-1" });
    ids[title] = thread.id;
    for (const line of [`${title} first`, `${title} second`]) {
      const turn = await client.startTurn(thread.id, { requestId: crypto.randomUUID(), text: line });
      await client.waitTurn(turn.id);
    }
  }
});

test.afterAll(async () => {
  client?.close();
  await app?.close();
  try {
    execFileSync(binary, ["--workspace", workspace, "serve", "--stop"], { env, stdio: "ignore", timeout: 30_000 });
  } catch {}
  if (binary) stopChatHome(binary, env);
  if (temporary) await rm(temporary, { recursive: true, force: true }).catch(() => {});
});

const row = (title: string) => page.locator(".sessions").getByRole("button", { name: title, exact: true });
const composer = () => page.getByRole("textbox", { name: "Message", exact: true });
const loading = () => page.getByRole("status", { name: "Loading chat…", exact: true });

/** Counts, frame by frame, what a switch shows until `stop` is called. */
async function sample() {
  await page.evaluate(() => {
    const seen = { frames: 0, boxless: 0, blank: 0, empty: 0 };
    const state = window as unknown as { switchSeen: typeof seen; switchStop?: () => void };
    state.switchSeen = seen;
    let frame = 0;
    const tick = () => {
      seen.frames++;
      const box = document.querySelector<HTMLElement>("[data-composer-input]");
      if (!box?.checkVisibility({ visibilityProperty: true })) seen.boxless++;
      // The loading screen that replaced the whole conversation.
      if (document.querySelector(".workspace-content > .chat-loading")) seen.blank++;
      const start = document.querySelector<HTMLElement>(".conversation-empty");
      if (start && start.getBoundingClientRect().height > 0) seen.empty++;
      frame = requestAnimationFrame(tick);
    };
    frame = requestAnimationFrame(tick);
    state.switchStop = () => cancelAnimationFrame(frame);
  });
  return async () =>
    page.evaluate(() => {
      const state = window as unknown as { switchSeen: object; switchStop: () => void };
      state.switchStop();
      return state.switchSeen as { frames: number; boxless: number; blank: number; empty: number };
    });
}

test("switching chats keeps the message box on every frame, and a slow history shows its mark over the transcript only", async ({}, info) => {
  test.setTimeout(180000);
  await row("Switch one").click();
  await expect(page.locator(".transcript")).toContainText("Echo: Switch one second");
  await expect(composer()).toBeVisible();

  // A slow history read: the box stays, the mark waits, then shows over the transcript.
  const stop = await sample();
  await app.evaluate((_, match) => {
    const state = globalThis as any;
    state.match = match;
    state.held = 0;
    state.gate = new Promise<void>((resolve) => (state.release = resolve));
  }, `/v1/threads/${ids["Switch two"]}/items`);
  await row("Switch two").click();
  await expect.poll(() => app.evaluate(() => (globalThis as any).held)).toBe(1);
  await expect(page.locator(".session-bar")).toContainText("Switch two");
  await expect(composer()).toBeVisible();
  await expect(loading()).toBeVisible();
  await expect(loading()).toHaveCSS("opacity", "1");
  const mark = (await loading().boundingBox())!;
  const box = (await page.locator(".composer").boundingBox())!;
  expect(mark.y + mark.height).toBeLessThanOrEqual(box.y + 0.5);
  // What is typed meanwhile is the new chat's draft.
  await composer().fill("Typed while it loads");
  for (const theme of ["light", "dark"] as const) {
    await page.evaluate((theme) => window.haarness.preferences({ theme }), theme);
    await expect(page.locator("html")).toHaveAttribute("data-theme", theme);
    await page.waitForTimeout(150);
    await page.screenshot({ path: join(shots ?? info.outputPath(""), `chat-switch-loading-${theme}.png`) });
  }
  await page.evaluate(() => window.haarness.preferences({ theme: "light" }));
  await app.evaluate(() => {
    const state = globalThis as any;
    state.match = "";
    state.release();
  });
  await expect(page.locator(".transcript")).toContainText("Echo: Switch two second");
  await expect(loading()).toHaveCount(0);
  await expect(composer()).toHaveValue("Typed while it loads");
  let seen = await stop();
  expect(seen.frames).toBeGreaterThan(20);
  expect(seen).toMatchObject({ boxless: 0, blank: 0, empty: 0 });
  await composer().fill("");

  // Quick switches back and forth: no frame without the box, and no mark.
  const quick = await sample();
  for (const title of ["Switch one", "Switch two", "Switch one"]) {
    await row(title).click();
    await expect(page.locator(".transcript")).toContainText(`Echo: ${title} second`);
  }
  seen = await quick();
  expect(seen).toMatchObject({ boxless: 0, blank: 0, empty: 0 });
  await expect(loading()).toHaveCount(0);
  expect(errors).toEqual([]);
});
