import { test, expect, _electron as electron, type ElectronApplication, type Page } from "@playwright/test";
import { mkdtemp, mkdir, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";
import { isolatedEnv } from "./fixtures/isolated-env";
import { stopChatHome } from "./fixtures/chat-home";

/**
 * What Search and the Settings filter show reads as words in the interface's
 * language: a tool is "Apply patch", never its name "apply_patch", and the
 * installed catalog follows a change of language like every other row.
 */
const binary = resolve(process.env.HAARNESS_BIN ?? "../../target/debug/haarness");
const letters = "abcdefghijklmnopqrstuvwxyz".split("");
/** An identifier written as words joined by underscores. */
const snake = /[\p{L}\p{N}]_[\p{L}\p{N}]/u;

let app: ElectronApplication;
let page: Page;
let root: string;
let project: string;
let env: Record<string, string>;

test.beforeAll(async () => {
  test.setTimeout(300_000);
  root = await mkdtemp(join(tmpdir(), "haarness-search-labels-"));
  project = join(root, "project");
  await mkdir(join(project, ".haarness"), { recursive: true });
  await writeFile(join(project, ".haarness/config.toml"), '[defaults]\nprovider="echo"\nmodel="echo-1"\n[lsp]\nenabled=false\n');
  execFileSync("git", ["init", "-q"], { cwd: project });
  env = {
    ...isolatedEnv(["ELECTRON_RUN_AS_NODE", "ELECTRON_RENDERER_URL"]),
    HAARNESS_BIN: binary,
    HAARNESS_HOME: join(root, "home"),
    HAARNESS_DATA_DIR: join(root, "data"),
    HAARNESS_DESKTOP_DATA: join(root, "desktop"),
    HAARNESS_CLOUD_CONFIG_DIR: join(root, "account"),
    HAARNESS_TEST_PROVIDER: "1",
    HAARNESS_DESKTOP_TEST: "1",
  };
  app = await electron.launch({ args: ["."], env, timeout: 240_000 });
  page = await app.firstWindow();
  await app.evaluate(({ dialog }, directory) => {
    dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [directory] });
  }, project);
  await page.getByRole("button", { name: "Open another project", exact: true }).click();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 240_000 });
});

test.afterAll(async () => {
  await page?.evaluate(() => window.haarness.preferences({ language: "en" })).catch(() => {});
  await app?.close().catch(() => {});
  try { execFileSync(binary, ["--workspace", project, "serve", "--stop"], { env, stdio: "ignore", timeout: 30_000 }); } catch {}
  if (env) stopChatHome(binary, env);
  if (root) await rm(root, { recursive: true, force: true }).catch(() => {});
});

/** Every row the Settings filter lists for each letter. */
async function settingsRows() {
  const filter = page.locator(".settings-filter");
  const rows = new Map<string, { name: string; detail: string }>();
  for (const letter of letters) {
    await filter.fill(letter);
    await expect(page.locator(".settings-search-results")).toBeVisible();
    for (const row of await page.locator(".settings-search-results button").evaluateAll((buttons) =>
      buttons.map((button) => ({
        name: button.querySelector("span")?.textContent ?? "",
        detail: button.querySelector("small")?.textContent ?? "",
      })),
    ))
      rows.set(row.name + "\n" + row.detail, row);
  }
  await filter.fill("");
  return [...rows.values()];
}

test("Settings search names tools by their titles, in the interface's language", async () => {
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  const filter = page.locator(".settings-filter");
  const results = page.locator(".settings-search-results");
  // The installed tools arrive after the dialog opens.
  await filter.fill("apply_patch");
  await expect(results.getByRole("button", { name: "Apply patch" })).toBeVisible();
  await expect(results.getByRole("button", { name: "Apply patch" })).toContainText("Settings › Tools");
  // Found by its name, its title or its words.
  await filter.fill("apply patch");
  await expect(results.getByRole("button", { name: "Apply patch" })).toBeVisible();
  await filter.fill("browser_cpu_analyze");
  await expect(results.getByRole("button", { name: "Browser CPU analyze" })).toBeVisible();

  const english = await settingsRows();
  expect(english.length).toBeGreaterThan(100);
  expect(english.filter((row) => snake.test(row.name))).toEqual([]);
  // A single-word tool reads as a title too, and is not taken for a word to
  // translate ("question" is the Question tool, not the word "Frage").
  expect(english).toContainEqual({ name: "Question", detail: "Settings › Tools" });

  // Changing the language rewrites the rows already loaded.
  await page.evaluate(() => window.haarness.preferences({ language: "de" }));
  await expect(page.locator(".settings-dialog h2")).toContainText("Einstellungen");
  const german = await settingsRows();
  expect(german.filter((row) => snake.test(row.name))).toEqual([]);
  expect(german.filter((row) => row.detail.startsWith("Settings"))).toEqual([]);
  expect(german).toContainEqual({ name: "Question", detail: "Einstellungen › Tools" });
  await page.evaluate(() => window.haarness.preferences({ language: "en" }));
  await page.getByRole("button", { name: "Close settings" }).click();
});

test("Search lists no snake_case names in English or German", async () => {
  for (const language of ["en", "de"]) {
    await page.evaluate((language) => window.haarness.preferences({ language }), language);
    await page.locator("[data-search-button]").click();
    const box = page.locator(".palette-dialog input[role=combobox]");
    const names = new Set<string>();
    for (const query of ["", ...letters]) {
      await box.fill(query);
      await expect(page.locator(".palette-count")).not.toHaveText(/…$/);
      for (const name of await page
        .locator(".palette-results [role=option]")
        .evaluateAll((rows) => rows.map((row) => row.getAttribute("aria-label") ?? "")))
        names.add(name);
    }
    await page.keyboard.press("Escape");
    expect(names.size).toBeGreaterThan(50);
    expect([...names].filter((name) => snake.test(name)), language).toEqual([]);
  }
  await page.evaluate(() => window.haarness.preferences({ language: "en" }));
});
