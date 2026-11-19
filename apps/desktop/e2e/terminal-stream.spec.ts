import { test, expect, _electron as electron, type ElectronApplication, type Page } from "@playwright/test";
import { Haarness } from "@haarness/sdk";
import { mkdtemp, mkdir, writeFile, readFile, realpath, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";
import { stopChatHome } from "./fixtures/chat-home";

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
  stopChatHome(binary, env);
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

/** Whether `name` (in the test folder) exists yet. */
const exists = (name: string) => readFile(join(temporary, name)).then(() => true, () => false);
/** Starts `command`, which writes to the file `name`, and waits for the file. */
async function run(command: string, name: string) {
  await prompt();
  await input().fill(command);
  await input().press("Enter");
  await expect.poll(() => exists(name)).toBe(true);
}
/** Ends or resumes the outage main simulates for terminal sockets. */
async function outage(on: boolean) {
  await app.evaluate((_, on) => {
    const test = globalThis as unknown as {
      haarnessTerminalOutage: boolean;
      haarnessTerminalSockets: { all(): { close(): void }[] };
    };
    test.haarnessTerminalOutage = on;
    if (on) for (const socket of test.haarnessTerminalSockets.all()) socket.close();
  }, on);
}

test("a terminal cut off from its server says so, offers Retry and New terminal, and keeps its shell", async () => {
  test.setTimeout(180_000);
  if (!(await page.locator(".terminal-panel").count()))
    await page.getByRole("button", { name: "Toggle terminal", exact: true }).click();
  await run(`cat -v > ${join(temporary, "outage.txt")}`, "outage.txt");
  const shells = (await client.listPtys()).map((item) => item.id);
  const reconnecting = page.getByRole("status").filter({ hasText: "Reconnecting…" });
  const alert = page.getByRole("alert").filter({ hasText: "Terminal disconnected" });
  await outage(true);
  await expect(reconnecting).toBeVisible();
  await expect(alert).toContainText("Haarness can't reach this terminal. Its shell may still be running.", {
    timeout: 30_000,
  });
  await expect(alert.getByRole("button", { name: "New terminal", exact: true })).toBeVisible();
  // The notice leaves the output it covers selectable: a point in the
  // terminal beside or below it reaches xterm, not the notice.
  const reachable = await page.locator(".terminal-view").evaluate((view) => {
    const box = view.getBoundingClientRect();
    return [
      [box.left + 12, box.top + 12],
      [box.left + box.width / 2, box.top + box.height * 0.75],
    ].every(([x, y]) => !!document.elementFromPoint(x, y)?.closest(".terminal-view"));
  });
  expect(reachable).toBe(true);
  // Typed while it is cut off, kept for the shell.
  await input().pressSequentially("kept");
  // Retry says it is reconnecting at once, while the outage lasts.
  await alert.getByRole("button", { name: "Retry", exact: true }).click();
  await expect(reconnecting).toBeVisible();
  await expect(alert).toHaveCount(0);
  await outage(false);
  await expect(reconnecting).toHaveCount(0);
  await input().press("Enter");
  await input().press("Control+d");
  await expect.poll(() => read("outage.txt")).toBe("kept\n");
  // The same shells: nothing was replaced.
  expect((await client.listPtys()).map((item) => item.id)).toEqual(shells);

  // New terminal from the notice opens another terminal and shows it.
  await outage(true);
  await expect(alert).toBeVisible({ timeout: 30_000 });
  await outage(false);
  await alert.getByRole("button", { name: "New terminal", exact: true }).click();
  await expect.poll(() => client.listPtys().then((items) => items.length)).toBe(shells.length + 1);
  const added = (await client.listPtys()).find((item) => !shells.includes(item.id))!;
  await expect(page.locator(`[data-cycle-tab="${added.id}"]`)).toHaveAttribute("aria-current", "true");
  await expect(alert).toHaveCount(0);
  await prompt();
  await client.deletePty(added.id);
  await expect(page.locator(`[data-cycle-tab="${added.id}"]`)).toHaveCount(0);
  expect(errors).toEqual([]);
});

test("a terminal scrolled up opens where it was when shown again", async () => {
  await prompt();
  await input().fill("seq -f 'row %04g' 1 400");
  await input().press("Enter");
  await expect(rows()).toContainText("row 0400");
  const top = () => page.locator(".terminal-view .xterm-rows > div").first().textContent();
  await page.locator(".terminal-view").hover();
  await page.mouse.wheel(0, -2400);
  await expect.poll(top).toMatch(/^row 0[0-3]\d\d/);
  // Let the scroll settle before reading where it is.
  await page.waitForTimeout(300);
  const place = await top();
  const shown = page.locator('[data-cycle-tab][aria-current="true"]');
  const id = await shown.getAttribute("data-cycle-tab");
  // The panel hidden and shown again.
  await page.getByRole("button", { name: "Hide terminal", exact: true }).click();
  await page.getByRole("button", { name: "Toggle terminal", exact: true }).click();
  await expect(input()).toBeFocused();
  await expect(shown).toHaveAttribute("data-cycle-tab", id!);
  await expect.poll(top).toBe(place);
  // Another tab and back, from the keyboard the tab step leaves in the
  // terminal it shows.
  await page.keyboard.press("Control+Tab");
  await expect(shown).not.toHaveAttribute("data-cycle-tab", id!);
  await expect.poll(top).not.toBe(place);
  // The step puts the keyboard in the terminal it shows a moment later (a
  // task after drawing it); the next key waits for it, as a person's would.
  await expect(input()).toBeFocused();
  await page.keyboard.press("Control+Shift+Tab");
  await expect(shown).toHaveAttribute("data-cycle-tab", id!);
  await expect.poll(top).toBe(place);
  // Typing goes to the shell and takes the view back to its prompt.
  await input().pressSequentially("x");
  await expect.poll(top).not.toBe(place);
  await input().press("Backspace");
  // Scrolled up again, then, while hidden, the scrollback is erased and
  // refilled past that line: it holds other text, so the view opens at the
  // bottom, at the prompt.
  await page.locator(".terminal-view").hover();
  await page.mouse.wheel(0, -2400);
  await expect.poll(top).toMatch(/^row 0[0-3]\d\d/);
  await page.waitForTimeout(300);
  await page.getByRole("button", { name: "Hide terminal", exact: true }).click();
  const socket = await client.connectPty(id!);
  socket.send(`printf '\\033[3J'; seq -f 'new %04g' 1 3000; touch ${join(temporary, "refilled")}\r`);
  await expect.poll(() => exists("refilled")).toBe(true);
  socket.close();
  await page.getByRole("button", { name: "Toggle terminal", exact: true }).click();
  await expect(input()).toBeFocused();
  await expect(page.locator(".terminal-view .xterm-rows")).toContainText("new 3000");
  await expect.poll(top).toMatch(/^new 29\d\d/);
  expect(errors).toEqual([]);
});

test("a terminal shown again past two megabytes of output keeps a program's cursor keys and bracketed paste", async () => {
  test.setTimeout(240_000);
  // The program turns both on, then writes more than the server keeps.
  await run(
    `printf '\\033[?1h\\033[?2004h'; seq 1 400000; printf 'SEQ_''DONE\\n'; cat -v > ${join(temporary, "modes.txt")}`,
    "modes.txt",
  );
  await expect(rows()).toContainText("SEQ_DONE", { timeout: 120_000 });
  await page.getByRole("button", { name: "Hide terminal", exact: true }).click();
  await page.getByRole("button", { name: "Toggle terminal", exact: true }).click();
  await expect(input()).toBeFocused();
  await expect(rows()).toContainText("SEQ_DONE", { timeout: 60_000 });
  await input().press("ArrowUp");
  await input().evaluate((element) => {
    const data = new DataTransfer();
    data.setData("text/plain", "pasted");
    element.dispatchEvent(new ClipboardEvent("paste", { clipboardData: data, bubbles: true, cancelable: true }));
  });
  await input().press("Enter");
  await input().press("Control+d");
  await expect.poll(() => read("modes.txt")).toBe("^[OA^[[200~pasted^[[201~\n");
  expect(errors).toEqual([]);
});

test("Option types what the layout composes until Settings makes it a Meta key", async () => {
  test.skip(process.platform !== "darwin", "only macOS composes characters with Option");
  await run(`stty -icanon -echo; cat -u > ${join(temporary, "option.txt")}`, "option.txt");
  const cdp = await page.context().newCDPSession(page);
  // As macOS reports Option+B on a US layout: the key's own code, and the
  // composed character as its text.
  const option = async (side: "AltLeft" | "AltRight") => {
    const location = side === "AltLeft" ? 1 : 2;
    const alt = { key: "Alt", code: side, windowsVirtualKeyCode: 18, location };
    const b = { key: "∫", code: "KeyB", windowsVirtualKeyCode: 66 };
    await cdp.send("Input.dispatchKeyEvent", { type: "rawKeyDown", modifiers: 1, ...alt });
    await cdp.send("Input.dispatchKeyEvent", { type: "keyDown", modifiers: 1, text: "∫", unmodifiedText: "b", ...b });
    await cdp.send("Input.dispatchKeyEvent", { type: "keyUp", modifiers: 1, ...b });
    await cdp.send("Input.dispatchKeyEvent", { type: "keyUp", modifiers: 0, ...alt });
  };
  const settings = () => page.getByRole("dialog", { name: "Settings", exact: true });
  const choose = async (value: string) => {
    await page.getByRole("button", { name: "Settings", exact: true }).click();
    await settings().getByRole("textbox", { name: "Filter settings" }).fill("Option as Meta");
    await settings().getByRole("button", { name: "Option as Meta key Settings › General", exact: true }).click();
    await settings().getByRole("combobox", { name: "Option as Meta key", exact: true }).click();
    await page.getByRole("option", { name: value, exact: true }).click();
    await expect(settings().getByRole("combobox", { name: "Option as Meta key", exact: true })).toContainText(value);
    await settings().getByRole("button", { name: "Close settings", exact: true }).click();
    await input().focus();
  };
  await input().focus();
  await option("AltLeft");
  await expect.poll(() => read("option.txt")).toBe("∫");
  await choose("Left Option");
  await option("AltLeft");
  await option("AltRight");
  await expect.poll(() => read("option.txt")).toBe("∫\u001bb∫");
  await choose("Both Option keys");
  await option("AltRight");
  await expect.poll(() => read("option.txt")).toBe("∫\u001bb∫\u001bb");
  await choose("Off");
  await cdp.detach();
  await input().press("Control+c");
  await input().pressSequentially("stty sane");
  await input().press("Enter");
  expect(errors).toEqual([]);
});

test("a terminal whose connection went dead while the machine slept connects again on waking, its output kept", async () => {
  await prompt();
  await input().fill("printf 'before-%s\\n' sleep");
  await input().press("Enter");
  await expect(rows()).toContainText("before-sleep");
  // Nothing comes in any more, as after a sleep on another network, while
  // the socket still reads as open; what is typed still leaves.
  await app.evaluate(() => {
    // A socket that stopped answering is given up within a second, here only.
    (globalThis as { haarnessTerminalPingTimeoutMs?: number }).haarnessTerminalPingTimeoutMs = 1000;
    const sockets = (globalThis as unknown as {
      haarnessTerminalSockets: { all(): { socket: { _socket: { pause(): void } } }[] };
    }).haarnessTerminalSockets;
    for (const socket of sockets.all()) socket.socket._socket.pause();
  });
  await input().fill("printf 'after-%s\\n' wake");
  await input().press("Enter");
  await page.waitForTimeout(1500);
  await expect(rows()).not.toContainText("after-wake");
  await app.evaluate(({ powerMonitor }) => powerMonitor.emit("resume"));
  await expect(rows()).toContainText("after-wake", { timeout: 10_000 });
  await expect(rows()).toContainText("before-sleep");
  await expect(page.getByText("Terminal disconnected")).toHaveCount(0);
  await app.evaluate(() => {
    delete (globalThis as { haarnessTerminalPingTimeoutMs?: number }).haarnessTerminalPingTimeoutMs;
  });
  expect(errors).toEqual([]);
});

/** The text of each row on the terminal's screen, without trailing blanks. */
const screen = () =>
  rows()
    .locator(":scope > div")
    .evaluateAll((items) => items.map((item) => (item.textContent ?? "").trimEnd()));
/** The rows that hold text. */
const written = async () => (await screen()).filter(Boolean);
/** Runs `command` and waits for the prompt after its output. */
async function settle(command: string, last: string) {
  await prompt();
  await input().fill(command);
  await input().press("Enter");
  await expect(rows()).toContainText(last);
  await expect.poll(async () => (await written()).at(-1)).toBe("query-shell$");
}
/** Hides the panel and shows it again: a new view, which replays the output. */
async function showAgain() {
  await page.getByRole("button", { name: "Hide terminal", exact: true }).click();
  await page.getByRole("button", { name: "Toggle terminal", exact: true }).click();
  await expect(input()).toBeFocused();
}
/** Right-clicks the terminal's blank corner; the labels of the menu main
 * built, and `choose` clicked in it. Main records menus in tests instead of
 * showing them (`haarnessContextMenus`). */
async function terminalMenu(choose?: string) {
  await app.evaluate(() => void ((globalThis as { haarnessContextMenus?: Electron.Menu[] }).haarnessContextMenus = []));
  const box = (await page.locator(".terminal-view .xterm-screen").boundingBox())!;
  await page.mouse.click(box.x + box.width - 20, box.y + box.height - 20, { button: "right" });
  let labels: string[] = [];
  await expect
    .poll(async () => {
      labels = await app.evaluate((_, choose) => {
        const menu = (globalThis as { haarnessContextMenus?: Electron.Menu[] }).haarnessContextMenus?.at(-1);
        if (choose) menu?.items.find((item) => item.label === choose)?.click();
        return menu?.items.map((item) => (item.type === "separator" ? "-" : item.label)) ?? [];
      }, choose);
      return labels.length;
    })
    .toBeGreaterThan(0);
  return labels;
}

test("Clear in the terminal's menu leaves only the prompt line, and a terminal shown again stays cleared", async () => {
  if (!(await page.locator(".terminal-panel").count()))
    await page.getByRole("button", { name: "Toggle terminal", exact: true }).click();
  await settle("seq -f 'line %03g' 1 200", "line 200");
  expect(await terminalMenu("Clear")).toEqual(["Paste", "-", "Clear"]);
  // The prompt line is now the first, and nothing is left above it.
  await expect.poll(screen).toEqual(["query-shell$", ...Array((await screen()).length - 1).fill("")]);
  await page.locator(".terminal-view").hover();
  await page.mouse.wheel(0, -2400);
  await page.waitForTimeout(300);
  expect(await written()).toEqual(["query-shell$"]);
  // The shell goes on as before.
  await settle("printf 'after-%s\\n' clear", "after-clear");
  const after = await written();
  expect(after).toEqual(["query-shell$ printf 'after-%s\\n' clear", "after-clear", "query-shell$"]);
  // Shown again, the view replays the server's output, cleared lines and
  // all, and clears itself where it was cleared.
  await showAgain();
  await expect.poll(written).toEqual(after);
  await showAgain();
  await expect.poll(written).toEqual(after);
  // A full-screen program keeps its screen: its menu has no Clear.
  await input().fill("seq -f 'page %03g' 1 300 | LESS= less");
  await input().press("Enter");
  await expect(rows()).toContainText("page 001");
  expect(await terminalMenu()).toEqual(["Paste"]);
  await input().focus();
  await input().press("q");
  await expect.poll(async () => (await written()).at(-1)).toBe("query-shell$");
  expect(await written()).toEqual([...after.slice(0, -1), "query-shell$ seq -f 'page %03g' 1 300 | LESS= less", "query-shell$"]);
  expect(errors).toEqual([]);
});

test("⌘K with the keyboard in the terminal clears it, does nothing under a full-screen program, and opens Search everywhere else", async () => {
  test.skip(process.platform !== "darwin", "⌘K clears the terminal on macOS only");
  // ⌘K as the window gets it from the system: main hears it first
  // (before-input-event), which Playwright's own keys skip.
  const commandK = () =>
    app.evaluate(({ BrowserWindow }) => {
      const contents = BrowserWindow.getAllWindows()[0].webContents;
      contents.sendInputEvent({ type: "keyDown", keyCode: "K", modifiers: ["meta"] });
      contents.sendInputEvent({ type: "keyUp", keyCode: "K", modifiers: ["meta"] });
    });
  // Where main thinks the keyboard is: in a terminal, or not.
  const inTerminal = (inside: boolean) =>
    expect
      .poll(() => app.evaluate(() => (globalThis as unknown as { haarnessTerminalKeyboard: Set<number> }).haarnessTerminalKeyboard.size))
      .toBe(inside ? 1 : 0);
  // The ⌘K presses the page itself heard, and whether it held them back.
  const heard = () =>
    page.evaluate(() => {
      const record = window as unknown as { heardKeys?: string[] };
      if (!record.heardKeys) {
        record.heardKeys = [];
        document.addEventListener("keydown", (event) => {
          if (event.metaKey && event.key.toLowerCase() === "k") record.heardKeys!.push(event.defaultPrevented ? "prevented" : "passed");
        });
      }
      return record.heardKeys.splice(0);
    });
  await heard();
  if (!(await page.locator(".terminal-panel").count()))
    await page.getByRole("button", { name: "Toggle terminal", exact: true }).click();
  await settle("seq -f 'line %03g' 1 200", "line 200");
  // A word selected in the output goes with it.
  const word = (await rows().locator(":scope > div").filter({ hasText: "line 190" }).last().boundingBox())!;
  await page.mouse.dblclick(word.x + 12, word.y + word.height / 2);
  const selection = page.locator(".terminal-view .xterm-selection div");
  await expect(selection).not.toHaveCount(0);
  await input().focus();
  await inTerminal(true);
  await commandK();
  await expect.poll(written).toEqual(["query-shell$"]);
  await expect(selection).toHaveCount(0);
  // Main took the key: neither the page nor the menu's Search heard it.
  expect(await heard()).toEqual([]);
  await expect(page.locator(".palette-dialog")).toHaveCount(0);
  await showAgain();
  await expect.poll(written).toEqual(["query-shell$"]);
  // A page navigation that starts and is refused (a file dropped on the
  // window, say) leaves the page as it was, and main still knows the
  // terminal has the keyboard; one that takes place starts a page without
  // it. (As the window reports them: a refused one never finishes, which
  // Playwright would wait for.)
  await inTerminal(true);
  const navigation = (event: "did-start-navigation" | "did-navigate") =>
    app.evaluate(({ BrowserWindow }, event) => {
      const contents = BrowserWindow.getAllWindows()[0].webContents;
      if (event === "did-start-navigation")
        contents.emit(event, { url: "file:///refused", isMainFrame: true, isSameDocument: false });
      else contents.emit(event, {}, "file:///refused", 200, "OK");
      return (globalThis as unknown as { haarnessTerminalKeyboard: Set<number> }).haarnessTerminalKeyboard.size;
    }, event);
  expect(await navigation("did-start-navigation")).toBe(1);
  expect(await navigation("did-navigate")).toBe(0);
  // The keyboard back in the terminal says so again.
  await input().blur();
  await input().focus();
  await inTerminal(true);

  // Under less, ⌘K leaves its screen and the lines behind it alone.
  await settle("printf 'before-%s\\n' less", "before-less");
  const before = await written();
  await input().fill("seq -f 'page %03g' 1 300 | LESS= less");
  await input().press("Enter");
  await expect(rows()).toContainText("page 001");
  const paged = await screen();
  await inTerminal(true);
  await commandK();
  await page.waitForTimeout(500);
  expect(await screen()).toEqual(paged);
  expect(await heard()).toEqual([]);
  await expect(page.locator(".palette-dialog")).toHaveCount(0);
  await input().press("q");
  await expect.poll(async () => (await written()).at(-1)).toBe("query-shell$");
  expect(await written()).toEqual([...before.slice(0, -1), "query-shell$ seq -f 'page %03g' 1 300 | LESS= less", "query-shell$"]);

  // With the keyboard in the message box, ⌘K reaches the page untouched, so
  // the menu runs Search (a system menu takes no synthetic key, so the test
  // runs the menu item that carries the key), and the terminal keeps its lines.
  const shown = await written();
  await page.getByRole("textbox", { name: "Message", exact: true }).click();
  await inTerminal(false);
  await commandK();
  await expect.poll(heard).toEqual(["passed"]);
  expect(await written()).toEqual(shown);
  const accelerator = await app.evaluate(({ BrowserWindow, Menu }) => {
    const find = (items: Electron.MenuItem[]): Electron.MenuItem | undefined =>
      items.reduce<Electron.MenuItem | undefined>(
        (found, item) => found ?? (item.accelerator === "CmdOrCtrl+K" ? item : item.submenu && find(item.submenu.items)),
        undefined,
      );
    const item = find(Menu.getApplicationMenu()?.items ?? []);
    // The menu acts for the window in front, which a key pressed in it is;
    // a test's window may not be.
    const front = BrowserWindow.getFocusedWindow;
    BrowserWindow.getFocusedWindow = () => BrowserWindow.getAllWindows()[0];
    try {
      item?.click();
    } finally {
      BrowserWindow.getFocusedWindow = front;
    }
    return item?.label;
  });
  expect(accelerator).toBe("Command palette");
  await expect(page.locator(".palette-dialog")).toHaveCount(1);
  await page.keyboard.press("Escape");
  await expect(page.locator(".palette-dialog")).toHaveCount(0);
  expect(errors).toEqual([]);
});

// Last: it replaces the server the other tests use.
test("after the server is replaced, the terminal it no longer has gives way to one new shell in every window", async () => {
  test.setTimeout(240_000);
  await prompt();
  // A second window on the project, its terminal panel open too.
  const opened = app.waitForEvent("window");
  await app.evaluate(({ Menu }) => {
    const find = (items: Electron.MenuItem[]): Electron.MenuItem | undefined =>
      items.reduce<Electron.MenuItem | undefined>(
        (found, item) => found ?? (item.label === "New window" ? item : item.submenu && find(item.submenu.items)),
        undefined,
      );
    find(Menu.getApplicationMenu()?.items ?? [])?.click();
  });
  const second = await opened;
  second.on("pageerror", (error) => errors.push(error.message));
  await second.setViewportSize({ width: 1500, height: 950 });
  const home = second.getByRole("button", { name: "Open another project", exact: true });
  await expect(home.or(second.locator('.application[data-project-ready="true"]'))).toBeVisible({ timeout: 90_000 });
  if (await home.isVisible()) await home.click();
  await expect(second.locator(".application")).toHaveAttribute("data-project-ready", "true", { timeout: 90_000 });
  await second.getByRole("button", { name: "Toggle terminal", exact: true }).click();
  await expect(second.locator(".terminal-view .xterm-rows")).toContainText("query-shell$");
  const old = (await client.listPtys()).map((item) => item.id);
  const instance = (await client.info()).serverInstanceId;
  await client.shutdownServer();
  client.close();
  const id = "local:" + workspace;
  await expect
    .poll(
      () =>
        page
          .evaluate(async (id) => (await window.haarness.api<{ serverInstanceId: string }>(id, "info", [])).serverInstanceId, id)
          .catch(() => instance),
      { timeout: 90_000 },
    )
    .not.toBe(instance);
  const connection = JSON.parse(
    execFileSync(binary, ["--workspace", workspace, "--trust-project", "desktop"], {
      env,
      input: '{"action":"connect"}',
      encoding: "utf8",
      timeout: 120000,
    }),
  );
  client = new Haarness({ baseUrl: connection.url, token: connection.token });
  await expect.poll(() => client.listPtys().then((items) => items.length), { timeout: 60_000 }).toBe(1);
  // Both windows looked for a terminal at once; neither opened a second one.
  await expect
    .poll(async () => (await Promise.all([page, second].map((window) => window.locator("[data-cycle-tab]").count()))).join(), {
      timeout: 60_000,
    })
    .toBe("1,1");
  await page.waitForTimeout(2000);
  const [shell, ...others] = await client.listPtys();
  expect(others).toEqual([]);
  expect(old).not.toContain(shell.id);
  // A window asking for a first terminal now is handed this one, and told it
  // did not start it, so it adopts its output instead of answering it again.
  for (const view of [page, second]) {
    const handed = await view.evaluate(
      (id) => window.haarness.api<{ id: string; created?: boolean }>(id, "createPty", [{ cols: 100, rows: 20, ifNone: true }]),
      "local:" + workspace,
    );
    expect(handed).toMatchObject({ id: shell.id });
    expect(handed.created).toBe(false);
  }
  // Only the new terminal is listed, and it is the one shown and connected.
  for (const window of [page, second]) {
    await expect(window.locator("[data-cycle-tab]")).toHaveCount(1);
    await expect(window.locator(`[data-cycle-tab="${shell.id}"]`)).toHaveAttribute("aria-current", "true");
  }
  // The shell asked both windows for their device attributes as it started;
  // one answered, so nothing was left on its command line, and either
  // window types into it.
  await expect.poll(() => read("start-answers.txt").then((text) => text.trim().split("\n").at(-1))).toBe("answered:6");
  const secondInput = second.locator(".xterm-helper-textarea");
  await secondInput.fill(`printf second > ${join(temporary, "second.txt")}`);
  await secondInput.press("Enter");
  await expect.poll(() => read("second.txt"), { timeout: 30_000 }).toBe("second");
  await second.close();
  await prompt();
  await input().fill(`printf replaced > ${join(temporary, "replaced.txt")}`);
  await input().press("Enter");
  await expect.poll(() => read("replaced.txt"), { timeout: 30_000 }).toBe("replaced");
  expect(errors).toEqual([]);
});
