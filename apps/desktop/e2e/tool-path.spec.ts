import { test, expect, _electron as electron, type ElectronApplication, type Page } from "@playwright/test";
import { mkdtemp, mkdir, writeFile, chmod, realpath, rm } from "node:fs/promises";
import { execFileSync } from "node:child_process";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { stopChatHome } from "./fixtures/chat-home";
import { isolatedEnv } from "./fixtures/isolated-env";

// Desktop opened from the Dock or the Finder gets launchd's short PATH, and
// the agent's commands run under `sh -l`, which never reads a zsh profile: a
// tool in ~/.local/bin or ~/.cargo/bin must still be found, after the
// person's own folders and never from a folder anyone may write to. The echo
// provider runs the command for real, in a home folder of the test's own.
let app: ElectronApplication | undefined;
let page: Page;
let root: string, project: string;
let env: Record<string, string>;
const binary = resolve(process.env.HAARNESS_BIN ?? "../../target/debug/haarness");

async function api(method: string, ...args: unknown[]): Promise<any> {
  return page.evaluate(
    ({ connection, method, args }) => (window as any).haarness.api(connection, method, args),
    { connection: "local:" + project, method, args },
  );
}
async function tool(folder: string, name: string, says: string, mode = 0o755) {
  await mkdir(folder, { recursive: true });
  await writeFile(join(folder, name), `#!/bin/sh\necho ${says}\n`);
  await chmod(join(folder, name), 0o755);
  await chmod(folder, mode);
}

test.skip(process.platform === "win32", "Windows apps get the person's PATH from the registry");

test.beforeAll(async () => {
  root = await realpath(await mkdtemp(join(process.env.HAARNESS_E2E_DIR ?? tmpdir(), "haarness-tool-path-")));
  project = join(root, "project");
  const home = join(root, "home");
  const own = join(root, "own bin");
  await mkdir(join(project, ".haarness"), { recursive: true });
  await writeFile(
    join(project, ".haarness/config.toml"),
    '[defaults]\nprovider="echo"\nmodel="echo-1"\n[permissions]\nbash="allow"\n[lsp]\nenabled=false\n',
  );
  await tool(join(home, ".local/bin"), "haarness-probe-local", "found-in-local-bin");
  await tool(join(home, ".local/bin"), "haarness-probe-both", "found-in-local-bin");
  await tool(join(home, ".cargo/bin"), "haarness-probe-cargo", "found-in-cargo-bin");
  await tool(own, "haarness-probe-both", "found-in-own-folder");
  // A folder anyone may write to is never added, even one Haarness knows.
  await tool(join(home, "go/bin"), "haarness-probe-open", "found-in-open-folder", 0o777);
  env = isolatedEnv();
  Object.assign(env, {
    HAARNESS_BIN: binary,
    HAARNESS_HOME: join(root, "haarness"),
    HAARNESS_DATA_DIR: join(root, "data"),
    HAARNESS_DESKTOP_DATA: join(root, "desktop"),
    HAARNESS_TEST_PROVIDER: "1",
    HAARNESS_DESKTOP_TEST: "1",
    HOME: home,
    // What launchd gives an app opened from the Dock, after a folder of the
    // person's own; a zsh user's profile would add the rest.
    PATH: [own, "/usr/bin", "/bin", "/usr/sbin", "/sbin"].join(":"),
    SHELL: "/bin/zsh",
  });
  delete env.ELECTRON_RUN_AS_NODE;
  delete env.ELECTRON_RENDERER_URL;
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
  await rm(root, { recursive: true, force: true });
});

test("a command finds the tools in the person's own folders when Desktop starts the server", async () => {
  test.setTimeout(150000);
  app = await electron.launch({ args: ["."], env, timeout: 60000 });
  page = await app.firstWindow();
  page.setDefaultTimeout(15000);
  await app.evaluate(({ dialog }, path) => {
    dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [path] });
  }, project);
  await page.getByRole("button", { name: "Open another project", exact: true }).click();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 60000 });
  await page.getByRole("button", { name: "New chat", exact: true }).click();
  const command = [
    "haarness-probe-local",
    "haarness-probe-cargo",
    "haarness-probe-both",
    "command -v haarness-probe-open || echo open-folder-skipped",
  ].join("; ");
  const box = page.getByRole("textbox", { name: "Message", exact: true });
  await box.fill(" !tool bash " + JSON.stringify({ command }));
  await page.getByRole("button", { name: /Send message|Send guidance/ }).click();
  const result = async () => {
    const threads: any[] = await api("listThreads", {});
    const thread = threads.find((item) => !item.isSubagent);
    if (!thread) return "";
    const items: any[] = await api("listItems", thread.id);
    return (items.find((item) => item.data.type === "tool_result")?.data.output ?? "") as string;
  };
  await expect.poll(result, { timeout: 30000 }).not.toBe("");
  await expect.poll(async () => (await api("activeTurns")).length, { timeout: 30000 }).toBe(0);
  const output = await result();
  // One line per command, in order: the person's own folder still comes
  // first for the name both folders have.
  expect(output).toMatch(/found-in-local-bin\s+found-in-cargo-bin\s+found-in-own-folder\s+open-folder-skipped/);
  expect(output).not.toContain("not found");
  // The transcript shows the same.
  await expect(page.locator(".transcript details.tool-call").last()).toContainText("haarness-probe-local");
});
