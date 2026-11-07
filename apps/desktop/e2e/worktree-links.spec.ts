import { test, expect, _electron as electron } from "@playwright/test";
import { Haarness } from "@haarness/sdk";
import { mkdtemp, mkdir, writeFile, readFile, realpath, rm, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";
import { stopChatHome } from "./fixtures/chat-home";

// A worktree whose link to its repository broke (its repository moved, or
// Git's record of it was pruned) cannot be checked or removed by Git. Remove
// worktree then forgets it and leaves its folder, and the notice says so in
// one line, in light, dark and right to left.
test("removing a worktree whose link to its repository broke forgets it and keeps its folder", async ({}, info) => {
  test.setTimeout(240000);
  const temporary = await realpath(await mkdtemp(join(process.env.HAARNESS_E2E_DIR ?? tmpdir(), "haarness-worktree-links-")));
  const workspace = join(temporary, "workspace");
  await mkdir(join(workspace, ".haarness"), { recursive: true });
  await writeFile(join(workspace, ".haarness/config.toml"), '[defaults]\nprovider="echo"\nmodel="echo-1"\n');
  await writeFile(join(workspace, "tracked.txt"), "base\n");
  for (const args of [["init", "-b", "main"], ["add", "."], ["-c", "user.name=Fixture", "-c", "user.email=fixture@example.test", "commit", "-m", "Initial fixture"]])
    execFileSync("git", args, { cwd: workspace, stdio: "ignore" });
  const binary = resolve(process.env.HAARNESS_BIN ?? "../../target/debug/haarness");
  const env: Record<string, string> = { ...process.env, HAARNESS_BIN: binary, HAARNESS_HOME: join(temporary, "home"), HAARNESS_DATA_DIR: join(temporary, "data"), HAARNESS_DESKTOP_DATA: join(temporary, "desktop"), HAARNESS_TEST_PROVIDER: "1", HAARNESS_DESKTOP_TEST: "1" };
  delete env.ELECTRON_RUN_AS_NODE;
  delete env.ELECTRON_RENDERER_URL;
  const app = await electron.launch({ args: ["."], env, timeout: 60000 });
  const page = await app.firstWindow();
  const errors: string[] = [];
  page.on("pageerror", error => errors.push(error.message));
  // Worktrees the test made, whose servers it stops when it ends.
  const made: string[] = [];
  const theme = async (value: "light" | "dark") => {
    await app.evaluate(({ nativeTheme }, value) => { nativeTheme.themeSource = value; }, value);
    await page.emulateMedia({ colorScheme: value });
    await expect.poll(() => page.evaluate(() => matchMedia("(prefers-color-scheme: dark)").matches)).toBe(value === "dark");
  };
  try {
    await page.setViewportSize({ width: 1280, height: 820 });
    await app.evaluate(({ dialog }, directory) => { dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [directory] }); }, workspace);
    await page.getByRole("button", { name: "Open another project", exact: true }).click();
    await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 60000 });
    const project = page.locator('.project-group[aria-label="Project workspace"]');
    await project.getByRole("button", { name: "Project options for workspace", exact: true }).click();
    await page.getByRole("menuitem", { name: "Enable worktree view", exact: true }).click();
    const create = async (name: string) => {
      await project.getByRole("button", { name: "New worktree", exact: true }).click();
      const dialog = page.getByRole("dialog", { name: "New worktree", exact: true });
      await dialog.getByRole("textbox", { name: "Name", exact: true }).fill(name);
      await dialog.getByRole("button", { name: "Create worktree", exact: true }).click();
      const tree = project.getByRole("region", { name: `Checkout ${name}`, exact: true });
      await expect(tree).toBeVisible({ timeout: 45000 });
      await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 60000 });
      return tree;
    };
    const connection = JSON.parse(execFileSync(binary, ["--workspace", workspace, "--trust-project", "desktop"], { env, input: '{"action":"connect"}', encoding: "utf8", timeout: 60000 }));
    const client = new Haarness({ baseUrl: connection.url, token: connection.token });
    const registry = () => client.workspacesList();
    const shots = [
      ["light", "Unlinked light", "en"],
      ["dark", "Unlinked dark", "en"],
      ["dark", "Unlinked rtl", "ar"],
    ] as const;
    for (const [mode, name, language] of shots) {
      const tree = await create(name);
      const folder = (await registry()).workspaces.find(item => item.name === name)!.path;
      made.push(folder);
      // Work in the folder, then cut its link: Git can no longer reach it.
      await writeFile(join(folder, "notes.txt"), "keep me\n");
      await writeFile(join(folder, ".git"), "gitdir: /nowhere/at/all/.git/worktrees/unlinked\n");
      await theme(mode);
      await tree.getByRole("button", { name: `Checkout options for ${name}`, exact: true }).click();
      await page.getByRole("menuitem", { name: "Remove worktree…", exact: true }).click();
      await page.getByRole("dialog", { name: "Remove worktree?", exact: true }).getByRole("button", { name: "Remove worktree", exact: true }).click();
      const toast = page.locator(".toast");
      await expect(toast).toHaveText("Worktree removed. Its folder no longer links to its repository, so it was left on disk.");
      await expect(tree).toHaveCount(0);
      if (language === "ar") {
        // The notice is read in the interface language as it is shown.
        await page.evaluate(() => (window as any).haarness.preferences({ language: "ar" }));
        await expect(page.locator("html")).toHaveAttribute("dir", "rtl");
        await expect(toast).toHaveText("تمت إزالة شجرة العمل. لم يعد مجلدها مرتبطًا بمستودعها، لذا تُرك على القرص.");
      }
      // Past the notice's fade-in.
      await page.waitForTimeout(400);
      await page.screenshot({ path: info.outputPath(`unlinked-${mode}${language === "ar" ? "-rtl" : ""}.png`) });
      await expect(page.locator(".error-banner")).toHaveCount(0);
      expect(await readFile(join(folder, "notes.txt"), "utf8")).toBe("keep me\n");
      expect((await stat(folder)).isDirectory()).toBe(true);
      expect((await registry()).workspaces.some(item => item.name === name)).toBe(false);
      await page.evaluate(() => (window as any).haarness.preferences({ language: "en" }));
      await expect(toast).toHaveCount(0, { timeout: 10000 });
    }

    // A worktree whose link works is still removed whole, with the usual notice.
    await theme("light");
    const tree = await create("Linked");
    const folder = (await registry()).workspaces.find(item => item.name === "Linked")!.path;
    made.push(folder);
    await tree.getByRole("button", { name: "Checkout options for Linked", exact: true }).click();
    await page.getByRole("menuitem", { name: "Remove worktree…", exact: true }).click();
    await page.getByRole("dialog", { name: "Remove worktree?", exact: true }).getByRole("button", { name: "Remove worktree", exact: true }).click();
    await expect(page.locator(".toast")).toHaveText("Worktree removed.");
    await expect(tree).toHaveCount(0);
    await expect.poll(() => stat(folder).then(() => "there", () => "gone")).toBe("gone");
    expect(errors).toEqual([]);
  } finally {
    await app.close().catch(() => undefined);
    for (const folder of [workspace, ...made])
      try { execFileSync(binary, ["--workspace", folder, "serve", "--stop"], { env, stdio: "ignore", timeout: 30000 }); } catch {}
    await stopChatHome(binary, env);
    await rm(temporary, { recursive: true, force: true });
  }
});
