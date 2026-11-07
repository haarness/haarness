import { test, expect, _electron as electron, type ElectronApplication, type Locator, type Page } from "@playwright/test";
import { mkdtemp, mkdir, writeFile, realpath } from "node:fs/promises";
import { execFileSync } from "node:child_process";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { stopChatHome } from "./fixtures/chat-home";
import { isolatedEnv } from "./fixtures/isolated-env";

// A call Haarness stopped before it finished: the server crashed while the
// call waited, and the next server gave it a result written for the model.
// Its row says in two words that it did not finish, and keeps the model's
// sentence out of its body. Pictures are kept in HAARNESS_SCREENSHOTS when it
// is set.
let app: ElectronApplication, page: Page, root: string, project: string;
let env: Record<string, string>;
const binary = resolve(process.env.HAARNESS_BIN ?? "../../target/debug/haarness");
const shots = process.env.HAARNESS_SCREENSHOTS;
const errors: string[] = [];
const MODEL_TEXT = "so there is no result";
test.describe.configure({ mode: "serial" });

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

/** The pids of this fixture's own server: its project is a fresh folder. */
function serverPids() {
  return execFileSync("ps", ["-axo", "pid=,command="], { encoding: "utf8" })
    .split("\n")
    .filter((line) => line.includes(project) && line.includes(" serve"))
    .map((line) => Number(line.trim().split(/\s+/)[0]));
}

async function until<T>(check: () => Promise<T | undefined> | T | undefined, label: string): Promise<T> {
  const deadline = Date.now() + 30000;
  while (Date.now() < deadline) {
    const value = await check();
    if (value) return value;
    await new Promise((done) => setTimeout(done, 100));
  }
  throw new Error(label);
}

test.beforeAll(async () => {
  test.setTimeout(180000);
  root = await realpath(await mkdtemp(join(process.env.HAARNESS_E2E_DIR ?? tmpdir(), "haarness-unfinished-")));
  project = join(root, "project");
  await mkdir(join(project, ".haarness"), { recursive: true });
  // The call waits for its approval, in Review, until the server dies.
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

  // The chat whose call the crash cuts off, made before Desktop opens.
  const server = JSON.parse(
    execFileSync(binary, ["--workspace", project, "--trust-project", "desktop"], {
      env,
      input: '{"action":"connect"}',
      encoding: "utf8",
      timeout: 90000,
    }),
  );
  const call = async (path: string, body?: unknown) => {
    const response = await fetch(server.url + "/v1" + path, {
      method: body === undefined ? "GET" : "POST",
      headers: { Authorization: "Bearer " + server.token, "Content-Type": "application/json" },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    return response.json();
  };
  const thread = await call("/threads", { provider: "echo", model: "echo-1", title: "Cut off" });
  await call(`/threads/${thread.id}/turns`, {
    requestId: crypto.randomUUID(),
    text: "!tool bash " + JSON.stringify({ command: "touch never.txt" }),
  });
  await until(async () => (await call("/permissions")).length > 0, "the call did not ask");
  for (const pid of serverPids()) process.kill(pid, "SIGKILL");
  await until(() => !serverPids().length, "the server outlived its kill");

  app = await electron.launch({ args: ["."], env, timeout: 60000 });
  page = await app.firstWindow();
  page.setDefaultTimeout(15000);
  page.on("pageerror", (error) => errors.push(error.message));
  await app.evaluate(({ dialog }, path) => {
    dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [path] });
  }, project);
  await page.getByRole("button", { name: "Open another project", exact: true }).click();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 60000 });
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

test("a call a crash cut off says quietly that it did not finish", async () => {
  await page.locator(".sidebar-lists .session-row").filter({ hasText: "Cut off" }).first().click();
  const row = page.locator("details.tool-call").last();
  await expect(row).toBeVisible();
  await expect(row.locator("summary")).toContainText("touch never.txt");
  await expect(row).not.toHaveAttribute("data-status", "running");
  const word = row.locator("summary .tool-status");
  await expect(word).toHaveText("Didn't finish");
  await row.locator("summary").hover();
  await expect(word).toHaveCSS("opacity", "1");
  await row.locator("summary").click();
  await expect(row).toHaveAttribute("open", "");
  expect(await row.textContent()).not.toContain(MODEL_TEXT);
  // Nor does it say there is no output: "Didn't finish" already says it all.
  await expect(row.locator(".tool-empty")).toHaveCount(0);
  await expect(row).not.toContainText("No output");
  // The restart's own notice follows, once.
  await expect(page.getByText("Haarness restarted while this turn was running", { exact: false })).toHaveCount(1);
  await pictures("unfinished-call", page.locator(".transcript, .conversation").first());
  try {
    await page.evaluate(() => (window as any).haarness.preferences({ language: "ja" }));
    await expect(word).toHaveText("未完了");
    await page.evaluate(() => (window as any).haarness.preferences({ language: "ar" }));
    await expect(word).toHaveText("لم يكتمل");
    await pictures("unfinished-call-rtl", page.locator(".transcript, .conversation").first());
  } finally {
    await page.evaluate(() => (window as any).haarness.preferences({ language: "en" }));
  }
  expect(errors).toEqual([]);
});
