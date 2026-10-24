import {
  test,
  expect,
  _electron as electron,
  type ElectronApplication,
} from "@playwright/test";
import {
  cp,
  mkdtemp,
  mkdir,
  readFile,
  writeFile,
  readdir,
  rename,
  rm,
  symlink,
  access,
} from "node:fs/promises";
import { join, resolve, dirname } from "node:path";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import { execFileSync } from "node:child_process";
import { stopChatHome } from "./fixtures/chat-home";

const binary = resolve(
  process.env.HAARNESS_BIN ?? "../../target/debug/haarness",
);
let temporary: string;
let application: string;
let app: ElectronApplication | undefined;
let env: Record<string, string>;
let workspace: string | undefined;

test.beforeEach(async () => {
  temporary = await mkdtemp(join(tmpdir(), "haarness-rebuild-"));
  application = join(temporary, "app");
  await mkdir(application);
  await cp("out", join(application, "out"), { recursive: true });
  await cp("package.json", join(application, "package.json"));
  await symlink(
    resolve("node_modules"),
    join(application, "node_modules"),
    "junction",
  );
  env = Object.fromEntries(
    Object.entries(process.env).filter(
      ([key, value]) =>
        value !== undefined &&
        !key.startsWith("HAARNESS_") &&
        !["ELECTRON_RUN_AS_NODE", "ELECTRON_RENDERER_URL"].includes(key),
    ),
  ) as Record<string, string>;
  Object.assign(env, {
    HAARNESS_BIN: binary,
    HAARNESS_DATA_DIR: join(temporary, "data"),
    HAARNESS_HOME: join(temporary, "home"),
    HAARNESS_DESKTOP_DATA: join(temporary, "desktop"),
    HAARNESS_DESKTOP_TEST: "1",
    HAARNESS_TEST_PROVIDER: "1",
    HAARNESS_DEFAULT_PROVIDER: "echo",
    HAARNESS_DEFAULT_MODEL: "echo-1",
  });
  workspace = undefined;
});
test.afterEach(async () => {
  await app?.close().catch(() => {});
  app = undefined;
  if (workspace)
    execFileSync(binary, ["--workspace", workspace, "serve", "--stop"], {
      env,
      timeout: 30000,
      stdio: "ignore",
    });
  stopChatHome(binary, env);
  await rm(temporary, { recursive: true, force: true });
});
async function launch() {
  app = await electron.launch({ args: [application], env });
  const page = await app.firstWindow();
  await expect(
    page.getByRole("button", { name: "Settings", exact: true }),
  ).toBeVisible();
  return page;
}

test("open windows retain deferred Settings and unsent drafts when a rebuild replaces hashed assets", async () => {
  let page = await launch();
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  workspace = join(temporary, "project");
  await mkdir(workspace);
  await app!.evaluate(({ dialog }, path) => {
    dialog.showOpenDialog = async () => ({
      canceled: false,
      filePaths: [path],
    });
  }, workspace);
  await page
    .getByRole("button", { name: "Open another project", exact: true })
    .click();
  const composer = page.getByRole("textbox", { name: "Message", exact: true });
  await composer.fill("rebuild conversation proof");
  await page.getByRole("button", { name: "Send message", exact: true }).click();
  await expect(page.locator(".transcript")).toContainText(
    "Echo: rebuild conversation proof",
  );
  await composer.fill("unsent draft survives rebuilding");
  const sessionRenderer = dirname(fileURLToPath(page.url()));
  const buildAssets = join(application, "out/renderer/assets");
  const oldName = (await readdir(buildAssets)).find((name) =>
    /^Settings-.*\.js$/.test(name),
  )!;
  expect(oldName).toBeTruthy();
  expect(sessionRenderer).not.toBe(join(application, "out/renderer"));
  // Simulate Vite replacing content-hashed output, without modifying the user's running app.
  const newName = "Settings-rebuilt.js";
  await rename(join(buildAssets, oldName), join(buildAssets, newName));
  for (const name of await readdir(buildAssets)) {
    if (!name.endsWith(".js")) continue;
    const path = join(buildAssets, name);
    await writeFile(
      path,
      (await readFile(path, "utf8")).replaceAll(oldName, newName),
    );
  }
  await expect(access(join(buildAssets, oldName))).rejects.toThrow();
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  await expect(
    page.getByRole("dialog", { name: "Settings", exact: true }),
  ).toBeVisible();
  await expect(page.locator(".fatal")).toHaveCount(0);
  await page.screenshot({
    animations: "disabled",
    path: test.info().outputPath("settings-after-rebuild.png"),
  });
  await page
    .getByRole("button", { name: "Close settings", exact: true })
    .click();
  await expect(composer).toHaveValue("unsent draft survives rebuilding");
  // Reload still uses the same coherent session and trusted IPC bridge.
  await page.reload();
  await expect(composer).toHaveValue("unsent draft survives rebuilding");
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  await expect(
    page.getByRole("dialog", { name: "Settings", exact: true }),
  ).toBeVisible();
  expect(errors).toEqual([]);
  await app!.close();
  app = undefined;
  await expect(access(sessionRenderer)).rejects.toThrow();
  page = await launch();
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  await expect(
    page.getByRole("dialog", { name: "Settings", exact: true }),
  ).toBeVisible();
  await expect(page.locator(".fatal")).toHaveCount(0);
  const newSession = dirname(fileURLToPath(page.url()));
  expect(newSession).not.toBe(sessionRenderer);
  await access(join(newSession, "assets", newName));
  await page.screenshot({
    animations: "disabled",
    path: test.info().outputPath("settings-after-relaunch.png"),
  });
});

test("view recovery keeps raw module errors and local paths out of the user interface", async () => {
  const page = await launch();
  const diagnostics: string[] = [];
  page.on("console", (message) => {
    if (message.type() === "error") diagnostics.push(message.text());
  });
  const assets = join(dirname(fileURLToPath(page.url())), "assets");
  const settings = (await readdir(assets)).find((name) =>
    /^Settings-.*\.js$/.test(name),
  )!;
  await rm(join(assets, settings));
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  const recovery = page.locator(".fatal");
  await expect(recovery).toContainText("Your server keeps running.");
  await expect(recovery).not.toContainText(
    /TypeError|file:\/\/|Settings-|dynamically imported|\/Users\//,
  );
  await expect(
    recovery.getByRole("button", { name: "Reload window", exact: true }),
  ).toBeVisible();
  expect(
    diagnostics.some((message) => message.includes("Desktop view failed")),
  ).toBe(true);
  await page.screenshot({
    animations: "disabled",
    path: test.info().outputPath("friendly-recovery.png"),
  });
});
