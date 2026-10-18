import { Haarness } from "@haarness/sdk";
import { test, expect, _electron as electron, type ElectronApplication, type Page, type Locator } from "@playwright/test";
import { mkdir, mkdtemp, realpath, writeFile, rm, readFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";

// Real Electron, IPC, disk preferences and project daemons. Only the native
// folder picker is supplied a deterministic answer. Drag tests use the mouse.
const binary = resolve(process.env.HAARNESS_BIN ?? "../../target/debug/haarness");
let app: ElectronApplication;
let page: Page;
let temporary: string;
let env: Record<string, string>;
let errors: string[];
let directories: string[];
/** Worktrees a test made; Desktop may have started their servers. */
let checkouts: string[];
const clients: Haarness[] = [];
const project = (name: string) => page.getByRole("region", { name: `Project ${name}`, exact: true });
const section = (name: string) => page.getByRole("region", { name: `Section ${name}`, exact: true });
const heading = (name: string) => project(name).locator(".project-heading");
const titles = (scope: Locator = page.locator(".project-list")) => scope.locator(".project-heading");
const item = (name: string) => page.getByRole("menuitem", { name, exact: true });
async function options(name: string) {
  await project(name).getByRole("button", { name: `Project options for ${name}`, exact: true }).click();
}
async function launch() {
  app = await electron.launch({ args: ["."], env });
  page = await app.firstWindow();
  await app.evaluate(({ BrowserWindow }) => {
    BrowserWindow.getAllWindows()[0].webContents.setBackgroundThrottling(false);
  });
  page.on("pageerror", error => errors.push(error.message));
  await expect(page.locator(".application")).toBeVisible();
}
async function close() {
  if (!app) return;
  const current = app;
  const child = current.process();
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    await Promise.race([
      (async () => {
        await current.evaluate(({ BrowserWindow }) => { for (const window of BrowserWindow.getAllWindows()) window.destroy(); }).catch(() => {});
        await current.close().catch(() => {});
      })(),
      new Promise<void>(resolve => { timer = setTimeout(resolve, 10000); }),
    ]);
  } finally {
    clearTimeout(timer);
    // Even a wedged native OS drag must not strand this fixture or prevent
    // its owned project daemons from being stopped by afterEach.
    if (child.exitCode === null) child.kill("SIGTERM");
  }
}
async function open(name: string, config = "") {
  const directory = join(temporary, "projects", name);
  if (!directories.includes(directory)) directories.push(directory);
  await mkdir(join(directory, ".haarness"), { recursive: true });
  await writeFile(join(directory, ".haarness", "config.toml"), '[defaults]\nprovider="echo"\nmodel="echo-1"\n' + config);
  await app.evaluate(({ dialog }, directory) => {
    const original = dialog.showOpenDialog;
    dialog.showOpenDialog = async () => {
      dialog.showOpenDialog = original;
      return { canceled: false, filePaths: [directory] };
    };
  }, directory);
  await page.getByRole("button", { name: "Open another project", exact: true }).click();
  await expect(project(name)).toHaveClass(/current/);
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
  const connection = JSON.parse(execFileSync(binary, ["--workspace", directory, "--trust-project", "desktop"], {
    env, input: JSON.stringify({ action: "connect" }), encoding: "utf8", timeout: 30000,
  }));
  const client = new Haarness({ baseUrl: connection.url, token: connection.token });
  clients.push(client);
  return client;
}
async function collapse(name: string) {
  const button = project(name).getByRole("button", { name: `Collapse project ${name}`, exact: true });
  if (await button.count()) await button.click();
}
async function createSection(name: string, group: string) {
  await options(name);
  await item("Section").click();
  await item("New section…").click();
  const dialog = page.getByRole("dialog", { name: "New section", exact: true });
  await dialog.getByLabel("Name", { exact: true }).fill(group);
  await dialog.getByRole("button", { name: "Create", exact: true }).click();
  await expect(titles(section(group))).toContainText([name]);
}
async function fileUnder(name: string, group: string) {
  await options(name);
  await item("Section").click();
  await page.getByRole("menuitemradio", { name: group, exact: true }).click();
}
async function pin(name: string, pinned = true) {
  await options(name);
  await item(pinned ? "Pin" : "Unpin").click();
}
async function layout(name: "By project" | "In one list") {
  await page.getByRole("button", { name: "Project list options", exact: true }).click();
  await item("Organize sidebar").click();
  await page.getByRole("menuitemradio", { name, exact: true }).click();
}
async function reload() {
  await page.reload();
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
}

async function dragToPoint(source: Locator, point: { x: number; y: number }, inspect?: () => Promise<void>) {
  const box = await source.boundingBox();
  expect(box).not.toBeNull();
  await page.mouse.move(box!.x + Math.min(35, box!.width / 2), box!.y + box!.height / 2);
  await page.mouse.down();
  await page.mouse.move(point.x, point.y, { steps: 12 });
  await page.mouse.move(point.x, point.y);
  await inspect?.();
  await page.mouse.up();
}

async function electronDrag(source: Locator, destination: Locator | { x: number; y: number }, options: { cancel?: boolean; inspect?: () => Promise<void> } = {}) {
  const start = await source.boundingBox();
  const from = { x: Math.round(start!.x + Math.min(8, start!.width / 2)), y: Math.round(start!.y + start!.height / 2) };
  const end = "boundingBox" in destination ? await destination.boundingBox() : undefined;
  const to = end ? { x: Math.round(end.x + Math.min(8, end.width / 2)), y: Math.round(end.y + end.height / 2) }
    : destination as { x: number; y: number };
  // Keep the fixture hidden so physical mouse input in the user's other
  // windows cannot mix with the replayed gesture. Events still enter Electron's
  // normal WebContents input path, without Playwright's drag interception.
  await app.evaluate(({ BrowserWindow }) => {
    const web = BrowserWindow.getAllWindows()[0].webContents;
    web.setBackgroundThrottling(false);
    web.focus();
  });
  await page.evaluate(() => {
    const host = window as typeof window & { orderInput?: unknown[]; stopOrderInput?: () => void };
    host.stopOrderInput?.();
    host.orderInput = [];
    const record = (event: Event) => {
      const mouse = event as MouseEvent;
      host.orderInput!.push({ type: event.type, x: mouse.clientX, y: mouse.clientY, buttons: mouse.buttons });
    };
    const types = ["pointerdown", "pointermove", "pointerup", "pointercancel", "dragstart", "dragover", "drop", "dragend"];
    types.forEach(type => window.addEventListener(type, record, true));
    host.stopOrderInput = () => types.forEach(type => window.removeEventListener(type, record, true));
  });
  await app.evaluate(async ({ BrowserWindow }, { from, to }) => {
    const web = BrowserWindow.getAllWindows()[0].webContents;
    web.sendInputEvent({ type: "mouseMove", ...from });
    web.sendInputEvent({ type: "mouseDown", ...from, button: "left", clickCount: 1 });
    for (let step = 1; step <= 16; step++) {
      web.sendInputEvent({ type: "mouseMove", x: Math.round(from.x + (to.x - from.x) * step / 16), y: Math.round(from.y + (to.y - from.y) * step / 16), modifiers: ["leftbuttondown"] });
      await new Promise(resolve => setTimeout(resolve, 12));
    }
  }, { from, to });
  await options.inspect?.();
  if (options.cancel) await page.keyboard.press("Escape");
  await app.evaluate(({ BrowserWindow }, to) => {
    BrowserWindow.getAllWindows()[0].webContents.sendInputEvent({ type: "mouseUp", ...to, button: "left", clickCount: 1 });
  }, to);
  const events = await page.evaluate(() => (window as typeof window & { orderInput?: unknown[] }).orderInput);
  await test.info().attach("electron-mouse-events", { body: JSON.stringify(events), contentType: "application/json" });
}

test.beforeEach(async () => {
  temporary = await realpath(await mkdtemp(join(tmpdir(), "haarness-sidebar-e2e-")));
  errors = [];
  directories = [];
  checkouts = [];
  env = Object.fromEntries(Object.entries(process.env).filter((entry): entry is [string, string] => entry[1] !== undefined));
  Object.assign(env, {
    HAARNESS_BIN: binary,
    HAARNESS_HOME: join(temporary, "haarness"),
    HAARNESS_DATA_DIR: join(temporary, "state"),
    HAARNESS_DESKTOP_DATA: join(temporary, "desktop"),
    HAARNESS_TEST_PROVIDER: "1",
    HAARNESS_DESKTOP_TEST: "1",
  });
  delete env.ELECTRON_RUN_AS_NODE;
  await launch();
});
test.afterEach(async () => {
  await page?.screenshot({ path: test.info().outputPath("sidebar.png") }).catch(() => {});
  if (page && test.info().status !== test.info().expectedStatus) {
    await test.info().attach("sidebar-ui", { body: await page.locator("body").innerText().catch(() => "closed"), contentType: "text/plain" });
  }
  for (const client of clients.splice(0)) client.close();
  await close();
  for (const directory of directories) {
    execFileSync(binary, ["--workspace", directory, "serve", "--stop"], { env, stdio: "ignore", timeout: 30000 });
  }
  // A removed worktree's folder is gone, which this tolerates.
  for (const directory of checkouts) {
    try {
      execFileSync(binary, ["--workspace", directory, "serve", "--stop"], { env, stdio: "ignore", timeout: 30000 });
    } catch {}
  }
  await rm(temporary, { recursive: true, force: true });
  expect(errors).toEqual([]);
});

for (const input of ["playwright", "electron"] as const) {
  for (const handle of ["title", "disclosure", "icon"] as const) {
    test(`sidebar screenshot layout drops on section title text from ${handle} using ${input} input`, async () => {
      for (const name of ["dashboard-front", "Desktop", "Downloads", "stacks", "xchat-front"]) { await open(name); await collapse(name); }
      await createSection("dashboard-front", "555");
      await fileUnder("Desktop", "555");
      const source = () => handle === "title" ? heading("Downloads").locator("span")
        : project("Downloads").locator(handle === "icon" ? ".project-disclosure svg" : ".project-disclosure");
      const destination = section("555").locator(".project-section-row > span");
      if (input === "playwright") await source().dragTo(destination, { targetPosition: { x: 8, y: 12 } });
      else await electronDrag(source(), destination);
      await expect(titles(section("555"))).toHaveText(["dashboard-front", "Desktop", "Downloads"]);
      await expect(heading("Downloads")).toHaveAttribute("aria-expanded", "false");
      const unfiled = section("No section").locator(".project-section-row > span");
      if (input === "playwright") await source().dragTo(unfiled, { targetPosition: { x: 8, y: 12 } });
      else await electronDrag(source(), unfiled);
      await expect(titles(section("No section"))).toHaveText(["stacks", "xchat-front", "Downloads"]);
      await expect(heading("Downloads")).toHaveAttribute("aria-expanded", "false");
      await expect(page.locator("[data-drop-position]")).toHaveCount(0);
    });
  }
}

test("sidebar same-section heading and background drops leave project order unchanged", async () => {
  for (const name of ["xchat-front", "Downloads", "stacks", "dashboard-front", "Desktop"]) {
    await open(name);
    await collapse(name);
  }
  await createSection("dashboard-front", "555");
  await fileUnder("Desktop", "555");
  const expected = ["dashboard-front", "Desktop", "xchat-front", "Downloads", "stacks"];
  await expect(titles()).toHaveText(expected);

  for (const pinned of [false, true]) {
    if (pinned) {
      for (const name of ["dashboard-front", "Desktop", "xchat-front", "Downloads"]) await pin(name);
    }
    for (const [name, group] of [["xchat-front", "No section"], ["dashboard-front", "555"]]) {
      const target = section(group).locator(".project-section-row");
      const inspect = async () => {
        await expect(target).toHaveAttribute("data-drop-position", "inside");
        await expect(page.locator(".order-drag-preview")).toBeVisible();
      };
      await electronDrag(heading(name).locator("span"), target.locator(":scope > span"), { inspect });
      await expect(page.locator(".order-drag-preview, [data-drop-position]")).toHaveCount(0);
      await expect(titles()).toHaveText(expected);

      const bounds = await section(group).boundingBox();
      const point = group === "No section"
        ? { x: bounds!.x + 60, y: bounds!.y + bounds!.height + 35 }
        : { x: bounds!.x + 60, y: bounds!.y + 4 };
      await electronDrag(heading(name), point, { inspect });
      await expect(page.locator(".order-drag-preview, [data-drop-position]")).toHaveCount(0);
      await expect(titles()).toHaveText(expected);
    }
    await reload();
    await expect(titles()).toHaveText(expected);
  }

  // A deliberate drop beside a row still changes the order within its section.
  for (const [source, destination] of [["xchat-front", "Downloads"], ["dashboard-front", "Desktop"]]) {
    const bounds = await heading(destination).boundingBox();
    await electronDrag(heading(source), { x: bounds!.x + 60, y: bounds!.y + bounds!.height - 2 });
  }
  await expect(titles()).toHaveText(["Desktop", "dashboard-front", "Downloads", "xchat-front", "stacks"]);
  await reload();
  await expect(titles()).toHaveText(["Desktop", "dashboard-front", "Downloads", "xchat-front", "stacks"]);
});

test("sidebar direct Electron input reorders sessions from text, status and archive controls in both layouts", async () => {
  const alpha = await open("alpha");
  for (const title of ["One", "Two", "Three"])
    await alpha.createThread({ title, provider: "echo", model: "echo-1" });
  await project("alpha").getByRole("button", { name: "Two", exact: true }).click();
  for (const mode of ["By project", "In one list"] as const) {
    await layout(mode);
    const list = mode === "By project" ? project("alpha").locator(".sessions") : page.locator(".project-list > .sessions");
    await expect(list.locator(".session-title")).toHaveText(["Three", "Two", "One"]);
    for (const [handle, destination, expected] of [
      [".session-title", "Two", ["Three", "One", "Two"]],
      [".session-status", "Three", ["One", "Three", "Two"]],
      ['button[aria-label="Archive chat One"]', "Two", ["Three", "Two", "One"]],
    ] as const) {
      const row = list.locator(".session-row").filter({ has: page.getByRole("button", { name: "One", exact: true }) });
      await electronDrag(row.locator(handle), list.getByRole("button", { name: destination, exact: true }));
      await expect(list.locator(".session-title")).toHaveText([...expected]);
      await expect(list.getByRole("button", { name: "Two", exact: true })).toHaveAttribute("aria-current", "true");
      await expect(page.locator(".order-drag-preview, [data-order-source], [data-drop-position]")).toHaveCount(0);
    }
  }
});

test("sidebar direct Electron input cancellation and tiny movements preserve clicks and disclosure", async () => {
  for (const name of ["alpha", "beta"]) { await open(name); await collapse(name); }
  await createSection("alpha", "555");
  await electronDrag(heading("beta"), section("555").locator(".project-section-row > span"), {
    cancel: true,
    inspect: async () => {
      await expect(section("555").locator(".project-section-row")).toHaveAttribute("data-drop-position", "inside");
      await expect(page.locator(".order-drag-preview")).toHaveText("beta");
    },
  });
  await expect(titles(section("555"))).toHaveText(["alpha"]);
  await expect(titles(section("No section"))).toHaveText(["beta"]);
  await expect(heading("beta")).toHaveAttribute("aria-expanded", "false");
  await expect(page.locator(".order-drag-preview, [data-order-source], [data-drop-position]")).toHaveCount(0);
  // Movement below the drag threshold is still an ordinary disclosure click.
  const button = project("beta").locator(".project-disclosure");
  const box = await button.boundingBox();
  await electronDrag(button, { x: Math.round(box!.x + 10), y: Math.round(box!.y + box!.height / 2 + 2) });
  await expect(heading("beta")).toHaveAttribute("aria-expanded", "true");
  await button.click();
  await expect(heading("beta")).toHaveAttribute("aria-expanded", "false");
});

test("sidebar shared pointer sorting reorders conversation and panel tabs using direct Electron input", async () => {
  const alpha = await open("alpha");
  for (const title of ["One", "Two", "Three"]) {
    await alpha.createThread({ title, provider: "echo", model: "echo-1" });
    await project("alpha").getByRole("button", { name: title, exact: true }).click();
  }
  const tabs = page.locator(".workspace-tabs [role=tab]");
  await expect(tabs).toHaveText(["One", "Two", "Three"]);
  await electronDrag(page.getByRole("tab", { name: "Three", exact: true }), page.getByRole("tab", { name: "One", exact: true }));
  await expect(tabs).toHaveText(["Three", "One", "Two"]);
  await expect(page.getByRole("tab", { name: "Three", exact: true })).toHaveAttribute("aria-selected", "true");
  await page.getByRole("button", { name: "Show side panel", exact: true }).click();
  for (const name of ["Files", "Review"]) {
    await page.getByRole("button", { name: "Open panel", exact: true }).click();
    await page.getByRole("button", { name: `Open ${name}`, exact: true }).click();
  }
  await electronDrag(page.getByRole("button", { name: "Show diff panel", exact: true }), page.getByRole("button", { name: "Show files panel", exact: true }));
  await expect(page.locator(".dock-tab > button:first-child")).toHaveText(["Review", "Files"]);
  await expect(page.locator(".review")).toBeVisible();
  await reload();
  // Reopen in a different sequence to verify the saved order, without
  // asserting that open tabs themselves survive a restart.
  for (const title of ["Two", "One", "Three"])
    await project("alpha").getByRole("button", { name: title, exact: true }).click();
  await expect(tabs).toHaveText(["Three", "One", "Two"]);
  await page.getByRole("button", { name: "Close diff panel", exact: true }).click();
  for (const name of ["Files", "Review"]) {
    await page.getByRole("button", { name: "Open panel", exact: true }).click();
    await page.getByRole("button", { name: `Open ${name}`, exact: true }).click();
  }
  await expect(page.locator(".dock-tab > button:first-child")).toHaveText(["Review", "Files"]);
});

test("sidebar drops projects on section headings and empty No section, persisting after an app restart", async () => {
  for (const name of ["alpha", "beta", "gamma"]) { await open(name); await collapse(name); }
  await createSection("alpha", "Clients");
  await createSection("beta", "Personal");
  await heading("gamma").dragTo(section("Clients").locator(".project-section-row"));
  await expect(titles(section("Clients"))).toHaveText(["alpha", "gamma"]);
  // Every project is now filed. There must still be somewhere to unfile by drag.
  await expect(section("No section")).toBeVisible();
  await heading("gamma").dragTo(section("No section").locator(".project-section-row"));
  await expect(titles(section("No section"))).toHaveText(["gamma"]);
  await heading("alpha").dragTo(section("Personal").locator(".project-section-row"));
  await expect(section("Clients")).toHaveCount(0);
  await expect(titles(section("Personal"))).toHaveText(["beta", "alpha"]);
  await expect(page.locator("[data-drop-position]")).toHaveCount(0);
  await close();
  await launch();
  await expect(titles(section("Personal"))).toHaveText(["beta", "alpha"]);
  await expect(titles(section("No section"))).toHaveText(["gamma"]);
});

test("sidebar session drops accept the space between rows and the list side gutters", async () => {
  const alpha = await open("alpha");
  for (const title of ["One", "Two", "Three"])
    await alpha.createThread({ title, provider: "echo", model: "echo-1" });
  const sessions = project("alpha").locator(".sessions");
  const chat = (name: string) => sessions.getByRole("button", { name, exact: true });
  await expect(sessions.locator(".session-title")).toHaveText(["Three", "Two", "One"]);
  const two = await chat("Two").boundingBox();
  // A human aims for the insertion gap, not necessarily the button centre.
  await dragToPoint(chat("One"), { x: two!.x + 50, y: two!.y - 1 }, async () => {
    await expect(sessions.locator("[data-drop-position]")).toHaveCount(1);
    await page.screenshot({ path: test.info().outputPath("session-gap-indicator.png") });
  });
  await expect(sessions.locator(".session-title")).toHaveText(["Three", "One", "Two"]);
  const last = await chat("Two").boundingBox();
  const list = await sessions.boundingBox();
  await dragToPoint(chat("Three"), { x: list!.x + 2, y: last!.y + last!.height / 2 });
  await expect(sessions.locator(".session-title")).toHaveText(["One", "Two", "Three"]);
  await reload();
  await expect(sessions.locator(".session-title")).toHaveText(["One", "Two", "Three"]);
});

test("sidebar flat sessions can be dropped into the blank end of the list", async () => {
  const alpha = await open("alpha");
  for (const title of ["One", "Two", "Three"])
    await alpha.createThread({ title, provider: "echo", model: "echo-1" });
  await layout("In one list");
  const sessions = page.locator(".project-list > .sessions");
  const labels = sessions.locator(".session-title");
  await expect(labels).toHaveText(["Three", "Two", "One"]);
  const list = await page.locator(".sidebar-lists").boundingBox();
  await dragToPoint(sessions.getByRole("button", { name: "Three", exact: true }), {
    x: list!.x + 50, y: list!.y + list!.height - 20,
  });
  await expect(labels).toHaveText(["Two", "One", "Three"]);
  await expect(page.locator("[data-drop-position]")).toHaveCount(0);
  await expect.poll(async () => JSON.parse(await readFile(join(temporary, "desktop", "desktop.json"), "utf8")).values["layout:v2"]?.sidebarLayout).toBe("flat");
  await reload();
  await expect(labels).toHaveText(["Two", "One", "Three"]);
});

test("sidebar project drops accept section spacing and blank space after No section", async () => {
  for (const name of ["alpha", "beta", "gamma"]) { await open(name); await collapse(name); }
  await createSection("alpha", "Clients");
  await createSection("beta", "Personal");
  const personal = await section("Personal").boundingBox();
  await dragToPoint(heading("gamma"), { x: personal!.x + 60, y: personal!.y + 4 });
  await expect(titles(section("Personal"))).toHaveText(["beta", "gamma"]);
  const unfiled = await section("No section").boundingBox();
  await dragToPoint(heading("gamma"), { x: unfiled!.x + 60, y: unfiled!.y + unfiled!.height + 35 }, async () => {
    await expect(section("No section").locator(".project-section-row")).toHaveAttribute("data-drop-position", "inside");
    await page.screenshot({ path: test.info().outputPath("unfiled-blank-indicator.png") });
  });
  await expect(titles(section("No section"))).toHaveText(["gamma"]);
  await reload();
  await expect(titles(section("Personal"))).toHaveText(["beta"]);
  await expect(titles(section("No section"))).toHaveText(["gamma"]);
});

test("sidebar session mouse reordering remains stable across repeated gestures and live updates", async () => {
  const alpha = await open("alpha");
  const threads = [];
  for (const title of ["One", "Two", "Three", "Four", "Five"])
    threads.push(await alpha.createThread({ title, provider: "echo", model: "echo-1" }));
  const sessions = project("alpha").locator(".sessions");
  const labels = sessions.locator(".session-title");
  let expected = ["Five", "Four", "Three", "Two", "One"];
  await expect(labels).toHaveText(expected);
  await sessions.getByRole("button", { name: "Three", exact: true }).click();
  // The selected row's title; the row also shows when the chat was last active.
  const selected = project("alpha").locator('.session-row > button[aria-current="true"] .session-title');
  for (let round = 0; round < 12; round++) {
    const from = round % 2 ? 0 : expected.length - 1;
    const to = round % 2 ? expected.length - 1 : 0;
    const source = labels.filter({ hasText: new RegExp(`^${expected[from]}$`) });
    const target = labels.filter({ hasText: new RegExp(`^${expected[to]}$`) }).locator("xpath=ancestor::div[contains(@class, 'session-row')][1]");
    const start = await source.boundingBox(), end = await target.boundingBox();
    await page.mouse.move(start!.x + 8, start!.y + start!.height / 2);
    await page.mouse.down();
    // Finish without a pause or a duplicate mouse move before release.
    await page.mouse.move(end!.x + 40, end!.y + (round % 2 ? end!.height - 2 : 2), { steps: 10 });
    if (round === 5) {
      // A server event while the gesture is in flight must not lose its source.
      await alpha.updateThread(threads[2].id, { title: "Three updated" });
      expected = expected.map(name => name === "Three" ? "Three updated" : name);
      await expect(labels).toContainText(["Three updated"]);
    }
    await page.mouse.up();
    const [name] = expected.splice(from, 1);
    expected.splice(to, 0, name);
    await expect(labels).toHaveText(expected);
    await expect(selected).toHaveText(round >= 5 ? "Three updated" : "Three");
    await expect(page.locator("[data-drop-position]")).toHaveCount(0);
  }
  await close();
  await launch();
  await expect(project("alpha").locator(".session-title")).toHaveText(expected);
});

test("sidebar projects can repeatedly move between sections and unfiled blank space", async () => {
  for (const name of ["alpha", "beta", "gamma"]) { await open(name); await collapse(name); }
  await createSection("alpha", "Clients");
  await createSection("beta", "Personal");
  for (let round = 0; round < 3; round++) {
    for (const group of ["Clients", "Personal"]) {
      const destination = await section(group).locator(".project-section-row").boundingBox();
      await dragToPoint(heading("gamma").locator("span"), { x: destination!.x + 5, y: destination!.y + 2 });
      await expect(titles(section(group))).toHaveText([group === "Clients" ? "alpha" : "beta", "gamma"]);
    }
    const destination = await section("No section").locator(".project-section-row").boundingBox();
    await dragToPoint(heading("gamma"), { x: destination!.x + 30, y: destination!.y + destination!.height + 25 });
    await expect(titles(section("No section"))).toHaveText(["gamma"]);
    await expect(page.locator("[data-drop-position]")).toHaveCount(0);
  }
  await close();
  await launch();
  await expect(titles(section("No section"))).toHaveText(["gamma"]);
});

test("sidebar project keyboard and mouse ordering follow visible pinned lanes", async () => {
  for (const name of ["alpha", "beta", "gamma", "delta"]) { await open(name); await collapse(name); }
  // Save an explicit order before pinning, so raw saved order differs from the UI.
  await heading("alpha").press("Alt+End");
  await expect(titles()).toHaveText(["beta", "gamma", "delta", "alpha"]);
  await pin("alpha");
  await pin("gamma");
  await expect(titles()).toHaveText(["gamma", "alpha", "beta", "delta"]);
  await heading("beta").press("Alt+ArrowDown");
  await expect(titles()).toHaveText(["gamma", "alpha", "delta", "beta"]);
  await expect(heading("beta")).toBeFocused();
  await heading("alpha").press("Alt+ArrowUp");
  await expect(titles()).toHaveText(["alpha", "gamma", "delta", "beta"]);
  await heading("beta").dragTo(heading("delta"));
  await expect(titles()).toHaveText(["alpha", "gamma", "beta", "delta"]);
  await heading("alpha").dragTo(heading("gamma"));
  await expect(titles()).toHaveText(["gamma", "alpha", "beta", "delta"]);
  await heading("alpha").press("Alt+End");
  await heading("beta").press("Alt+Home");
  await expect(titles()).toHaveText(["gamma", "alpha", "beta", "delta"]);
  await reload();
  await expect(titles()).toHaveText(["gamma", "alpha", "beta", "delta"]);
  await expect(page.locator(".toast")).toHaveCount(0);
});

test("sidebar row drops handle expanded projects, before and after edges, and cross-section pinned projects", async () => {
  const alpha = await open("alpha");
  await alpha.createThread({ title: "Alpha chat", provider: "echo", model: "echo-1" });
  await open("beta");
  await open("gamma");
  await createSection("alpha", "Clients");
  await createSection("beta", "Personal");
  await heading("gamma").dragTo(heading("alpha"), { targetPosition: { x: 30, y: 2 } });
  await expect(titles(section("Clients"))).toHaveText(["gamma", "alpha"]);
  await heading("gamma").dragTo(heading("alpha"));
  await expect(titles(section("Clients"))).toHaveText(["alpha", "gamma"]);
  const chat = project("alpha").getByRole("button", { name: "Alpha chat", exact: true });
  await expect(chat).toBeVisible();
  await heading("beta").dragTo(chat);
  await expect(titles(section("Clients"))).toHaveText(["alpha", "beta", "gamma"]);
  await expect(section("Personal")).toHaveCount(0);
  await pin("gamma");
  await fileUnder("gamma", "No section");
  await heading("gamma").dragTo(heading("beta"));
  await expect(titles(section("Clients"))).toHaveText(["gamma", "alpha", "beta"]);
  await expect(heading("gamma")).toHaveAttribute("aria-description", "Pinned");
  await expect(page.locator("[data-drop-position]")).toHaveCount(0);
  await reload();
  await expect(titles(section("Clients"))).toHaveText(["gamma", "alpha", "beta"]);
});

test("sidebar canceled drags clear indicators and unrelated session drags never file projects", async () => {
  const alpha = await open("alpha");
  await alpha.createThread({ title: "Alpha chat", provider: "echo", model: "echo-1" });
  await open("beta");
  await createSection("alpha", "Clients");
  const chat = project("alpha").getByRole("button", { name: "Alpha chat", exact: true });
  await chat.dragTo(heading("beta"));
  await expect(titles(section("Clients"))).toHaveText(["alpha"]);
  await expect(titles(section("No section"))).toHaveText(["beta"]);
  const source = await heading("beta").boundingBox();
  const target = await heading("alpha").boundingBox();
  await page.mouse.move(source!.x + 35, source!.y + 15);
  await page.mouse.down();
  await page.mouse.move(target!.x + 35, target!.y + 15, { steps: 15 });
  await page.mouse.move(target!.x + 36, target!.y + 15);
  await expect(project("alpha")).toHaveAttribute("data-drop-position", /before|after/);
  await page.mouse.move(700, 80, { steps: 10 });
  await expect(page.locator("[data-drop-position]")).toHaveCount(0);
  await page.keyboard.press("Escape");
  await page.mouse.up();
  await expect(titles(section("Clients"))).toHaveText(["alpha"]);
  await expect(titles(section("No section"))).toHaveText(["beta"]);
});

test("sidebar section menus rename, merge, cancel removal, and retain project metadata", async () => {
  for (const name of ["alpha", "beta", "gamma"]) { await open(name); await collapse(name); }
  await createSection("alpha", "Clients");
  await fileUnder("beta", "Clients");
  await createSection("gamma", "Personal");
  await pin("beta");
  await options("alpha");
  await item("Edit").click();
  const edit = page.getByRole("dialog", { name: "Edit project", exact: true });
  await edit.getByLabel("Name", { exact: true }).fill("Client app");
  await edit.getByRole("button", { name: "Save", exact: true }).click();
  await section("Clients").getByRole("button", { name: "Section options for Clients", exact: true }).click();
  await item("Rename section…").click();
  const rename = page.getByRole("dialog", { name: "Rename section", exact: true });
  await rename.getByLabel("Name", { exact: true }).fill("Personal");
  await rename.getByRole("button", { name: "Save", exact: true }).click();
  await expect(section("Clients")).toHaveCount(0);
  await expect(titles(section("Personal"))).toHaveText(["beta", "Client app", "gamma"]);
  await section("Personal").getByRole("button", { name: "Section options for Personal", exact: true }).click();
  await item("Remove section").click();
  const remove = page.getByRole("dialog", { name: "Remove section Personal?", exact: true });
  await remove.getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(titles(section("Personal"))).toHaveCount(3);
  await section("Personal").getByRole("button", { name: "Section options for Personal", exact: true }).click();
  await item("Remove section").click();
  await remove.getByRole("button", { name: "Remove section", exact: true }).click();
  await expect(page.locator(".project-section")).toHaveCount(0);
  await reload();
  await expect(titles()).toHaveText(["beta", "Client app", "gamma"]);
  await expect(heading("beta")).toHaveAttribute("aria-description", "Pinned");
});

test("sidebar flat and project session orders stay independent through archive, restore, refresh and restart", async () => {
  const alpha = await open("alpha");
  const one = await alpha.createThread({ title: "One", provider: "echo", model: "echo-1" });
  await alpha.createThread({ title: "Two", provider: "echo", model: "echo-1" });
  await open("beta");
  await expect(project("alpha").locator(".session-title")).toHaveText(["Two", "One"]);
  await project("alpha").getByRole("button", { name: "One", exact: true }).press("Alt+Home");
  await expect(project("alpha").locator(".session-title")).toHaveText(["One", "Two"]);
  await collapse("alpha");
  await layout("In one list");
  const flat = () => page.locator(".project-list > .sessions");
  await expect(flat().locator(".session-title")).toHaveText(["Two", "One"]);
  await flat().getByRole("button", { name: "One", exact: true }).dragTo(flat().getByRole("button", { name: "Two", exact: true }));
  await expect(flat().locator(".session-title")).toHaveText(["One", "Two"]);
  await flat().getByRole("button", { name: "Archive chat One", exact: true }).click();
  await expect(flat().locator(".session-title")).toHaveText(["Two"]);
  await expect.poll(() => alpha.getThread(one.id).then(thread => thread.archived)).toBe(true);
  await alpha.updateThread(one.id, { archived: false });
  await expect(flat().locator(".session-title")).toHaveText(["One", "Two"]);
  await flat().getByRole("button", { name: "One", exact: true }).press("Alt+End");
  await expect(flat().locator(".session-title")).toHaveText(["Two", "One"]);
  await expect.poll(async () => JSON.parse(await readFile(join(temporary, "desktop", "desktop.json"), "utf8")).values["layout:v2"]?.sidebarLayout).toBe("flat");
  await close();
  await launch();
  await expect(flat().locator(".session-title")).toHaveText(["Two", "One"]);
  await layout("By project");
  await expect(heading("alpha")).toHaveAttribute("aria-expanded", "false");
  await heading("alpha").click();
  await expect(project("alpha").locator(".session-title")).toHaveText(["One", "Two"]);
  await options("alpha");
  await item("Refresh project").click();
  await expect(project("alpha").locator(".session-title")).toHaveText(["One", "Two"]);
});

test("sidebar an archived chat says so in the chat; Restore or a new message returns it to the sidebar", async () => {
  const alpha = await open("alpha");
  const thread = await alpha.createThread({ title: "Shelved idea", provider: "echo", model: "echo-1" });
  await alpha.waitTurn((await alpha.startTurn(thread.id, { text: "first thought", requestId: crypto.randomUUID() })).id);
  const row = () => project("alpha").getByRole("button", { name: "Shelved idea", exact: true });
  const notice = page.locator(".conversation .archived-notice");
  await row().click();
  await expect(page.locator(".transcript")).toContainText("first thought");
  await expect(notice).toHaveCount(0);
  await project("alpha").getByRole("button", { name: "Archive chat Shelved idea", exact: true }).click();
  await expect(row()).toHaveCount(0);
  await expect.poll(() => alpha.getThread(thread.id).then(value => value.archived)).toBe(true);

  // Search is the way back in; opening the chat does not restore it by itself.
  await page.getByRole("button", { name: "Search", exact: true }).click();
  await page.getByRole("combobox", { name: "Search Haarness", exact: true }).fill("Shelved");
  await page.getByRole("option", { name: "Shelved idea", exact: true }).click();
  await expect(page.locator(".transcript")).toContainText("first thought");
  await expect(notice).toHaveText("This chat is archived. Sending a message restores it.Restore");
  await expect(notice).toHaveAttribute("role", "status");
  await expect(row()).toHaveCount(0);
  expect((await alpha.getThread(thread.id)).archived).toBe(true);
  await page.screenshot({ path: test.info().outputPath("archived-chat.png") });
  await notice.getByRole("button", { name: "Restore", exact: true }).click();
  await expect(notice).toHaveCount(0);
  await expect(row()).toBeVisible();
  await expect(row()).toHaveAttribute("aria-current", "true");
  expect((await alpha.getThread(thread.id)).archived).toBe(false);

  // Archived elsewhere while open: the notice follows, and continuing the chat restores it.
  await alpha.updateThread(thread.id, { archived: true });
  await expect(notice).toBeVisible();
  await expect(row()).toHaveCount(0);
  await page.getByRole("textbox", { name: "Message", exact: true }).fill("second thought");
  await page.getByRole("button", { name: /Send message|Send guidance/ }).click();
  await expect(page.locator(".transcript")).toContainText("Echo: second thought");
  await expect(notice).toHaveCount(0);
  await expect(row()).toBeVisible();
  expect((await alpha.getThread(thread.id)).archived).toBe(false);
});

test("sidebar removing a section's last project hides its heading and reopening restores its presentation", async () => {
  const alpha = await open("alpha");
  const thread = await alpha.createThread({ title: "Retained chat", provider: "echo", model: "echo-1" });
  await open("beta");
  await createSection("alpha", "Clients");
  await pin("alpha");
  await options("alpha");
  await item("Remove project").click();
  await page.getByRole("dialog", { name: "Remove alpha from the sidebar?", exact: true })
    .getByRole("button", { name: "Remove project", exact: true }).click();
  await expect(project("alpha")).toHaveCount(0);
  await expect(section("Clients")).toHaveCount(0);
  await expect(titles()).toHaveText(["beta"]);
  expect((await alpha.getThread(thread.id)).archived).toBe(false);
  await reload();
  await expect(section("Clients")).toHaveCount(0);
  await open("alpha");
  await expect(titles(section("Clients"))).toHaveText(["alpha"]);
  await expect(heading("alpha")).toHaveAttribute("aria-description", "Pinned");
  await expect(project("alpha").getByRole("button", { name: "Retained chat", exact: true })).toBeVisible();
});

test("sidebar pointer drag scrolls a long list to a section heading", async () => {
  await page.setViewportSize({ width: 1000, height: 700 });
  const alpha = await open("alpha");
  for (let index = 0; index < 28; index++)
    await alpha.createThread({ title: `Scroll chat ${index}`, provider: "echo", model: "echo-1" });
  await open("beta");
  await createSection("alpha", "Clients");
  await createSection("beta", "Personal");
  const list = page.locator(".sidebar-lists");
  await heading("alpha").scrollIntoViewIfNeeded();
  const source = await heading("alpha").boundingBox();
  const box = await list.boundingBox();
  const target = section("Personal").locator(".project-section-row");
  await page.mouse.move(source!.x + 40, source!.y + 15);
  await page.mouse.down();
  await page.mouse.move(box!.x + 80, box!.y + box!.height - 5, { steps: 15 });
  await page.mouse.move(box!.x + 81, box!.y + box!.height - 5);
  await expect.poll(() => list.evaluate(element => element.scrollTop)).toBeGreaterThan(0);
  await expect.poll(async () => {
    const rect = await target.boundingBox();
    return rect!.y + rect!.height <= box!.y + box!.height;
  }).toBe(true);
  // Leave the auto-scroll edge and let the list come to rest before aiming:
  // it keeps scrolling while the pointer stays in that zone.
  await page.mouse.move(box!.x + 80, box!.y + box!.height / 2, { steps: 5 });
  let resting = -1;
  await expect.poll(async () => {
    const top = await list.evaluate(element => element.scrollTop);
    const still = top === resting;
    resting = top;
    return still;
  }, { intervals: [150] }).toBe(true);
  const destination = await target.boundingBox();
  await page.mouse.move(destination!.x + 80, destination!.y + destination!.height / 2, { steps: 10 });
  await page.mouse.move(destination!.x + 81, destination!.y + destination!.height / 2);
  await expect(target).toHaveAttribute("data-drop-position", "inside");
  await page.screenshot({ path: test.info().outputPath("section-drop-indicator.png") });
  await page.mouse.up();
  await expect(section("Clients")).toHaveCount(0);
  await expect(titles(section("Personal"))).toHaveText(["beta", "alpha"]);
  await expect(project("alpha").locator(".session-title")).toHaveCount(28);
  await expect(page.locator("[data-drop-position]")).toHaveCount(0);
});

for (const gesture of ["keyboard", "drag"] as const) {
  test(`sidebar scheduled rows never reorder their owning project through ${gesture}`, async () => {
    const alpha = await open("alpha");
    await alpha.createThread({
      title: "Scheduled run", provider: "echo", model: "echo-1",
      schedule: { id: crypto.randomUUID(), name: "Nightly" },
    });
    await open("beta");
    await collapse("beta");
    await project("alpha").locator(".scheduled-heading").click();
    await project("alpha").locator(".schedule-group-heading").click();
    const run = project("alpha").locator(".schedule-runs .session-row > button").first();
    if (gesture === "keyboard") await run.press("Alt+End");
    else await run.dragTo(heading("beta"));
    await expect(titles()).toHaveText(["alpha", "beta"]);
    await expect(run).toBeVisible();
    await expect(page.locator("[data-drop-position]")).toHaveCount(0);
  });
}

test("sidebar Alt and an arrow step the chats the sidebar shows; on a focused row they still move it", async () => {
  const alpha = await open("alpha");
  for (const title of ["One", "Two", "Three"])
    await alpha.createThread({ title, provider: "echo", model: "echo-1" });
  const row = (title: string) => project("alpha").getByRole("button", { name: title, exact: true });
  await expect(project("alpha").locator(".session-title")).toHaveText(["Three", "Two", "One"]);
  await row("Two").click();
  await expect(row("Two")).toHaveAttribute("aria-current", "true");
  const composer = page.getByRole("textbox", { name: "Message", exact: true });
  await composer.focus();
  await composer.press("Alt+ArrowDown");
  await expect(row("One")).toHaveAttribute("aria-current", "true");
  await expect(composer).toBeFocused();
  // A draft does not stop the step, and past the last row it wraps.
  await composer.fill("draft stays");
  await composer.press("Alt+ArrowDown");
  await expect(row("Three")).toHaveAttribute("aria-current", "true");
  await expect(composer).toBeFocused();
  await composer.press("Alt+ArrowUp");
  await expect(row("One")).toHaveAttribute("aria-current", "true");
  await expect(composer).toHaveValue("draft stays");
  // A sidebar row with the keyboard moves itself and leaves the open chat alone.
  await row("One").press("Alt+ArrowUp");
  await expect(project("alpha").locator(".session-title")).toHaveText(["Three", "One", "Two"]);
  await expect(row("One")).toHaveAttribute("aria-current", "true");
  // The palette offers the same steps.
  await page.getByRole("button", { name: "Search", exact: true }).click();
  await page.getByRole("combobox", { name: "Search Haarness", exact: true }).fill("Next chat");
  await page.getByRole("option", { name: "Next chat", exact: true }).click();
  await expect(row("Two")).toHaveAttribute("aria-current", "true");
});

test("sidebar Focus sidebar shows a hidden sidebar and puts the keyboard on the open chat", async () => {
  const alpha = await open("alpha");
  await alpha.createThread({ title: "Focus me", provider: "echo", model: "echo-1" });
  await alpha.createThread({ title: "Other chat", provider: "echo", model: "echo-1" });
  await project("alpha").getByRole("button", { name: "Focus me", exact: true }).click();
  await collapse("alpha");
  await page.getByRole("button", { name: "Hide sidebar", exact: true }).click();
  await expect(page.locator(".sidebar")).toBeHidden();
  await page.getByRole("textbox", { name: "Message", exact: true }).focus();
  // Control+Shift+L on macOS; elsewhere that is the file editor's Add
  // selection to context, so Focus sidebar is Alt+Shift+L there.
  await page.keyboard.press(process.platform === "darwin" ? "Control+Shift+L" : "Alt+Shift+L");
  await expect(page.locator(".sidebar")).toBeVisible();
  const row = project("alpha").getByRole("button", { name: "Focus me", exact: true });
  await expect(row).toBeFocused();
  await expect(row).toHaveAttribute("aria-current", "true");
});

test("sidebar prompt keys move through the open chat's prompts and back to the newest output", async () => {
  const alpha = await open("alpha");
  const thread = await alpha.createThread({ title: "Long talk", provider: "echo", model: "echo-1" });
  for (let index = 0; index < 3; index++)
    await alpha.waitTurn((await alpha.startTurn(thread.id, { text: "::long 60", requestId: crypto.randomUUID() })).id);
  await project("alpha").getByRole("button", { name: "Long talk", exact: true }).click();
  await expect(page.locator(".transcript")).toContainText("60");
  const reading = page.locator('.outline-dash[aria-current="location"]');
  const latest = page.getByRole("button", { name: "Jump to latest", exact: true });
  const mod = process.platform === "darwin" ? "Meta" : "Control";
  const composer = page.getByRole("textbox", { name: "Message", exact: true });
  await composer.focus();
  await composer.press(`${mod}+Shift+ArrowUp`);
  await expect(reading).toHaveAttribute("aria-label", "Jump to prompt 1");
  await expect(latest).toBeVisible();
  await composer.press(`${mod}+ArrowDown`);
  await expect(reading).toHaveAttribute("aria-label", "Jump to prompt 2");
  // In a draft the keys move the caret instead.
  await composer.fill("x");
  await composer.press(`${mod}+ArrowDown`);
  await expect(reading).toHaveAttribute("aria-label", "Jump to prompt 2");
  await composer.fill("");
  await composer.press(`${mod}+Shift+ArrowDown`);
  await expect(latest).toHaveCount(0);
  // From the newest output, Previous goes back to the last prompt.
  await composer.press(`${mod}+ArrowUp`);
  await expect(reading).toHaveAttribute("aria-label", "Jump to prompt 3");
  await expect(latest).toBeVisible();
});

test("sidebar rows show when a chat was last active, and a scheduled run's waiting label and Archive chats' count speak the reader's language", async () => {
  const de: Record<string, string> = JSON.parse(await readFile(resolve("../../locales/de.json"), "utf8"));
  const alpha = await open("alpha", '[permissions]\nbash="ask"\n');
  await alpha.createThread({ title: "Quiet chat", provider: "echo", model: "echo-1" });
  // The age sits in the row without joining its name.
  const quiet = project("alpha").getByRole("button", { name: "Quiet chat", exact: true });
  await expect(quiet.locator(".session-age")).toHaveText("now");
  await expect(quiet.locator(".session-age")).toHaveAttribute("aria-hidden", "true");
  const run = await alpha.createThread({
    title: "Scheduled run", provider: "echo", model: "echo-1",
    schedule: { id: crypto.randomUUID(), name: "Nightly" },
  });
  await alpha.startTurn(run.id, { requestId: crypto.randomUUID(), text: '!tool bash {"command":"printf waiting-proof"}' });
  await expect.poll(async () => (await alpha.listPermissions()).length).toBe(1);
  await page.evaluate(() => window.haarness.preferences({ language: "de" }));
  const group = page.getByRole("region", { name: de["Project {name}"].replace("{name}", "alpha"), exact: true });
  await expect(group.locator(".session-age")).toHaveText(de["now"]);
  await group.locator(".scheduled-heading").click();
  await group.locator(".schedule-group-heading").click();
  await expect(group.locator(".schedule-runs .session-waiting")).toHaveText(de["Needs approval"]);
  await group.getByRole("button", { name: de["Project options for {name}"].replace("{name}", "alpha"), exact: true }).click();
  await page.getByRole("menuitem", { name: de["Archive chats"], exact: true }).click();
  await page.getByRole("dialog", { name: de["Archive all chats in {name}?"].replace("{name}", "alpha"), exact: true })
    .getByRole("button", { name: de["Archive chats"], exact: true }).click();
  await expect(page.locator(".toast")).toHaveText(de["Archived {count} chats"].replace("{count}", "2"));
  await page.evaluate(() => window.haarness.preferences({ language: "en" }));
});

test("sidebar Select chats archives several chats at once, notes a chat still working, and Escape leaves it", async () => {
  const alpha = await open("alpha", '[permissions]\nbash="ask"\n');
  const threads: Awaited<ReturnType<Haarness["createThread"]>>[] = [];
  for (const title of ["One", "Two", "Three", "Four"])
    threads.push(await alpha.createThread({ title, provider: "echo", model: "echo-1" }));
  // "Two" waits for an approval, so its turn is still running.
  await alpha.startTurn(threads[1].id, { requestId: crypto.randomUUID(), text: '!tool bash {"command":"printf still-working"}' });
  await expect.poll(async () => (await alpha.listPermissions()).length).toBe(1);
  const row = (title: string) => project("alpha").getByRole("button", { name: title, exact: true });
  const bar = page.getByRole("toolbar", { name: "Chat selection", exact: true });
  await row("One").click();
  await options("alpha");
  await item("Select chats").click();
  await expect(bar).toContainText("0 selected");
  await expect(row("Four")).toHaveAttribute("aria-pressed", "false");
  // A click picks without opening; Shift picks the rows since.
  await row("Four").click();
  await row("Two").click({ modifiers: ["Shift"] });
  await expect(bar.getByRole("status")).toHaveText("3 selected");
  for (const title of ["Four", "Three", "Two"]) await expect(row(title)).toHaveAttribute("aria-pressed", "true");
  await expect(row("One")).toHaveAttribute("aria-current", "true");
  await page.screenshot({ path: test.info().outputPath("select-chats-bar.png") });
  await bar.getByRole("button", { name: "Select all shown", exact: true }).click();
  await expect(bar.getByRole("status")).toHaveText("4 selected");
  await row("One").click();
  await expect(row("One")).toHaveAttribute("aria-pressed", "false");
  await bar.getByRole("button", { name: "Archive", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "Archive 3 chats?", exact: true });
  await expect(dialog).toContainText("Archiving does not stop a chat that is still working.");
  await expect(dialog.locator(".dialog-list li")).toHaveText(["Four", "Three", /^Two\s*Working$/]);
  await page.screenshot({ path: test.info().outputPath("select-chats-confirm.png") });
  await dialog.getByRole("button", { name: "Archive", exact: true }).click();
  await expect(page.locator(".toast")).toHaveText("Archived 3 chats");
  await expect(project("alpha").locator(".session-title")).toHaveText(["One"]);
  await expect(bar).toHaveCount(0);
  await expect.poll(async () => (await Promise.all(threads.map(thread => alpha.getThread(thread.id)))).map(thread => thread.archived))
    .toEqual([false, true, true, true]);

  // Command-click (Control-click off macOS) starts picking on that row; Escape ends it.
  await alpha.createThread({ title: "Five", provider: "echo", model: "echo-1" });
  await row("Five").click({ modifiers: [process.platform === "darwin" ? "Meta" : "Control"] });
  await expect(bar.getByRole("status")).toHaveText("1 selected");
  await expect(row("Five")).toHaveAttribute("aria-pressed", "true");
  await expect(row("One")).toHaveAttribute("aria-current", "true");
  await page.keyboard.press("Escape");
  await expect(bar).toHaveCount(0);
  await expect(row("Five")).not.toHaveAttribute("aria-pressed", /.*/);
});

test("sidebar Select chats removes picked worktrees and names the one it keeps for uncommitted changes", async () => {
  const directory = join(temporary, "projects", "repo");
  await mkdir(directory, { recursive: true });
  const git = (...args: string[]) =>
    execFileSync("git", ["-c", "user.name=Haarness E2E", "-c", "user.email=e2e@example.com", ...args], { cwd: directory, stdio: "ignore" });
  git("init", "-q", "-b", "main");
  await writeFile(join(directory, "README.md"), "Select chats fixture\n");
  git("add", "README.md");
  git("commit", "-q", "-m", "Initial commit");
  const repo = await open("repo");
  const clean = (await repo.createWorkspace({ name: "clean" })).workspace;
  const dirty = (await repo.createWorkspace({ name: "dirty" })).workspace;
  checkouts.push(clean.path, dirty.path);
  await writeFile(join(dirty.path, "unsaved.txt"), "work in progress\n");
  await options("repo");
  await item("Enable worktree view").click();
  await options("repo");
  await item("Refresh project").click();
  const checkout = (name: string) => project("repo").getByRole("region", { name: `Checkout ${name}`, exact: true });
  await expect(checkout("clean")).toBeVisible();
  await expect(checkout("dirty")).toBeVisible();
  await options("repo");
  await item("Select chats").click();
  const bar = page.getByRole("toolbar", { name: "Chat selection", exact: true });
  // The project folder itself cannot be picked.
  await expect(project("repo").getByRole("button", { name: /^Select worktree / })).toHaveCount(2);
  await checkout("clean").getByRole("button", { name: "Select worktree clean", exact: true }).click();
  await checkout("dirty").getByRole("button", { name: "Select worktree dirty", exact: true }).click();
  await expect(bar.getByRole("status")).toHaveText("2 selected");
  await expect(bar.getByRole("button", { name: "Archive", exact: true })).toBeDisabled();
  await bar.getByRole("button", { name: "Remove worktrees", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "Remove 2 worktrees?", exact: true });
  // Listed in the order the sidebar draws them.
  await expect(dialog.locator(".dialog-list li")).toHaveCount(2);
  expect((await dialog.locator(".dialog-list li").allTextContents()).sort()).toEqual(["clean", "dirty"]);
  await dialog.getByRole("button", { name: "Remove worktrees", exact: true }).click();
  await expect(page.locator(".toast")).toHaveText("Removed 1 worktree 1 failed: dirty: uncommitted changes");
  await expect(checkout("clean")).toHaveCount(0);
  await expect(checkout("dirty")).toBeVisible();
  await expect(bar).toHaveCount(0);
  const left = (await repo.workspacesList()).workspaces.map(workspace => workspace.id);
  expect(left).not.toContain(clean.id);
  expect(left).toContain(dirty.id);
  expect(await readFile(join(dirty.path, "unsaved.txt"), "utf8")).toBe("work in progress\n");
});
