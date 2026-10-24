import { test, expect, _electron as electron, type ElectronApplication, type Locator, type Page } from "@playwright/test";
import { mkdtemp, mkdir, realpath, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";
import { stopChatHome } from "./fixtures/chat-home";

// Settings is a modal at most 1040x760 in the middle of the window. Its
// lists, menus and suggestion lists were placed against the window, so one
// opened near the panel's end or bottom hung past it over what it covers.
// They now keep 8px inside the dialog's box where they fit, and Escape on one
// closes it alone.
test.describe.configure({ mode: "serial" });

let app: ElectronApplication, page: Page;
let temporary: string, workspace: string, binary: string, env: Record<string, string>;
const errors: string[] = [];
const shots = process.env.HAARNESS_SCREENSHOTS;

test.beforeAll(async () => {
  test.setTimeout(240000);
  temporary = await realpath(await mkdtemp(join(tmpdir(), "haarness-settings-popups-")));
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
  page = await app.firstWindow();
  page.on("pageerror", (error) => errors.push(error.message));
  await page.setViewportSize({ width: 1600, height: 1000 });
  await app.evaluate(({ dialog }, directory) => {
    dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [directory] });
  }, workspace);
  await page.getByRole("button", { name: "Open another project", exact: true }).click();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 90000 });
});

test.afterAll(async () => {
  await app?.close();
  try {
    execFileSync(binary, ["--workspace", workspace, "serve", "--stop"], { env, stdio: "ignore", timeout: 30_000 });
  } catch {}
  if (binary) stopChatHome(binary, env);
  if (temporary) await rm(temporary, { recursive: true, force: true }).catch(() => {});
});

const settings = () => page.getByRole("dialog", { name: "Settings", exact: true });
/** The popover a control opened, wherever it is mounted. */
const popup = () => page.locator(".select-popover, .action-menu.popover:not(.menu-branch-panel)").last();

async function openSettings() {
  if (await settings().isVisible()) return;
  await page.getByRole("button", { name: "Settings", exact: true }).first().click();
  await expect(settings()).toBeVisible();
}

async function section(name: string) {
  await settings().locator("aside").getByRole("button", { name, exact: true }).click();
  await expect(settings().locator(".settings-main h1")).toHaveText(name);
  // Lists and rows that load settle before their controls are measured.
  await page.waitForTimeout(400);
}

/** Every control in `scope` that opens a list or a menu. */
function openers(scope: Locator) {
  return scope.locator(
    'button.select-trigger:not(:disabled), button[aria-haspopup="menu"]:not(:disabled), .combobox-toggle:not(:disabled)',
  );
}

type Box = { left: number; right: number; top: number; bottom: number };
const box = (locator: Locator) =>
  locator.evaluate((element) => {
    const rect = element.getBoundingClientRect();
    return { left: rect.left, right: rect.right, top: rect.top, bottom: rect.bottom };
  });
function inside(layer: Box, frame: Box, label: string) {
  // 8px in from the dialog's edges; half a pixel for subpixel layout.
  expect(layer.left, label).toBeGreaterThanOrEqual(frame.left + 8 - 0.5);
  expect(layer.right, label).toBeLessThanOrEqual(frame.right - 8 + 0.5);
  expect(layer.top, label).toBeGreaterThanOrEqual(frame.top + 8 - 0.5);
  expect(layer.bottom, label).toBeLessThanOrEqual(frame.bottom - 8 + 0.5);
}

/** Opens each control at the bottom of the scrolled pane, where the window has room below and the dialog does not. */
async function checkSection(name: string, frame: Locator) {
  await section(name);
  const controls = openers(settings().locator(".settings-content"));
  const count = await controls.count();
  let checked = 0;
  for (let index = 0; index < count; index++) {
    const control = controls.nth(index);
    if (!(await control.isVisible())) continue;
    await control.evaluate((element) => element.scrollIntoView({ block: "end" }));
    const label = `${name}: ${(await control.getAttribute("aria-label")) ?? (await control.textContent())}`;
    await control.click();
    const layer = popup();
    await expect(layer, label).toBeVisible();
    // Placement settles over a frame or two (the list measures itself first).
    await page.waitForTimeout(120);
    inside(await box(layer), await box(frame), label);
    await page.keyboard.press("Escape");
    await expect(layer, label).toBeHidden();
    await expect(settings(), label).toBeVisible();
    checked++;
  }
  return checked;
}

test("lists, menus and suggestions opened in Settings stay inside its panel, and Escape closes only them", async ({}, info) => {
  test.setTimeout(240000);
  await openSettings();
  let checked = 0;
  // Every section but Marketplace, which reads a catalog from the network.
  for (const name of [
    "General", "Browser", "Keyboard shortcuts", "Servers", "Projects", "Profiles", "Providers", "Models", "Agents",
    "Commands", "Skills", "Tools", "MCP servers", "Plugins", "Extensions", "Hooks", "Schedules", "About",
  ])
    checked += await checkSection(name, settings());
  // The run must have opened something in Settings itself, near its end.
  expect(checked).toBeGreaterThan(4);

  // A form opened over Settings keeps its own lists and suggestions inside it.
  await section("Hooks");
  await settings().getByRole("button", { name: "Add hook", exact: true }).first().click();
  const form = page.getByRole("dialog", { name: "Add hook", exact: true });
  await expect(form).toBeVisible();
  for (const control of [
    form.getByRole("combobox", { name: "Hook event", exact: true }),
    form.getByRole("button", { name: "Show suggestions", exact: true }),
  ]) {
    if (!(await control.isVisible()) || (await control.isDisabled())) continue;
    await control.click();
    const layer = page.locator(".select-popover").last();
    await expect(layer).toBeVisible();
    await page.waitForTimeout(120);
    inside(await box(layer), await box(form), "Add hook");
    await page.keyboard.press("Escape");
    await expect(layer).toBeHidden();
    await expect(form).toBeVisible();
  }
  await page.keyboard.press("Escape");
  await expect(form).toBeHidden();
  await expect(settings()).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(settings()).toBeHidden();
  info.annotations.push({ type: "opened", description: String(checked) });
  expect(errors).toEqual([]);
});

/** A screenshot of Settings with `open` showing, in both themes. */
async function capture(name: string, open: () => Promise<void>, info: { outputPath: (name: string) => string }) {
  for (const theme of ["light", "dark"] as const) {
    await page.evaluate((theme) => window.haarness.preferences({ theme }), theme);
    await expect(page.locator("html")).toHaveAttribute("data-theme", theme);
    await open();
    await page.waitForTimeout(250);
    const path = join(shots ?? info.outputPath(""), `${name}-${theme}.png`);
    await page.screenshot({ path });
    await page.keyboard.press("Escape");
    await expect(settings()).toBeVisible();
  }
}

test("open lists and menus in Settings, in both themes, in a large and a small window", async ({}, info) => {
  test.setTimeout(240000);
  if (shots) await mkdir(shots, { recursive: true });
  for (const [size, viewport] of [
    ["1600x1000", { width: 1600, height: 1000 }],
    ["small", { width: 900, height: 640 }],
  ] as const) {
    await page.setViewportSize(viewport);
    await openSettings();
    await section("General");
    const select = openers(settings().locator(".settings-content")).filter({ has: page.locator(".select-label") });
    const last = select.last();
    await capture(`settings-select-${size}`, async () => {
      await last.evaluate((element) => element.scrollIntoView({ block: "end" }));
      await last.click();
      await expect(popup()).toBeVisible();
    }, info);
    await section("Profiles");
    const menu = settings().getByRole("button", { name: "Manage catalog", exact: true });
    await capture(`settings-menu-${size}`, async () => {
      await menu.evaluate((element) => element.scrollIntoView({ block: "end" }));
      await menu.click();
      await expect(popup()).toBeVisible();
    }, info);
    await section("Hooks");
    await settings().getByRole("button", { name: "Add hook", exact: true }).first().click();
    const form = page.getByRole("dialog", { name: "Add hook", exact: true });
    await expect(form).toBeVisible();
    const suggestions = form.getByRole("button", { name: "Show suggestions", exact: true });
    if ((await suggestions.isVisible()) && !(await suggestions.isDisabled()))
      for (const theme of ["light", "dark"] as const) {
        await page.evaluate((theme) => window.haarness.preferences({ theme }), theme);
        await suggestions.click();
        await expect(page.locator(".combobox-popover")).toBeVisible();
        await page.waitForTimeout(250);
        await page.screenshot({ path: join(shots ?? info.outputPath(""), `settings-combobox-${size}-${theme}.png`) });
        await page.keyboard.press("Escape");
      }
    await page.keyboard.press("Escape");
    await expect(form).toBeHidden();
    await page.keyboard.press("Escape");
    await expect(settings()).toBeHidden();
  }
  await page.evaluate(() => window.haarness.preferences({ theme: "light" }));
  await page.setViewportSize({ width: 1600, height: 1000 });
  expect(errors).toEqual([]);
});
