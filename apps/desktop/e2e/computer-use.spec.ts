/**
 * Settings › Computer use and the computer use approval dock.
 *
 * Desktop's IPC handlers are wrapped in the main process, so the renderer
 * runs unchanged against a stand-in for the server's `computer-use` resource
 * and its pending requests: nothing here turns computer use on, asks macOS
 * for a permission or touches the screen. The stand-in records every request
 * the pane sends, which the tests check.
 */
import { test, expect, _electron as electron, type ElectronApplication, type Page } from "@playwright/test";
import { mkdtemp, mkdir, readFile, writeFile, realpath, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";
import { Haarness, type PendingPermissionInfo, type Thread } from "@haarness/sdk";
import { stopChatHome } from "./fixtures/chat-home";
import type { ComputerUseStatus } from "../src/renderer/computer-use";

let app: ElectronApplication, page: Page, temporary: string;
let env: Record<string, string>, client: Haarness, thread: Thread, directory: string;
const errors: string[] = [];
const evidence = process.env.HAARNESS_COMPUTER_EVIDENCE;
const binary = resolve(process.env.HAARNESS_BIN ?? "../../target/debug/haarness");
/** The stand-in for the Mac helper the computer use runtime tests use: it answers as the helper would and never
 * touches the screen. A live run uses the real one. */
const standIn = resolve("../../crates/haarness-computer/tests/fake-helper.py");
test.describe.configure({ mode: "serial" });
test.skip(process.platform !== "darwin", "Computer use is offered on macOS only");

const terms = {
  title: "Terms for computer use",
  intro:
    "Turning on computer use gives Haarness agents control of this Mac much like your own: they capture the screen, read app contents via macOS Accessibility, and use the mouse and keyboard. By switching it on you accept every point below.",
  sections: [
    { heading: "Use it for", items: ["Developing, testing, troubleshooting and showing off software made by you or your team."] },
    { heading: "Keep in mind", items: ["App contents can't be trusted: a web page, file or message might carry instructions aimed at the agent. Keep an eye on it and press Stop if anything seems off."] },
  ],
};
const status = (change: Partial<ComputerUseStatus> = {}): ComputerUseStatus => ({
  supported: true,
  platform: "macos",
  console: true,
  enabled: false,
  switch: false,
  terms: { version: "2", accepted: false, acceptedAt: null, text: terms },
  permissions: { accessibility: false, screenRecording: false },
  screen: { locked: false, asleep: false },
  host: { bundle: "com.apple.Terminal", name: "Terminal", haarness: false },
  paused: false,
  active: null,
  settings: { defaultAct: "ask", ring: true, apps: [] },
  ...change,
});

interface Fake {
  active: boolean;
  status: ComputerUseStatus;
  calls: Record<string, any>[];
  permissions: PendingPermissionInfo[];
  replies: { id: string; decision: string }[];
}

test.beforeAll(async () => {
  test.setTimeout(240000);
  temporary = await realpath(await mkdtemp(join(tmpdir(), "haarness-computer-use-")));
  env = Object.fromEntries(Object.entries(process.env).filter((x): x is [string, string] => x[1] !== undefined));
  Object.assign(env, {
    HAARNESS_BIN: binary,
    HAARNESS_HOME: join(temporary, "home"),
    HAARNESS_DATA_DIR: join(temporary, "state"),
    HAARNESS_DESKTOP_DATA: join(temporary, "desktop"),
    HAARNESS_TEST_PROVIDER: "1",
    HAARNESS_DESKTOP_TEST: "1",
  });
  if (!process.env.HAARNESS_COMPUTER_LIVE)
    Object.assign(env, { HAARNESS_MAC_HELPER: standIn, FAKE_HELPER_LOG: join(temporary, "helper.log") });
  delete env.ELECTRON_RUN_AS_NODE;
  directory = join(temporary, "computer-project");
  await mkdir(join(directory, ".haarness"), { recursive: true });
  await writeFile(join(directory, ".haarness/config.toml"), '[defaults]\nprovider="echo"\nmodel="echo-1"\napproval_mode="review"\n');
  for (const args of [
    ["init", "-b", "main"],
    ["add", "."],
    ["-c", "user.name=Sam Rivera", "-c", "user.email=sam@example.test", "commit", "-m", "Fixture"],
  ])
    execFileSync("git", args, { cwd: directory, stdio: "ignore" });
  if (evidence) await mkdir(evidence, { recursive: true });
  app = await electron.launch({ args: ["."], env, timeout: 120000 });
  page = await app.firstWindow();
  page.on("pageerror", (error) => errors.push(error.message));
  await app.evaluate(({ BrowserWindow }) => {
    const win = BrowserWindow.getAllWindows()[0];
    win.setContentSize(1400, 940);
    win.show();
  });
  await installStub();
  await stub({ active: true, status: status(), calls: [], permissions: [], replies: [] });
  await app.evaluate(({ dialog }, directory) => {
    dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [directory] });
  }, directory);
  await page.getByRole("button", { name: "Open another project", exact: true }).click();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
  const endpoint = JSON.parse(
    execFileSync(binary, ["--workspace", directory, "--trust-project", "desktop"], {
      env,
      input: '{"action":"connect"}',
      encoding: "utf8",
      timeout: 30000,
    }),
  );
  client = new Haarness({ baseUrl: endpoint.url, token: endpoint.token });
  thread = await client.createThread({ title: "Mac chat", provider: "echo", model: "echo-1" });
  await page.getByRole("button", { name: thread.title, exact: true }).click();
});
test.afterAll(async () => {
  await app?.close().catch(() => {});
  client?.close();
  try {
    execFileSync(binary, ["--workspace", directory, "--trust-project", "serve", "--stop"], { env, stdio: "ignore", timeout: 30000 });
  } catch {}
  stopChatHome(binary, env);
  if (!evidence && temporary) await rm(temporary, { recursive: true, force: true });
});

/** Answers `computer-use` requests from a status the test sets, applying each action as the server would. */
async function installStub() {
  await app.evaluate(({ ipcMain }) => {
    const handlers = (ipcMain as unknown as { _invokeHandlers: Map<string, (...args: any[]) => any> })._invokeHandlers;
    const fake = () => (globalThis as any).computerFake as Fake | undefined;
    const advertise = (value: any) =>
      fake()?.active && value?.capabilities && !value.capabilities.includes("computer-use.v1")
        ? { ...value, capabilities: [...value.capabilities, "computer-use.v1"] }
        : value;
    const connect = handlers.get("haarness:connect")!;
    handlers.set("haarness:connect", async (event, ...args) => {
      const result = await connect(event, ...args);
      if (result.ok && result.value?.info) result.value = { ...result.value, info: advertise(result.value.info) };
      return result;
    });
    const api = handlers.get("haarness:api")!;
    handlers.set("haarness:api", async (event, raw) => {
      const current = fake();
      const { method, args } = raw as { method: string; args: any[] };
      if (current?.active && method === "manageResource" && args[0] === "computer-use") {
        const input = args[1] ?? {};
        current.calls.push(input);
        if (input.action === "words") return { ok: true, value: { title: "Computer use", data: { supported: current.status.supported } } };
        const next = structuredClone(current.status);
        if (input.action === "accept") {
          next.terms.accepted = true;
          next.terms.changed = false;
          next.terms.acceptedAt = "2026-11-07T09:30:00Z";
          next.enabled = next.switch = true;
        } else if (input.action === "enable") next.enabled = next.switch = !!input.enabled;
        else if (input.action === "withdraw") {
          next.terms.accepted = false;
          next.terms.acceptedAt = null;
          next.enabled = next.switch = false;
        } else if (input.action === "configure") {
          if (input.defaultAct) next.settings.defaultAct = input.defaultAct;
          if (typeof input.ring === "boolean") next.settings.ring = input.ring;
        } else if (input.action === "app") {
          next.settings.apps = next.settings.apps.filter((app: { bundle: string }) => app.bundle !== input.bundle);
          if (input.rule !== "clear")
            next.settings.apps = [...next.settings.apps, { bundle: input.bundle, name: input.bundle === "com.apple.TextEdit" ? "TextEdit" : null, rule: input.rule }].sort((a, b) => a.bundle.localeCompare(b.bundle));
        }
        current.status = next;
        return { ok: true, value: { title: "Computer use", data: next } };
      }
      if (current?.active && method === "replyPermission" && current.permissions.some((item) => item.permissionId === args[0])) {
        current.replies.push({ id: args[0], decision: args[1].decision });
        current.permissions = current.permissions.filter((item) => item.permissionId !== args[0]);
        return { ok: true, value: null };
      }
      const result = await api(event, raw);
      if (!result.ok || !current?.active) return result;
      if (method === "info") result.value = advertise(result.value);
      if (method === "listPermissions") result.value = [...result.value, ...current.permissions];
      return result;
    });
  });
}
async function stub(change: Partial<Fake>) {
  await app.evaluate((_, change) => {
    (globalThis as any).computerFake = { ...((globalThis as any).computerFake ?? {}), ...change };
  }, change as Record<string, unknown>);
}
async function fake(): Promise<Fake> {
  return app.evaluate(() => (globalThis as any).computerFake);
}
const actions = async () => (await fake()).calls.map((call) => call.action);
const settings = () => page.getByRole("dialog", { name: "Settings", exact: true });
/** The dialog whatever language names it, for screenshots across languages. */
const dialog = () => page.locator("dialog.settings-dialog");
/** The dock whatever language names it. */
const docked = () => page.locator(".prompt-dock.approval");
async function openComputerUse() {
  if (!(await settings().count())) await page.getByRole("button", { name: "Settings", exact: true }).click();
  await settings().getByRole("textbox", { name: "Filter settings" }).fill("");
  await settings().getByRole("button", { name: "Computer use", exact: true }).click();
  await expect(settings().getByRole("heading", { name: "Computer use", exact: true })).toBeVisible();
}
async function closeSettings() {
  if (await settings().count()) await settings().getByRole("button", { name: "Close settings", exact: true }).click();
}
async function snap(name: string, target: { screenshot(options: { path: string }): Promise<unknown> } = page) {
  await target.screenshot({ path: evidence ? join(evidence, name) : test.info().outputPath(name) });
}
async function look(name: string, target: () => { screenshot(options: { path: string }): Promise<unknown> }) {
  for (const theme of ["light", "dark"] as const) {
    await page.evaluate((theme) => window.haarness.preferences({ theme }), theme);
    await page.waitForTimeout(150);
    await snap(`${name}-${theme}.png`, target());
  }
  await page.evaluate(() => window.haarness.preferences({ language: "ar" }));
  await expect(page.locator("html")).toHaveAttribute("dir", "rtl");
  await page.waitForTimeout(150);
  await snap(`${name}-ar.png`, target());
  await page.evaluate(() => window.haarness.preferences({ language: "en", theme: "light" }));
  await expect(page.locator("html")).toHaveAttribute("dir", "ltr");
}
/** The renderer lists requests when it loads (and on server events), so a reload shows a stubbed one. */
async function showPermissions(permissions: PendingPermissionInfo[]) {
  await stub({ replies: [], permissions });
  await page.reload();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
}
const noHorizontalOverflow = async () =>
  expect(await settings().locator(".settings-content").evaluate((element) => element.scrollWidth <= element.clientWidth + 1)).toBe(true);

test("the bar's words reach the server in the interface language as soon as Desktop connects", async () => {
  await expect.poll(async () => (await fake()).calls.find((call) => call.action === "words")?.bar?.usingApp).toBe("Haarness is working in %@");
  const words = (await fake()).calls.find((call) => call.action === "words")!.bar;
  expect(Object.keys(words).sort()).toEqual(["pause", "paused", "resume", "stop", "stopHint", "usingApp", "usingMac"]);
  expect(words.stop).toBe("Stop");
});

test("a server without computer use says so in one quiet line", async () => {
  await stub({ status: status({ supported: false }) });
  await openComputerUse();
  await expect(settings().getByTestId("computer-use-unavailable")).toHaveText("Computer use needs macOS and a build of Haarness that includes it.");
  await expect(settings().getByRole("switch")).toHaveCount(0);
  await closeSettings();
});

test("agreeing takes the checkbox, then turns computer use on", async () => {
  await stub({ status: status(), calls: [] });
  await openComputerUse();
  const pane = settings().locator(".computer-use-settings");
  await expect(pane.getByText("Terms for computer use", { exact: true })).toBeVisible();
  await expect(pane.getByTestId("computer-terms")).toContainText("App contents can't be trusted");
  await expect(pane.getByTestId("computer-terms").getByText("Use it for", { exact: true })).toBeVisible();
  // Agreeing for the first time: nothing changed since.
  await expect(pane.getByTestId("computer-terms-changed")).toHaveCount(0);
  const agree = pane.getByRole("button", { name: "Agree and turn on", exact: true });
  await expect(agree).toBeDisabled();
  await look("computer-use-terms", dialog);
  await openComputerUse();
  await pane.getByRole("checkbox", { name: "I have read and agree to these terms", exact: true }).check();
  await expect(agree).toBeEnabled();
  await agree.click();
  await expect.poll(async () => (await fake()).calls.find((call) => call.action === "accept")).toMatchObject({ action: "accept", version: "2" });
  await expect(pane.getByRole("switch", { name: "Agents may use this Mac", exact: true })).toBeChecked();
  // Every request carries the bar's words in the language shown at the time (the screenshots above switch to
  // Arabic and back), so the server always has them.
  const calls = (await fake()).calls;
  for (const call of calls) expect(call.bar?.usingMac).toBeTruthy();
  expect(calls.at(-1)?.bar?.usingMac).toBe("Haarness is working on this Mac");
  await expect(pane.getByText(/You agreed on November 7, 2026\./)).toBeVisible();
  await expect(
    pane.getByText("While an agent works, a strip along the top edge of the screen shows the app in use, with Pause and Stop. Esc also stops it while it works in the app in front.", { exact: true }),
  ).toBeVisible();
  await noHorizontalOverflow();
  expect(errors).toEqual([]);
  await look("computer-use-switch", dialog);
  await openComputerUse();
});

test("after the terms changed, one quiet line says so where the person agrees, and agreeing is the same single step", async () => {
  const before = (await fake()).status;
  await stub({
    status: { ...before, enabled: false, terms: { version: "2", accepted: false, acceptedAt: null, changed: true, text: terms } },
    calls: [],
  });
  await openComputerUse();
  const pane = settings().locator(".computer-use-settings");
  const note = pane.getByTestId("computer-terms-changed");
  await expect(note).toHaveText("You agreed to an earlier version of these terms.", { timeout: 8000 });
  // The line sits after the terms, just above the box that agrees to them.
  const [body, line, box] = await Promise.all(
    [pane.getByTestId("computer-terms"), note, pane.locator(".computer-agree")].map((item) => item.boundingBox()),
  );
  expect(body!.y + body!.height).toBeLessThanOrEqual(line!.y);
  expect(line!.y + line!.height).toBeLessThanOrEqual(box!.y);
  await look("computer-use-terms-changed", dialog);
  await openComputerUse();
  await pane.getByRole("checkbox", { name: "I have read and agree to these terms", exact: true }).check();
  await pane.getByRole("button", { name: "Agree and turn on", exact: true }).click();
  await expect.poll(async () => (await fake()).calls.find((call) => call.action === "accept")).toMatchObject({ action: "accept", version: "2" });
  await expect(pane.getByRole("switch", { name: "Agents may use this Mac", exact: true })).toBeChecked();
  await expect(note).toHaveCount(0);
  await noHorizontalOverflow();
  expect(errors).toEqual([]);
});

test("macOS permissions name the app macOS asks about; the first click prompts, later ones open System Settings", async () => {
  await stub({ calls: [] });
  const pane = settings().locator(".computer-use-settings");
  await expect(pane.getByText("macOS asks for these for Terminal, the app Haarness runs in. Everything started from it shares them.", { exact: true })).toBeVisible();
  const accessibility = settings().locator('[data-setting="Accessibility"]');
  const screen = settings().locator('[data-setting="Screen Recording"]');
  await expect(accessibility).toContainText("Not allowed");
  await accessibility.getByRole("button", { name: "Allow Accessibility in System Settings", exact: true }).click();
  await expect.poll(actions).toContain("permissions");
  await screen.getByRole("button", { name: "Allow Screen Recording in System Settings", exact: true }).click();
  await expect.poll(async () => (await fake()).calls.find((call) => call.action === "open")).toMatchObject({ action: "open", pane: "screen-recording" });
  // Allowed in System Settings: the next look shows it, without a flicker of the rest.
  await stub({ status: { ...(await fake()).status, permissions: { accessibility: true, screenRecording: true } } });
  await expect(accessibility).toContainText("Allowed", { timeout: 8000 });
  await expect(screen).toContainText("Allowed");
  await expect(accessibility.getByRole("button")).toHaveCount(0);
  // Desktop's own server: macOS asks about Haarness itself.
  await stub({ status: { ...(await fake()).status, host: { bundle: "dev.haarness.desktop", name: "Haarness", haarness: true } } });
  await expect(pane.getByText("macOS asks for these for Haarness.", { exact: true })).toBeVisible({ timeout: 8000 });
  await stub({ status: { ...(await fake()).status, console: false } });
  await expect(pane.getByText("This Mac's screen belongs to another signed-in user, so agents can't use it from this account.", { exact: true })).toBeVisible({ timeout: 8000 });
  await stub({ status: { ...(await fake()).status, console: true } });
});

test("apps without a rule and the click ring change with one control each", async () => {
  await stub({ calls: [] });
  await settings().getByRole("combobox", { name: "Apps without a rule", exact: true }).click();
  await page.getByRole("option", { name: "Act without asking", exact: true }).click();
  await expect.poll(async () => (await fake()).calls.at(-1)).toMatchObject({ action: "configure", defaultAct: "allow" });
  await settings().getByRole("switch", { name: "Mark each click on screen", exact: true }).uncheck();
  await expect.poll(async () => (await fake()).calls.at(-1)).toMatchObject({ action: "configure", ring: false });
  await expect(settings().getByRole("switch", { name: "Mark each click on screen", exact: true })).not.toBeChecked();
});

test("app rules are added, changed and removed, and a bad identifier is refused before anything is sent", async () => {
  await stub({ calls: [] });
  const rules = settings().locator('[data-setting="App rules"]');
  const input = rules.getByRole("textbox", { name: "App bundle identifier", exact: true });
  await input.fill("TextEdit app");
  await rules.getByRole("button", { name: "Add app", exact: true }).click();
  await expect(rules.getByRole("alert")).toHaveText("Use a bundle identifier like com.apple.TextEdit.");
  expect(await actions()).not.toContain("app");
  await input.fill("com.apple.TextEdit");
  await expect(rules.getByRole("alert")).toHaveCount(0);
  await rules.getByRole("combobox", { name: "Rule for the new app", exact: true }).click();
  await page.getByRole("option", { name: "Ask every time", exact: true }).click();
  await rules.getByRole("button", { name: "Add app", exact: true }).click();
  await expect.poll(async () => (await fake()).calls.at(-1)).toMatchObject({ action: "app", bundle: "com.apple.TextEdit", rule: "ask" });
  await expect(rules.locator(".computer-app-list li")).toHaveCount(1);
  await expect(rules.locator(".computer-app-name")).toContainText("TextEdit");
  await expect(rules.locator(".computer-app-name small")).toHaveText("com.apple.TextEdit");
  await expect(input).toHaveValue("");
  await rules.getByRole("combobox", { name: "Rule for TextEdit", exact: true }).click();
  await page.getByRole("option", { name: "Never", exact: true }).click();
  await expect.poll(async () => (await fake()).calls.at(-1)).toMatchObject({ action: "app", bundle: "com.apple.TextEdit", rule: "deny" });
  await input.fill("com.jetbrains.*");
  await rules.getByRole("button", { name: "Add app", exact: true }).click();
  await expect(rules.locator(".computer-app-list li")).toHaveCount(2);
  await expect(rules.locator(".computer-apps-head p")).toHaveText(
    "Password managers and Haarness itself are never used, and agents never type into terminals. In Auto, apps set to ask every time still ask you.",
  );
  await look("computer-use-settings", dialog);
  await openComputerUse();
  await rules.getByRole("button", { name: "Remove the rule for TextEdit", exact: true }).click();
  await expect.poll(async () => (await fake()).calls.at(-1)).toMatchObject({ action: "app", bundle: "com.apple.TextEdit", rule: "clear" });
  await expect(rules.locator(".computer-app-list li")).toHaveCount(1);
  await noHorizontalOverflow();
});

test("the terms stay a click away, and withdrawing turns computer use off", async () => {
  await stub({ calls: [] });
  const pane = settings().locator(".computer-use-settings");
  await expect(pane.getByTestId("computer-terms")).toHaveCount(0);
  await pane.getByRole("button", { name: "View terms", exact: true }).click();
  await expect(pane.getByTestId("computer-terms")).toContainText("Turning on computer use");
  await pane.getByRole("button", { name: "Hide terms", exact: true }).click();
  await pane.getByRole("button", { name: "Withdraw agreement", exact: true }).click();
  await expect.poll(actions).toContain("withdraw");
  await expect(pane.getByRole("button", { name: "Agree and turn on", exact: true })).toBeDisabled();
  await closeSettings();
  expect(errors).toEqual([]);
});

const request = (id: string, mac: Record<string, unknown>, allowAlways = true): PendingPermissionInfo =>
  ({
    permissionId: id,
    permission: "computer",
    operation: `computer:act:${thread.id}:com.apple.TextEdit`,
    patterns: [`act:${thread.id}:com.apple.TextEdit`],
    alwaysPatterns: [`computer:act:${thread.id}:com.apple.TextEdit`],
    allowAlways,
    explicit: true,
    detail: { ref: "e7", __mac: { asks: ["internal"], ...mac } },
    threadId: thread.id,
    rootThreadId: thread.id,
    turnId: null,
    createdAt: new Date().toISOString(),
  }) as PendingPermissionInfo;

/** A picture like the helper's: a 360×216 crop of a window with a ring on the spot. */
async function spotPicture() {
  return page.evaluate(() => {
    const canvas = document.createElement("canvas");
    canvas.width = 360;
    canvas.height = 216;
    const draw = canvas.getContext("2d")!;
    draw.fillStyle = "#f4f2ee";
    draw.fillRect(0, 0, 360, 216);
    draw.fillStyle = "#ffffff";
    draw.fillRect(20, 24, 320, 168);
    draw.fillStyle = "#d9d6d0";
    draw.fillRect(20, 24, 320, 26);
    draw.fillStyle = "#3478f6";
    draw.fillRect(236, 150, 84, 28);
    draw.fillStyle = "#ffffff";
    draw.font = "13px -apple-system, sans-serif";
    draw.fillText("Save", 263, 169);
    draw.fillStyle = "#555";
    draw.fillText("Untitled — Edited", 130, 42);
    draw.strokeStyle = "rgb(219, 163, 115)";
    draw.lineWidth = 3;
    draw.beginPath();
    draw.arc(278, 164, 13, 0, Math.PI * 2);
    draw.stroke();
    return canvas.toDataURL("image/jpeg", 0.7);
  });
}

test("a computer use request docks as Use this Mac, with the spot, and allows that app for this chat", async () => {
  const id = crypto.randomUUID();
  const picture = await spotPicture();
  await showPermissions([
    request(id, {
      kind: "act",
      action: "click",
      bundle: "com.apple.TextEdit",
      app: "TextEdit",
      target: 'button "Save"',
      label: "Save",
      summary: 'Click button "Save" in TextEdit',
      preview: picture,
    }),
  ]);
  const dock = page.getByRole("alert", { name: "Permission request" });
  await expect(dock.locator(".prompt-title")).toHaveText("Use this Mac");
  await expect(dock.locator(".computer-approval-summary")).toHaveText("Click “Save” in TextEdit");
  await expect(dock.locator(".computer-approval-preview img")).toHaveAttribute("src", picture);
  // Shown smaller than it is, the picture opens full size.
  await expect(dock.locator(".computer-approval-preview img")).toHaveJSProperty("naturalWidth", 360);
  await expect(dock.locator(".computer-approval-preview [data-image-open]")).toHaveCount(1);
  await expect(dock).not.toContainText("asks");
  await expect(dock).not.toContainText("Chrome");
  await expect(dock.locator(".prompt-actions").getByRole("button")).toHaveText(["Deny", "Allow once", "Always allow TextEdit", "Allow in this chat"]);
  await expect(dock.getByRole("button", { name: "Allow in this chat", exact: true })).toHaveClass(/primary/);
  await look("computer-use-approval", docked);
  await dock.getByRole("button", { name: "Allow in this chat", exact: true }).click();
  await expect.poll(async () => (await fake()).replies.at(-1)).toEqual({ id, decision: "always" });
  await expect(dock).toHaveCount(0);
});

test("Always allow saves a rule for the app, then allows this request once", async () => {
  const id = crypto.randomUUID();
  await showPermissions([
    request(id, { kind: "act", action: "type", bundle: "com.apple.Notes", app: "Notes", label: "Body", text: "milk", summary: 'Type into text area "Body" in Notes' }),
  ]);
  const dock = page.getByRole("alert", { name: "Permission request" });
  await expect(dock.locator(".computer-approval-summary")).toHaveText("Type into “Body” in Notes", { timeout: 10000 });
  const button = dock.getByRole("button", { name: "Always allow Notes", exact: true });
  await expect(button).toHaveAttribute("title", "Agents won't ask before using Notes again, in any chat. You can change this in Settings → Computer use.");
  await button.click();
  await expect.poll(async () => (await fake()).replies.at(-1)).toEqual({ id, decision: "once" });
  const rule = (await fake()).calls.find((call) => call.action === "app");
  expect(rule).toMatchObject({ action: "app", bundle: "com.apple.Notes", rule: "allow" });
  expect(rule?.bar?.usingApp).toBeTruthy();
});

test("an app set to ask every time asks each time and offers no chat-wide allow", async () => {
  const id = crypto.randomUUID();
  await showPermissions([
    request(
      id,
      {
        kind: "act",
        action: "click",
        bundle: "com.apple.mail",
        app: "Mail",
        target: 'button "Send"',
        label: "Send",
        summary: 'Click button "Send" in Mail',
        everyTime: true,
      },
      false,
    ),
  ]);
  const dock = page.getByRole("alert", { name: "Permission request" });
  await expect(dock.locator(".prompt-title")).toHaveText("Use this Mac", { timeout: 10000 });
  await expect(dock.locator(".prompt-actions").getByRole("button")).toHaveText(["Deny", "Allow once"]);
  await expect(dock.getByRole("button", { name: "Allow once", exact: true })).toHaveClass(/primary/);
  await expect(dock.getByTestId("computer-always")).toHaveCount(0);
  await expect(dock.locator(".computer-approval-note")).toHaveText("You set this app to ask every time.");
  await snap("computer-use-approval-every-time.png", dock);
  await dock.getByRole("button", { name: "Allow once", exact: true }).click();
  await expect.poll(async () => (await fake()).replies.at(-1)).toEqual({ id, decision: "once" });
});

test("a computer use card Auto's reviewer left to the person carries its note", async () => {
  const id = crypto.randomUUID();
  const asked = {
    ...request(id, {
      kind: "act",
      action: "click",
      bundle: "com.apple.Notes",
      app: "Notes",
      target: 'button "Delete"',
      label: "Delete",
      summary: 'Click button "Delete" in Notes',
    }),
    review: { status: "asked", risk: "high", rule: "Destructive action", rationale: "It deletes a note the person never named." },
  } as PendingPermissionInfo;
  await showPermissions([asked]);
  const dock = docked();
  await expect(dock.locator(".computer-approval-summary")).toHaveText("Click “Delete” in Notes", { timeout: 10000 });
  await expect(dock.locator(".approval-review")).toContainText("Destructive action");
  await expect(dock.locator(".approval-review")).toContainText("It deletes a note the person never named.");
  await look("computer-use-approval-reviewed", docked);
  await docked().getByRole("button", { name: "Deny", exact: true }).click();
  await expect.poll(async () => (await fake()).replies.at(-1)).toEqual({ id, decision: "reject" });
});

test("typing shows its text, and a whole-screen look says what goes to the model", async () => {
  const typing = crypto.randomUUID();
  const screen = crypto.randomUUID();
  await showPermissions([
    request(typing, { kind: "act", action: "type", app: "Notes", target: "text area", summary: "Type into text area in Notes", text: "Groceries\nmilk" }),
  ]);
  const dock = page.getByRole("alert", { name: "Permission request" });
  await expect(dock.locator(".computer-approval-text")).toHaveText("Groceries ⏎ milk", { timeout: 10000 });
  await dock.getByRole("button", { name: "Deny", exact: true }).click();
  await expect.poll(async () => (await fake()).replies.at(-1)).toEqual({ id: typing, decision: "reject" });
  await showPermissions([request(screen, { kind: "screen", action: "screen", summary: "Look at every app's windows" })]);
  await expect(dock.locator(".computer-approval-summary")).toHaveText("Look at every app's windows", { timeout: 10000 });
  await expect(dock.locator(".computer-approval-note")).toHaveText(
    "Everything open on screen, apart from password managers and Haarness, goes to the model.",
  );
  await expect(dock.locator(".prompt-actions").getByRole("button")).toHaveText(["Deny", "Allow once", "Allow in this chat"]);
  await dock.getByRole("button", { name: "Allow once", exact: true }).click();
  await expect.poll(async () => (await fake()).replies.at(-1)).toEqual({ id: screen, decision: "once" });
  expect(errors).toEqual([]);
});

/** Turns computer use on in the server's own settings, as agreeing in Settings would, with apps acting unasked. */
async function computerUseOn(on: boolean) {
  // With a data folder set, the server's global configuration lives in it.
  const folder = join(env.HAARNESS_DATA_DIR, "config");
  await mkdir(folder, { recursive: true });
  await writeFile(
    join(folder, "computer-use.json"),
    JSON.stringify(on ? { enabled: true, terms: { version: "2", acceptedAt: "2026-11-07T00:00:00Z" }, defaultAct: "allow" } : { enabled: false }),
  );
}

test("a pause on the strip shows as a still line in the chat, and its Resume lets the agent go on", async () => {
  test.skip(!!process.env.HAARNESS_COMPUTER_LIVE, "Pauses through the stand-in helper");
  test.setTimeout(150000);
  await stub({ active: false, permissions: [] });
  await computerUseOn(true);
  await page.reload();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
  // The stand-in presses Pause on the strip as the agent starts typing, and stays paused until it hears Resume.
  await page.locator("[data-composer-input]").fill(' !tool mac_type {"ref":"e2","text":"HOLD","snapshot":false}');
  await page.locator(".send-button").click();
  const line = page.locator(".assistant-activity");
  await expect(line).toContainText("Paused", { timeout: 60000 });
  const resume = line.getByRole("button", { name: "Resume", exact: true });
  await expect(resume).toBeVisible();
  // Nothing moves, nothing is asked, and the control is no taller than the line it sits on.
  await expect(line.locator(".activity-shimmer")).toHaveCount(0);
  await expect(page.getByRole("alert", { name: "Permission request" })).toHaveCount(0);
  const [lineBox, buttonBox] = [await line.boundingBox(), await resume.boundingBox()];
  expect(buttonBox!.height).toBeLessThanOrEqual(lineBox!.height);
  const step = page.locator("details.tool-call").last();
  await look("computer-use-paused", () => page.locator(".conversation"));
  // A window that opens during the pause shows it from the start.
  await page.reload();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
  await expect(line).toContainText("Paused", { timeout: 20000 });
  await line.getByRole("button", { name: "Resume", exact: true }).click();
  // The agent goes on: the strip heard Resume, the step ends, and the turn with it.
  await expect(line).toHaveCount(0, { timeout: 30000 });
  await expect(step).toHaveAttribute("data-status", /done|failed/);
  const helper = (await readFile(join(temporary, "helper.log"), "utf8")).split("\n").filter(Boolean).map((row) => JSON.parse(row).op);
  expect(helper.slice(helper.indexOf("type"))).toContain("resume");
  await expect(page.locator(".message.assistant").last()).toContainText("running again");
  await computerUseOn(false);
  expect(errors).toEqual([]);
});

// Takes a real screenshot of Finder through the server's helper, so it needs
// a Mac whose screen this run may look at: HAARNESS_COMPUTER_LIVE=1.
test("[live] a snapshot's row names the app and shows the screenshot it took", async () => {
  test.skip(!process.env.HAARNESS_COMPUTER_LIVE, "Set HAARNESS_COMPUTER_LIVE=1 to look at this Mac's screen");
  test.setTimeout(120000);
  await stub({ active: false, permissions: [] });
  // With a data folder set, the server's global configuration lives in it.
  const settingsFile = join(env.HAARNESS_DATA_DIR, "config", "computer-use.json");
  await mkdir(join(env.HAARNESS_DATA_DIR, "config"), { recursive: true });
  await writeFile(
    settingsFile,
    JSON.stringify({ enabled: true, terms: { version: "2", acceptedAt: "2026-11-07T00:00:00Z" } }),
  );
  await page.reload();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
  await page.locator("[data-composer-input]").fill(' !tool mac_snapshot {"app":"com.apple.finder","depth":4}');
  await page.locator(".send-button").click();
  const row = page.locator("details.tool-call").filter({ hasText: "Viewed" }).last();
  await expect(row.locator(".tool-target")).toHaveText("Finder", { timeout: 60000 });
  await expect(row).toHaveAttribute("data-status", "done");
  if (!(await row.getAttribute("open"))) await row.locator("summary").click();
  const image = row.locator(".tool-image img");
  await expect(image).toBeVisible({ timeout: 20000 });
  expect(await image.evaluate((element) => (element as HTMLImageElement).naturalWidth)).toBeGreaterThan(400);
  await snap("computer-use-transcript-live.png", row);
  await writeFile(settingsFile, JSON.stringify({ enabled: false }));
  expect(errors).toEqual([]);
});
