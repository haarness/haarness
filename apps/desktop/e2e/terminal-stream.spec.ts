import { test, expect, _electron as electron, type ElectronApplication, type Page } from "@playwright/test";
import { Haarness } from "@haarness/sdk";
import { mkdtemp, mkdir, writeFile, readFile, realpath, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";

// What the terminal panel sends its shell: the answers a program waits for,
// never answers to output shown before, and every key, also while its
// connection is being made again.
test.describe.configure({ mode: "serial" });

let app: ElectronApplication, page: Page, client: Haarness;
let temporary: string, workspace: string, binary: string;
let env: Record<string, string>;
const errors: string[] = [];
const read = (name: string) => readFile(join(temporary, name), "utf8").catch(() => "");

test.beforeAll(async () => {
  test.setTimeout(240000);
  temporary = await realpath(await mkdtemp(join(tmpdir(), "haarness-terminal-")));
  workspace = join(temporary, "workspace");
  await mkdir(join(workspace, ".haarness"), { recursive: true });
  await writeFile(join(workspace, ".haarness/config.toml"), '[defaults]\nprovider="echo"\nmodel="echo-1"\n');
  // Like fish or nvim at start, the shell asks the terminal for its device
  // attributes and waits for the answer, then records what it got. It asks
  // a second after it starts, so a test can have it ask while no view of the
  // terminal is connected.
  const shell = join(temporary, "query-shell");
  await writeFile(
    shell,
    `#!/bin/bash
sleep 1
printf '\\033[c'
if IFS= read -rs -t 10 -d c reply; then printf 'answered:%s' "\${#reply}"; else printf none; fi >> ${JSON.stringify(join(temporary, "start-answers.txt"))}
printf '\\n' >> ${JSON.stringify(join(temporary, "start-answers.txt"))}
export PS1='query-shell$ '
exec /bin/bash --noprofile --norc -i
`,
    { mode: 0o755 },
  );
  binary = resolve(process.env.HAARNESS_BIN ?? "../../target/debug/haarness");
  env = {
    ...(process.env as Record<string, string>),
    HAARNESS_BIN: binary,
    HAARNESS_HOME: join(temporary, "home"),
    HAARNESS_DATA_DIR: join(temporary, "data"),
    HAARNESS_DESKTOP_DATA: join(temporary, "desktop"),
    HAARNESS_TEST_PROVIDER: "1",
    HAARNESS_DESKTOP_TEST: "1",
    SHELL: shell,
    BASH_SILENCE_DEPRECATION_WARNING: "1",
  };
  delete env.ELECTRON_RUN_AS_NODE;
  delete env.ELECTRON_RENDERER_URL;
  app = await electron.launch({ args: ["."], env, timeout: 60000 });
  page = await app.firstWindow();
  page.on("pageerror", (error) => errors.push(error.message));
  await page.setViewportSize({ width: 1500, height: 950 });
  await app.evaluate(({ dialog }, directory) => {
    dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [directory] });
  }, workspace);
  await page.getByRole("region", { name: "Project home" }).getByRole("button", { name: "Open project", exact: true }).click();
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
});

test.afterAll(async () => {
  for (const terminal of (await client?.listPtys().catch(() => [])) ?? [])
    await client.deletePty(terminal.id).catch(() => {});
  client?.close();
  await app?.close();
  try {
    execFileSync(binary, ["--workspace", workspace, "--trust-project", "serve", "--stop"], {
      env,
      stdio: "ignore",
      timeout: 30_000,
    });
  } catch {}
  if (temporary) await rm(temporary, { recursive: true, force: true }).catch(() => {});
});

const input = () => page.locator(".xterm-helper-textarea");
const rows = () => page.locator(".terminal-view .xterm-rows");
async function prompt() {
  await expect(rows()).toContainText("query-shell$");
}

test("a new terminal answers the query its shell sends as it starts, and one shown again answers nothing twice", async () => {
  const toggle = page.getByRole("button", { name: "Toggle terminal", exact: true });
  await toggle.click();
  await expect(input()).toBeFocused();
  // Hidden at once, the panel is not connected when the shell asks; shown
  // again, it replays the question, which it never showed, and answers it.
  await page.getByRole("button", { name: "Hide terminal", exact: true }).click();
  await page.waitForTimeout(2000);
  expect(await read("start-answers.txt")).toBe("");
  await toggle.click();
  await expect.poll(() => read("start-answers.txt")).toBe("answered:6\n");
  await prompt();
  await input().fill(`cat -v > ${join(temporary, "typed.txt")}`);
  await input().press("Enter");
  // Shown again, the panel replays the shell's start, its query included.
  const hide = page.getByRole("button", { name: "Hide terminal", exact: true });
  await hide.click();
  await page.getByRole("button", { name: "Toggle terminal", exact: true }).click();
  await expect(input()).toBeFocused();
  await expect(rows()).toContainText("typed.txt");
  await input().pressSequentially("done");
  await input().press("Enter");
  await input().press("Control+d");
  await expect.poll(() => read("typed.txt")).toBe("done\n");
  expect(await read("start-answers.txt")).toBe("answered:6\n");
  expect(errors).toEqual([]);
});

test("keys typed while a terminal's connection is down reach the shell once it is back", async () => {
  await prompt();
  const keys = join(temporary, "keys.txt");
  await input().fill(`cat -v > ${keys}`);
  await input().press("Enter");
  await expect.poll(() => readFile(keys, "utf8").then(() => true, () => false)).toBe(true);
  // The socket ends as in an outage, and the panel connects again a moment
  // later; what is typed in between waits for it instead of vanishing.
  await app.evaluate(() => {
    const sockets = (globalThis as unknown as { haarnessTerminalSockets: { all(): { close(): void }[] } })
      .haarnessTerminalSockets;
    for (const socket of sockets.all()) socket.close();
  });
  await input().pressSequentially("kept");
  await input().press("Enter");
  await input().press("Control+d");
  await expect.poll(() => read("keys.txt")).toBe("kept\n");
  expect(errors).toEqual([]);
});

test("stepping through terminals quickly leaves the one shown connected", async () => {
  test.setTimeout(240_000);
  if (!(await page.locator(".terminal-panel").count()))
    await page.getByRole("button", { name: "Toggle terminal", exact: true }).click();
  await expect(input()).toBeFocused();
  await expect.poll(async () => (await read("start-answers.txt")).length).toBeGreaterThan(0);
  const add = page.getByRole("button", { name: "New terminal", exact: true });
  // Each new terminal is shown until its shell has its answer: a terminal
  // no view shows answers nothing until one does.
  const first = (await client.listPtys()).length;
  for (let count = first + 1; count <= first + 2; count++) {
    await add.click();
    await expect.poll(() => client.listPtys().then((items) => items.length)).toBe(count);
    await expect.poll(async () => (await read("start-answers.txt")).split("\n").filter(Boolean)).toEqual(
      Array(count).fill("answered:6"),
    );
  }
  await expect(input()).toBeFocused();
  for (let press = 0; press < 7; press++) await page.keyboard.press("Control+Tab");
  await expect(input()).toBeFocused();
  await prompt();
  await input().fill(`printf stepped > ${join(temporary, "stepped.txt")}`);
  await input().press("Enter");
  await expect.poll(() => read("stepped.txt"), { timeout: 30_000 }).toBe("stepped");
  expect(errors).toEqual([]);
});
