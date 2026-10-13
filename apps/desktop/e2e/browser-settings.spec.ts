import {
  settingsControls,
  settingsTitles,
} from "../src/renderer/settings-catalog";
import type {
  BrowserSettingsValue,
  BrowserPolicy,
} from "../src/browser-settings";
import {
  test,
  expect,
  _electron as electron,
  type ElectronApplication,
  type Page,
} from "@playwright/test";
import {
  mkdtemp,
  mkdir,
  writeFile,
  readFile,
  realpath,
  rm,
} from "node:fs/promises";
import { tmpdir, homedir } from "node:os";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";
import { createServer, type Server } from "node:http";
import { WebSocketServer } from "ws";
import { Haarness, type Thread } from "@haarness/sdk";
let app: ElectronApplication,
  page: Page,
  temporary: string,
  site: Server,
  wss: WebSocketServer,
  siteURL: string;
let env: Record<string, string>,
  endpoint: { url: string; token: string },
  client: Haarness,
  thread: Thread,
  connection: string;
let tabID: string;
let protectedRequests = 0;
/** WebSockets that reached the fixture on its localhost name, the one the tests block. */
let blockedSockets = 0;
const errors: string[] = [];
const directories: string[] = [];
const evidence = process.env.HAARNESS_BROWSER_EVIDENCE;
const binary = resolve(
  process.env.HAARNESS_BIN ?? "../../target/debug/haarness",
);
test.describe.configure({ mode: "serial" });
const html = `<!doctype html><html lang="en"><head><title>Browser fixture</title><meta name="description" content="Haarness browser integration fixture"><meta name="viewport" content="width=device-width"></head><body style="font:18px system-ui;background:#edf2ff;color:#14213d;padding:25px"><h1>Haarness browser fixture</h1><form id="form"><label>Name <input id="name" name="name"></label><label>Tier <select id="tier"><option value="one">One</option><option value="two">Two</option></select></label><label><input type="checkbox" id="agree"> Agree</label><button>Submit order</button></form><p id="result" role="status">Ready</p><label>Upload document<input type="file" id="upload"></label><p id="uploaded"></p><a href="/download" download="fixture.txt">Download fixture</a><button onclick="window.open('/popup')">Open popup</button><iframe title="Child form" src="/frame"></iframe><p id="socket">Connecting socket</p><script>form.onsubmit=e=>{e.preventDefault();result.textContent='Saved '+nameInput.value+' '+tier.value+' '+agree.checked;fetch('/api',{method:'POST',body:nameInput.value})};const nameInput=document.getElementById('name');upload.onchange=async()=>uploaded.textContent=await upload.files[0].text();console.log('fixture console ready');const socket=new WebSocket(location.origin.replace('http','ws')+'/ws');socket.onmessage=e=>document.getElementById('socket').textContent=e.data;</script></body></html>`;
/** Tabs once Desktop's browser is attached, else -1: servers answer a tab
 * list without a browser too, naming which browsers the chat can use. */
function attachedTabs(state: any): number {
  const haarness = state.browsers?.find(
    (entry: any) => entry.browser === "haarness",
  );
  return haarness?.available === false ? -1 : state.tabs.length;
}
async function tool(
  name: string,
  args: object = {},
  target = thread,
  base = endpoint.url,
) {
  const sdk = new Haarness({ baseUrl: base, token: endpoint.token });
  const result = await sdk.callBrowserTool(
    target.id,
    { name: "browser_" + name, arguments: args as any },
    { timeoutMs: 90_000 },
  );
  sdk.close();
  if (result.isError) throw new Error(result.output);
  return {
    value: JSON.parse(result.output),
    metadata: result.metadata as { attachments: string[]; result: unknown },
  };
}
async function inspect(id = tabID) {
  return (await tool("snapshot", { tabID: id })).value.content as string;
}
function ref(snapshot: string, text: string) {
  const line = snapshot
    .split("\n")
    .find((line) => /@e\d+/.test(line) && line.includes(JSON.stringify(text)));
  const value = line?.match(/@e\d+/)?.[0];
  if (!value) throw new Error("No ref for " + text + " in " + snapshot);
  return value;
}
async function select(value: Thread) {
  await page.getByRole("button", { name: value.title, exact: true }).click();
  await expect
    .poll(() =>
      page.evaluate(
        (connection) => window.haarness.storage("session:" + connection),
        connection,
      ),
    )
    .toBe(value.id);
  await expect
    .poll(async () => {
      try {
        return attachedTabs((await tool("tabs_list", {}, value)).value);
      } catch {
        return -1;
      }
    })
    .toBeGreaterThanOrEqual(0);
}
test.beforeAll(async () => {
  test.setTimeout(240000);
  temporary = await realpath(
    await mkdtemp(join(tmpdir(), "haarness-browser-settings-")),
  );
  env = Object.fromEntries(
    Object.entries(process.env).filter(
      (x): x is [string, string] => x[1] !== undefined,
    ),
  );
  Object.assign(env, {
    HAARNESS_BIN: binary,
    HAARNESS_HOME: join(temporary, "home"),
    HAARNESS_DATA_DIR: join(temporary, "state"),
    HAARNESS_DESKTOP_DATA: join(temporary, "desktop"),
    HAARNESS_TEST_PROVIDER: "1",
    HAARNESS_DESKTOP_TEST: "1",
  });
  delete env.ELECTRON_RUN_AS_NODE;
  if (process.env.HAARNESS_BROWSER_LIVE === "1" && !env.OPENAI_API_KEY) {
    const auth = JSON.parse(
      await readFile(
        join(homedir(), ".local/share/haarness/auth.json"),
        "utf8",
      ),
    );
    env.OPENAI_API_KEY = auth.openai.key;
  }
  for (const name of ["browser-project", "remote-project"]) {
    const dir = join(temporary, name);
    directories.push(dir);
    await mkdir(join(dir, ".haarness"), { recursive: true });
    await writeFile(
      join(dir, ".haarness/config.toml"),
      '[defaults]\nprovider="echo"\nmodel="echo-1"\n',
    );
    await writeFile(join(dir, "upload.txt"), "Server upload 日本");
    for (const args of [
      ["init", "-b", "main"],
      ["add", "."],
      [
        "-c",
        "user.name=Browser Fixture",
        "-c",
        "user.email=fixture@example.test",
        "commit",
        "-m",
        "Fixture",
      ],
    ])
      execFileSync("git", args, { cwd: dir, stdio: "ignore" });
  }
  site = createServer((request, response) => {
    response.setHeader("Access-Control-Allow-Origin", "*");
    if (request.url === "/redirect-protected") {
      response.writeHead(302, {
        Location: siteURL.replace("127.0.0.1", "localhost") + "/protected",
      });
      response.end();
      return;
    }
    if (request.url?.startsWith("/protected")) {
      protectedRequests++;
      response.setHeader("Content-Type", "text/html");
      response.end(
        "<title>Protected fixture</title><h1>Protected content</h1>",
      );
      return;
    }
    if (request.url === "/download") {
      response.writeHead(200, {
        "Content-Type": "text/plain",
        "Content-Disposition": 'attachment; filename="fixture.txt"',
      });
      response.end("Downloaded from fixture");
      return;
    }
    if (request.url === "/api") {
      response.setHeader("Content-Type", "application/json");
      response.end('{"saved":true}');
      return;
    }
    response.setHeader("Content-Type", "text/html");
    response.end(
      request.url === "/popup"
        ? "<title>Popup fixture</title><h1>Popup isolated</h1>"
        : request.url === "/frame"
          ? "<label>Frame name<input></label>"
          : html,
    );
  });
  wss = new WebSocketServer({ server: site, path: "/ws" });
  wss.on("connection", (socket, request) => {
    if (/^localhost\.?:/i.test(request.headers.host ?? "")) blockedSockets++;
    socket.send("WebSocket connected through server");
  });
  await new Promise<void>((resolve) => site.listen(0, "127.0.0.1", resolve));
  siteURL = "http://127.0.0.1:" + (site.address() as any).port;
  if (evidence) await mkdir(evidence, { recursive: true });
  app = await electron.launch({ args: ["."], env, timeout: 120000 });
  // Leave native page dialogs for Haarness; Playwright otherwise auto-dismisses them.
  app.context().on("dialog", () => {});
  page = await app.firstWindow();
  page.on("pageerror", (error) => errors.push(error.message));
  await app.evaluate(({ BrowserWindow }) => {
    const win = BrowserWindow.getAllWindows()[0];
    win.setContentSize(1500, 980);
    win.show();
  });
  await app.evaluate(({ dialog }, directory) => {
    dialog.showOpenDialog = async () => ({
      canceled: false,
      filePaths: [directory],
    });
  }, directories[0]);
  await page
    .getByRole("region", { name: "Project home" })
    .getByRole("button", { name: "Open project", exact: true })
    .click();
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
  );
  endpoint = JSON.parse(
    execFileSync(
      binary,
      ["--workspace", directories[0], "--trust-project", "desktop"],
      { env, input: '{"action":"connect"}', encoding: "utf8", timeout: 30000 },
    ),
  );
  client = new Haarness({ baseUrl: endpoint.url, token: endpoint.token });
  connection = "local:" + directories[0];
  thread = await client.createThread({
    title: "Browser integration",
    provider: "echo",
    model: "echo-1",
  });
  await client.createThread({
    title: "Other conversation",
    provider: "echo",
    model: "echo-1",
  });
  await select(thread);
});
test.afterAll(async () => {
  await app?.close().catch(() => {});
  client?.close();
  wss?.clients.forEach((socket) => socket.terminate());
  wss?.close();
  await new Promise<void>((resolve) => site?.close(() => resolve()));
  for (const dir of directories)
    try {
      execFileSync(
        binary,
        ["--workspace", dir, "--trust-project", "serve", "--stop"],
        { env, stdio: "ignore", timeout: 30000 },
      );
    } catch {}
  if (evidence) {
    await mkdir(evidence, { recursive: true });
    await writeFile(join(evidence, "fixture-path.txt"), temporary);
  } else if (temporary) await rm(temporary, { recursive: true, force: true });
});
const settings = () =>
  page.getByRole("dialog", { name: "Settings", exact: true });
async function openSettings(section = "Browser") {
  if (!(await settings().count()))
    await page.getByRole("button", { name: "Settings", exact: true }).click();
  await settings().getByRole("textbox", { name: "Filter settings" }).fill("");
  await settings().getByRole("button", { name: section, exact: true }).click();
  if (section === "Browser")
    await expect(
      settings().getByRole("switch", { name: "Agent browser control" }),
    ).toBeEnabled();
}
async function closeSettings() {
  if (await settings().count())
    await settings()
      .getByRole("button", { name: "Close settings", exact: true })
      .click();
}
async function choose(name: string, value: string) {
  await settings().getByRole("combobox", { name, exact: true }).click();
  await page.getByRole("option", { name: value, exact: true }).click();
  await expect(
    settings().getByRole("combobox", { name, exact: true }),
  ).toBeEnabled();
  await expect(
    settings().getByRole("combobox", { name, exact: true }),
  ).toContainText(value);
}
async function policy(scope = "project") {
  return (
    await client.managementView("browser-settings", { action: "get", scope })
  ).data as unknown as BrowserSettingsValue;
}
async function savePolicy(
  change: (value: BrowserPolicy) => void,
  scope = "project",
) {
  const before = await policy(scope);
  const next = structuredClone(before.scopePolicy ?? before.inheritedPolicy);
  change(next);
  return (
    await client.managementView("browser-settings", {
      action: "set",
      scope,
      revision: before.revision,
      policy: next,
    } as any)
  ).data as unknown as BrowserSettingsValue;
}
async function globalFind(query: string, title: string) {
  await app.evaluate(({ BrowserWindow }) => {
    BrowserWindow.getAllWindows()[0].webContents.send("haarness:event", {
      kind: "menu",
      data: "palette",
    });
  });
  await page
    .getByRole("combobox", { name: "Search Haarness", exact: true })
    .fill(query);
  await page.getByRole("option", { name: title, exact: true }).click();
}

test("current working folder leads Projects settings and remains searchable in light and dark windows", async () => {
  for (const [theme, width, height] of [
    ["Dark", 1500, 980], ["Light", 1500, 980], ["Dark", 1000, 740],
  ] as const) {
    await openSettings("General");
    await choose("Appearance", theme);
    await app.evaluate(({ BrowserWindow }, size) => {
      BrowserWindow.getAllWindows()[0].setContentSize(size.width, size.height);
    }, { width, height });
    await openSettings("Projects");
    const folder = settings().getByRole("region", { name: "Current working folder" });
    await expect(folder).toContainText(directories[0]);
    await expect(folder.getByRole("heading", { name: "browser-project", exact: true })).toBeVisible();
    await expect(settings().locator(".settings-content > :first-child")).toHaveAttribute("data-setting", "Current working folder");
    await expect(settings().getByRole("button", { name: "Change project trust" })).toHaveCount(0);
    await expect(settings()).not.toContainText("project code trusted");
    expect(await folder.evaluate(element => element.scrollWidth <= element.clientWidth)).toBe(true);
    await folder.getByRole("button", { name: "Refresh project" }).click();
    await expect(folder).toContainText(directories[0]);
    const name = `projects-current-folder-${theme.toLowerCase()}-${width}.png`;
    await settings().screenshot({ path: evidence ? join(evidence, name) : test.info().outputPath(name) });
  }
  await globalFind("current working folder", "Current working folder");
  await expect(settings().getByRole("button", { name: "Refresh project" })).toBeFocused();
  await expect(settings().getByRole("region", { name: "Current working folder" })).toHaveClass(/setting-search-target/);
  await app.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows()[0].setContentSize(1500, 980));
  await closeSettings();
  expect(errors).toEqual([]);
});

test("settings search keeps long results readable in narrow, scaled, light and dark windows", async () => {
  const filter = () =>
    settings().getByRole("textbox", { name: "Filter settings" });
  const results = () =>
    settings().getByRole("navigation", { name: "Settings search results" });
  for (const [theme, width, height, zoom] of [
    ["dark", 1500, 980, 1],
    ["light", 1500, 980, 1],
    ["dark", 1000, 740, 1],
    ["dark", 1000, 740, 1.25],
  ] as const) {
    await test.step(`${theme} ${width} at ${zoom} zoom`, async () => {
      await openSettings("General");
      await choose("Appearance", theme === "dark" ? "Dark" : "Light");
      await app.evaluate(
        ({ BrowserWindow }, size) => {
          const win = BrowserWindow.getAllWindows()[0];
          win.setContentSize(size.width, size.height);
          win.webContents.setZoomFactor(size.zoom);
        },
        { width, height, zoom },
      );
      await openSettings();
      await filter().fill("bro");
      await expect(
        results().getByRole("button", {
          name: "browser_cpu_analyze Settings › Tools",
          exact: true,
        }),
      ).toBeAttached();
      expect(await results().getByRole("button").count()).toBeGreaterThan(20);
      // Check every result, including the offscreen rows, so a large catalog
      // cannot compress rows, overlap labels, or create a horizontal scrollbar.
      expect(
        await settings()
          .locator("aside")
          .evaluate((aside) => {
            const nav = aside.querySelector<HTMLElement>("nav")!;
            const buttons = Array.from(nav.querySelectorAll("button"));
            const failures: string[] = [];
            for (const element of [aside, nav, ...buttons]) {
              if (element.scrollWidth > element.clientWidth + 1)
                failures.push("Horizontal overflow: " + element.tagName);
            }
            buttons.forEach((button, index) => {
              const row = button.getBoundingClientRect();
              if (row.height < 44)
                failures.push("Compressed row: " + button.textContent);
              if (
                index &&
                buttons[index - 1].getBoundingClientRect().bottom > row.top
              )
                failures.push("Overlapping rows: " + button.textContent);
              const [title, detail] = Array.from(button.children).map((child) =>
                child.getBoundingClientRect(),
              );
              if (
                title.bottom > detail.top ||
                title.top < row.top ||
                detail.bottom > row.bottom
              )
                failures.push("Overlapping labels: " + button.textContent);
              if (
                title.left < row.left ||
                title.right > row.right ||
                detail.right > row.right
              )
                failures.push("Uncontained label: " + button.textContent);
              if (button.title !== button.querySelector("span")?.textContent)
                failures.push(
                  "Missing full label tooltip: " + button.textContent,
                );
            });
            return failures;
          }),
      ).toEqual([]);
      const screenshot = `settings-search-${theme}-${width}-${zoom}.png`;
      await filter().hover();
      const screenshotPath = evidence
        ? join(evidence, screenshot)
        : test.info().outputPath(screenshot);
      if (zoom === 1) {
        await settings().screenshot({ path: screenshotPath });
      } else {
        // Electron's native capture includes zoom; Playwright's locator clip
        // coordinates do not account for webContents.setZoomFactor().
        const png = await app.evaluate(async ({ BrowserWindow }) =>
          (await BrowserWindow.getAllWindows()[0].webContents.capturePage())
            .toPNG()
            .toString("base64"),
        );
        await writeFile(screenshotPath, Buffer.from(png, "base64"));
      }
      if (theme === "dark" && width === 1500) {
        await settings()
          .locator("aside")
          .screenshot({
            path: evidence
              ? join(evidence, "settings-search-sidebar.png")
              : test.info().outputPath("settings-search-sidebar.png"),
          });
      }
      // Keyboard navigation must reveal results at the end of the scroll area
      // while keeping search and the version footer visible.
      const last = results().getByRole("button").last();
      await last.focus();
      await expect(last).toBeInViewport();
      await expect(filter()).toBeInViewport();
      await expect(settings().locator("aside > .small")).toBeInViewport();
      await filter().focus();
      await filter().press("Tab");
      await expect(results().getByRole("button").first()).toBeFocused();
      await page.keyboard.press("Enter");
      await expect(filter()).toHaveValue("");
      await expect(
        settings().getByRole("switch", { name: "Agent browser control" }),
      ).toBeVisible();
    });
  }
  await filter().fill("browser_cpu_analyze");
  const toolResult = results().getByRole("button", {
    name: "browser_cpu_analyze Settings › Tools",
    exact: true,
  });
  await expect(toolResult).toHaveAttribute("title", "browser_cpu_analyze");
  await toolResult.click();
  await expect(
    settings().locator('[data-setting="tool:browser_cpu_analyze"]'),
  ).toHaveClass(/setting-search-target/);
  await filter().fill("no-such-setting-xyz");
  await expect(results().getByText("No matching settings.")).toBeVisible();
  await filter().fill("");
  await expect(results()).toHaveCount(0);
  await expect(
    settings().getByRole("button", { name: "General", exact: true }),
  ).toBeVisible();
  await app.evaluate(({ BrowserWindow }) => {
    const win = BrowserWindow.getAllWindows()[0];
    win.webContents.setZoomFactor(1);
    win.setContentSize(1500, 980);
  });
  await closeSettings();
  expect(errors).toEqual([]);
});

test("every settings control is discoverable and focused from Settings and global search", async () => {
  test.setTimeout(300000);
  await openSettings();
  for (const [section, title] of settingsControls) {
    await test.step("Settings search: " + title, async () => {
      await settings()
        .getByRole("textbox", { name: "Filter settings" })
        .fill(title);
      await settings()
        .getByRole("navigation", { name: "Settings search results" })
        .getByRole("button", {
          name: `${title} Settings › ${settingsTitles[section]}`,
          exact: true,
        })
        .click();
      await expect(
        settings().locator(`[data-setting="${title}"]`),
      ).toBeVisible();
      await expect(settings().locator(`[data-setting="${title}"]`)).toHaveClass(
        /setting-search-target/,
      );
    });
    await test.step("Global search: " + title, async () => {
      await globalFind(title, title);
      await expect(
        settings().locator(`[data-setting="${title}"]`),
      ).toBeVisible();
    });
  }
  await globalFind("Shortcut: Open browser", "Shortcut: Open browser");
  await expect(
    settings().getByRole("textbox", { name: "Shortcut browser", exact: true }),
  ).toBeFocused();
  await globalFind("cookies", "Browsing data");
  await expect(
    settings().locator('[data-setting="Browsing data"]'),
  ).toHaveClass(/setting-search-target/);
  await settings()
    .getByRole("textbox", { name: "Filter settings" })
    .fill("zzzzz no match");
  await expect(
    settings().getByText("No matching settings.", { exact: true }),
  ).toBeVisible();
  await openSettings();
  await page.screenshot({
    path: evidence
      ? join(evidence, "browser-settings.png")
      : test.info().outputPath("browser-settings.png"),
  });
  await closeSettings();
});

test("settings disable agent execution live, keep manual browsing, cancel waits, and persist", async () => {
  tabID = (
    await tool("tabs_open", {
      url: siteURL + "/settings-proof?mode=full#anchor",
    })
  ).value.id;
  await openSettings();
  await settings()
    .getByRole("switch", { name: "Agent browser control" })
    .uncheck();
  await expect.poll(async () => (await policy()).policy.enabled).toBe(false);
  await expect(tool("snapshot", { tabID })).rejects.toThrow(
    /disabled.*Settings|Agent browser control is disabled/i,
  );
  const manual = (await page.evaluate(
    ({ connection, thread, url }) =>
      window.haarness.browserCommand(connection, thread, {
        type: "tabs.open",
        url,
      }),
    { connection, thread: thread.id, url: siteURL + "/manual" },
  )) as any;
  expect(manual.url).toBe(siteURL + "/manual");
  await page.reload();
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
  );
  await openSettings();
  await expect(
    settings().getByRole("switch", { name: "Agent browser control" }),
  ).not.toBeChecked();
  await settings()
    .getByRole("switch", { name: "Agent browser control" })
    .check();
  await expect.poll(async () => (await policy()).policy.enabled).toBe(true);
  await closeSettings();
  await tool("navigate", { tabID, url: siteURL });
  const marker = "never-visible-" + crypto.randomUUID();
  const waiting = tool("wait", {
    tabID,
    condition: "text",
    text: marker,
    timeoutMs: 30000,
  }).then(() => "unexpected success", String);
  await expect
    .poll(async () =>
      (await client.listItems(thread.id)).some(
        (item: any) =>
          item.data.type === "tool_call" &&
          item.data.arguments?.text === marker,
      ),
    )
    .toBe(true);
  await openSettings();
  await settings()
    .getByRole("switch", { name: "Agent browser control" })
    .uncheck();
  expect(await waiting).toMatch(/disabled|settings changed/i);
  await settings()
    .getByRole("switch", { name: "Agent browser control" })
    .check();
  await expect.poll(async () => (await policy()).policy.enabled).toBe(true);
  await closeSettings();
  expect(await inspect()).toContain("Haarness browser fixture");
});

test("site rules prompt, block, isolate capabilities, and reject pending stale approvals", async () => {
  await openSettings();
  const siteOrigin = settings().getByRole("textbox", { name: "Site origin" });
  await siteOrigin.click();
  await siteOrigin.fill("https://draft.example.test");
  const refresh = await siteOrigin.evaluateHandle((input) => {
    let interruptions = 0;
    const observer = new MutationObserver((records) => { interruptions += records.length; });
    observer.observe(input, { attributes: true, attributeFilter: ["disabled"] });
    return { finish() { observer.disconnect(); return interruptions; } };
  });
  await savePolicy(policy => { policy.defaults.downloads = "ask"; });
  await expect(settings().getByRole("combobox", { name: "Default downloads", exact: true })).toContainText("Requires approval");
  expect(await refresh.evaluate(value => value.finish())).toBe(0);
  await refresh.dispose();
  await expect(siteOrigin).toHaveValue("https://draft.example.test");
  await savePolicy(policy => { policy.defaults.downloads = "allow"; });
  await expect(settings().getByRole("combobox", { name: "Default downloads", exact: true })).toContainText("Always allow");
  await settings()
    .getByRole("textbox", { name: "Site origin" })
    .fill(siteURL + "/not-an-origin");
  await settings()
    .getByRole("button", { name: "Add site", exact: true })
    .click();
  await expect(settings().getByRole("alert")).toContainText(
    "exact HTTP(S) origin",
  );
  await siteOrigin.click();
  await siteOrigin.fill(siteURL);
  await expect(siteOrigin).toHaveValue(siteURL);
  await settings()
    .getByRole("button", { name: "Add site", exact: true })
    .click();
  await expect.poll(async () => (await policy()).policy.sites.length).toBe(1);
  await choose(siteURL + " browsing", "Requires approval");
  await expect
    .poll(async () => (await policy()).policy.sites[0].browsing)
    .toBe("ask");
  const pending = tool("snapshot", { tabID });
  await expect
    .poll(async () => (await client.listPermissions()).length)
    .toBe(1);
  const approval = (await client.listPermissions())[0];
  expect(approval.allowAlways).toBe(false);
  await expect(page.locator(".approval").getByRole("button", { name: /Always|Auto/ })).toHaveCount(0);
  await client.replyPermission(approval.permissionId, { decision: "once" });
  expect((await pending).value.content).toContain("Haarness browser fixture");
  const stale = tool("snapshot", { tabID }).then(
    () => "unexpected success",
    String,
  );
  await expect
    .poll(async () => (await client.listPermissions()).length)
    .toBe(1);
  await settings()
    .getByRole("switch", { name: "Agent browser control" })
    .uncheck();
  await expect.poll(async () => (await policy()).policy.enabled).toBe(false);
  await client.replyPermission(
    (await client.listPermissions())[0].permissionId,
    { decision: "always" },
  );
  expect(await stale).toMatch(/disabled/i);
  await settings()
    .getByRole("switch", { name: "Agent browser control" })
    .check();
  await choose(siteURL + " browsing", "Block");
  await expect(tool("snapshot", { tabID })).rejects.toThrow(/denied/i);
  await choose(siteURL + " browsing", "Always allow");
  await choose(siteURL + " uploads", "Block");
  const snapshot = await inspect();
  await expect(
    tool("files_upload", {
      tabID,
      ref: ref(snapshot, "Upload document"),
      paths: ["upload.txt"],
    }),
  ).rejects.toThrow(/denied/i);
  await choose(siteURL + " uploads", "Always allow");
  await tool("files_upload", {
    tabID,
    ref: ref(snapshot, "Upload document"),
    paths: ["upload.txt"],
  });
  await choose(siteURL + " downloads", "Block");
  await tool("click", { tabID, ref: ref(snapshot, "Download fixture") });
  const download = async () =>
    (await tool("files_list", { tabID })).value.files.find(
      (file: any) => file.name === "fixture.txt" && file.state === "completed",
    );
  await expect.poll(download).toBeTruthy();
  await expect(
    tool("files_get", { tabID, fileID: (await download()).id }),
  ).rejects.toThrow(/denied/i);
  await choose(siteURL + " downloads", "Always allow");
  const downloaded = await tool("files_get", {
    tabID,
    fileID: (await download()).id,
  });
  expect(await readFile(downloaded.value.files[0].path, "utf8")).toBe(
    "Downloaded from fixture",
  );
  await closeSettings();
});

test("parity: remembered site approvals and combined external uploads keep deny boundaries", async () => {
  await savePolicy(policy => { policy.enabled=true; policy.sites=[]; policy.defaults.browsing="ask"; policy.rememberApprovals=true; });
  await openSettings();
  await expect(settings().getByRole("checkbox", {name:"Remember site approvals"})).toBeChecked();
  await closeSettings();
  const first=tool("snapshot",{tabID});
  await expect.poll(async()=>(await client.listPermissions()).length).toBe(1);
  const approval=(await client.listPermissions())[0];
  expect(approval.permission).toBe("browser_site");
  // The fixture workspace is a git repository, so the rule's scope is a choice.
  await page.getByRole("button",{name:"Always allow",exact:true}).click();
  await page.getByRole("menuitem",{name:"Always for this folder",exact:true}).click();
  expect((await first).value.content).toContain("Haarness browser fixture");
  expect(await inspect()).toContain("Haarness browser fixture");
  await savePolicy(policy=>{policy.defaults.browsing="allow";policy.defaults.uploads="ask";});
  const source=join(temporary,"external-upload.txt");await writeFile(source,"COMBINED_UPLOAD_PROOF");
  const snapshot=await inspect();
  const upload=tool("files_upload",{tabID,ref:ref(snapshot,"Upload document"),paths:[source]});
  await expect.poll(async()=>(await client.listPermissions()).length).toBe(1);
  const combined=(await client.listPermissions())[0];
  expect(combined.permission).toBe("multiple");
  expect(combined.patterns!.some(pattern=>pattern.startsWith("external_directory:"))).toBe(true);
  expect(combined.patterns!.some(pattern=>pattern.startsWith("browser_site:uploads:"))).toBe(true);
  await expect(page.locator(".approval")).toContainText("external_directory:");
  await expect(page.locator(".approval")).toContainText("browser_site:uploads:");
  await expect(page.locator(".approval")).toContainText("external-upload.txt");
  await expect(page.locator(".approval")).not.toContainText("__browser");
  await page.screenshot({path:test.info().outputPath("combined-upload-approval.png")});
  await page.getByRole("button",{name:"Allow once",exact:true}).click();await upload;
  expect(await inspect()).toContain("COMBINED_UPLOAD_PROOF");
  await savePolicy(policy=>{policy.defaults.browsing="deny";});
  await expect(tool("snapshot",{tabID})).rejects.toThrow(/denied/i);
  await savePolicy(policy=>{policy.defaults.browsing="allow";policy.defaults.uploads="allow";policy.rememberApprovals=false;});
});

test("global policy inheritance, project overrides, stale updates, and remote routing", async () => {
  const original = await policy("global");
  await savePolicy((policy) => {
    policy.enabled = false;
  }, "global");
  expect((await policy()).policy.enabled).toBe(true); // Project override remains authoritative.
  const before = await policy();
  await client.managementView("browser-settings", {
    action: "reset",
    revision: before.revision,
  } as any);
  expect((await policy()).policy.enabled).toBe(false);
  const reset = await policy();
  await openSettings();
  await expect(
    settings().getByRole("switch", { name: "Agent browser control" }),
  ).not.toBeChecked();
  await settings()
    .getByRole("switch", { name: "Agent browser control" })
    .check();
  await expect.poll(async () => (await policy()).policy.enabled).toBe(true);
  await expect(
    client.managementView("browser-settings", {
      action: "set",
      revision: reset.revision,
      policy: before.policy,
    } as any),
  ).rejects.toThrow(/changed|Refresh/i);
  const remote = await page.evaluate(
    (input) => window.haarness.connect(input),
    {
      url: endpoint.url,
      token: endpoint.token,
      workspace: directories[1],
      label: "Browser settings remote",
    },
  );
  const remoteClient = new Haarness({
    baseUrl: endpoint.url + "/v1/host/workspaces/" + remote!.remoteWorkspace,
    token: endpoint.token,
  });
  const remotePolicy = (
    await remoteClient.managementView("browser-settings", { action: "get" })
  ).data as unknown as BrowserSettingsValue;
  expect(remotePolicy.policy.enabled).toBe(false);
  await remoteClient.managementView("browser-settings", {
    action: "set",
    revision: remotePolicy.revision,
    policy: { ...remotePolicy.policy, enabled: true },
  } as any);
  expect(
    (
      (await remoteClient.managementView("browser-settings", { action: "get" }))
        .data as unknown as BrowserSettingsValue
    ).policy.enabled,
  ).toBe(true);
  expect((await policy()).policy.enabled).toBe(true);
  const currentGlobal = await policy("global");
  await client.managementView("browser-settings", {
    action: "set",
    scope: "global",
    revision: currentGlobal.revision,
    policy: original.scopePolicy ?? original.inheritedPolicy,
  } as any);
  remoteClient.close();
  await closeSettings();
});

test("web and localhost link routing plus full URL display use persisted desktop preferences", async () => {
  await openSettings();
  await choose("Local URL destination", "System browser");
  await choose("Web link destination", "Haarness");
  await app.evaluate(({ shell }) => {
    (globalThis as any).openedBrowserLinks = [];
    shell.openExternal = async (url) => {
      (globalThis as any).openedBrowserLinks.push(url);
    };
  });
  expect(
    await page.evaluate(
      ({ connection, thread, url }) =>
        window.haarness.openLink(connection, thread, url),
      { connection, thread: thread.id, url: siteURL + "/external" },
    ),
  ).toBe("external");
  expect(
    await app.evaluate(() => (globalThis as any).openedBrowserLinks),
  ).toEqual([siteURL + "/external"]);
  await choose("Local URL destination", "Haarness");
  await settings().getByRole("switch", { name: "Show full URL" }).uncheck();
  const url = siteURL + "/full/path?query=yes#fragment";
  expect(
    await page.evaluate(
      ({ connection, thread, url }) =>
        window.haarness.openLink(connection, thread, url),
      { connection, thread: thread.id, url },
    ),
  ).toBe("browser");
  await closeSettings();
  const address = page.getByRole("textbox", {
    name: "Browser address",
    exact: true,
  });
  await expect(address).toHaveValue(siteURL);
  await address.focus();
  await expect(address).toHaveValue(url);
  // Blur must not accidentally navigate to the shortened origin.
  await page.getByRole("button", { name: "Go", exact: true }).click();
  await expect
    .poll(async () => {
      const inventory = (await tool("tabs_list")).value;
      return inventory.tabs.find(
        (tab: any) => tab.id === inventory.focusedTabID,
      )?.url;
    })
    .toBe(url);
  await openSettings();
  const preferences = (await page.evaluate(() => window.haarness.boot()))
    .preferences;
  expect(preferences.browserLocalLinks).toBe("browser");
  expect(preferences.browserWebLinks).toBe("browser");
  expect(preferences.shortcuts.browser).toBe("Mod+Shift+B");
  await app.close();
  app = await electron.launch({ args: ["."], env, timeout: 120000 });
  app.context().on("dialog", () => {});
  page = await app.firstWindow();
  page.on("pageerror", (error) => errors.push(error.message));
  await app.evaluate(({ BrowserWindow }) => {
    const win = BrowserWindow.getAllWindows()[0];
    win.setContentSize(1500, 980);
    win.show();
  });
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
  );
  await openSettings();
  await expect(
    settings().getByRole("switch", { name: "Show full URL" }),
  ).not.toBeChecked();
  await settings().getByRole("switch", { name: "Show full URL" }).check();
  await closeSettings();
});

test("browsing history and downloads can be searched, saved, removed, and cleared without losing project files", async () => {
  await openSettings();
  await settings()
    .getByRole("button", { name: "Manage browsing history", exact: true })
    .click();
  await settings()
    .getByRole("textbox", { name: "Search browser records" })
    .fill("full/path");
  const history = settings().getByRole("region", {
    name: "Browsing history",
    exact: true,
  });
  await expect
    .poll(() => history.locator(".browser-record").count())
    .toBeGreaterThan(0);
  await settings()
    .getByRole("button", { name: "Manage downloads", exact: true })
    .click();
  const downloads = settings().getByRole("region", {
    name: "Download history",
    exact: true,
  });
  await expect(
    downloads.getByText("fixture.txt", { exact: true }),
  ).toBeVisible();
  const saved = join(temporary, "saved-download.txt");
  await app.evaluate(({ dialog }, filePath) => {
    dialog.showSaveDialog = async () => ({ canceled: false, filePath });
  }, saved);
  await downloads
    .getByRole("button", { name: "Save as…", exact: true })
    .click();
  await expect
    .poll(async () => readFile(saved, "utf8").catch(() => "missing"))
    .toBe("Downloaded from fixture");
  await downloads
    .getByRole("button", { name: "Remove download fixture.txt", exact: true })
    .click();
  await expect(
    downloads.getByText("No matching downloads.", { exact: true }),
  ).toBeVisible();
  expect(await readFile(saved, "utf8")).toBe("Downloaded from fixture");
  const autoSaved = join(temporary, "auto-saved-download.txt");
  await app.evaluate(({ dialog }, filePath) => {
    dialog.showSaveDialog = async () => ({ canceled: false, filePath });
  }, autoSaved);
  await settings()
    .getByRole("switch", { name: "Ask where to save downloads" })
    .check();
  await tool("navigate", { tabID, url: siteURL });
  const snapshot = await inspect();
  await tool("click", { tabID, ref: ref(snapshot, "Download fixture") });
  await expect
    .poll(async () => readFile(autoSaved, "utf8").catch(() => "missing"))
    .toBe("Downloaded from fixture");
  await settings()
    .getByRole("switch", { name: "Ask where to save downloads" })
    .uncheck();
  await page.screenshot({
    path: evidence
      ? join(evidence, "browser-settings-history.png")
      : test.info().outputPath("browser-settings-history.png"),
  });
  await tool("evaluate", {
    tabID,
    script:
      'document.cookie="clear_me=yes;path=/";localStorage.setItem("clear_me","yes");document.cookie',
  });
  const remote = (
    await page.evaluate(() => window.haarness.boot())
  ).recent.find((entry) => entry.remote)!;
  // The restart reconnects the selected project; explicitly reconnect the
  // second fixture before exercising its project-scoped browser data.
  await page.evaluate((id) => window.haarness.connect({ recent: id }), remote.id);
  const otherThread = await page.evaluate(
    (connection) =>
      window.haarness.api<Thread>(connection, "createThread", [
        {
          title: "Other project browsing data",
          provider: "echo",
          model: "echo-1",
        },
      ]),
    remote.id,
  );
  await page.evaluate(
    ({ connection, thread, url }) =>
      window.haarness.browserCommand(connection, thread, {
        type: "tabs.open",
        url,
      }),
    {
      connection: remote.id,
      thread: otherThread.id,
      url: siteURL + "/other-project",
    },
  );
  await expect
    .poll(async () =>
      page.evaluate(
        (connection) => window.haarness.browserData(connection),
        remote.id,
      ),
    )
    .toMatchObject({
      history: expect.arrayContaining([
        expect.objectContaining({ url: siteURL + "/other-project" }),
      ]),
    });
  await settings()
    .getByRole("button", { name: "Clear browsing data", exact: true })
    .click();
  const confirmation = page.getByRole("dialog", {
    name: "Clear browsing data?",
    exact: true,
  });
  await confirmation
    .getByRole("button", { name: "Clear browsing data", exact: true })
    .click();
  await expect
    .poll(async () =>
      page.evaluate(
        (connection) => window.haarness.browserData(connection),
        connection,
      ),
    )
    .toEqual({ history: [], downloads: [] });
  expect(await readFile(join(directories[0], "upload.txt"), "utf8")).toBe(
    "Server upload 日本",
  );
  expect(await readFile(saved, "utf8")).toBe("Downloaded from fixture");
  await closeSettings();
  await expect
    .poll(async () => {
      try {
        return attachedTabs((await tool("tabs_list")).value);
      } catch {
        return -1;
      }
    })
    .toBe(0);
  const clean = (await tool("tabs_open", { url: siteURL })).value.id;
  const storage = (
    await tool("evaluate", {
      tabID: clean,
      script:
        '({cookie:document.cookie,local:localStorage.getItem("clear_me")})',
    })
  ).value.value;
  expect(storage).toEqual({ cookie: "", local: null });
  expect(
    (
      await page.evaluate(
        (connection) => window.haarness.browserData(connection),
        remote.id,
      )
    ).history.some((entry) => entry.url === siteURL + "/other-project"),
  ).toBe(true);
});

test("notification settings persist and browser settings render without UI errors", async () => {
  await openSettings("General");
  await choose("Turn completion notifications", "Always");
  await settings()
    .getByRole("switch", { name: "Permission notifications", exact: true })
    .uncheck();
  await settings()
    .getByRole("switch", { name: "Question notifications", exact: true })
    .uncheck();
  const preferences = (await page.evaluate(() => window.haarness.boot()))
    .preferences;
  expect(preferences.completionNotifications).toBe("always");
  expect(preferences.permissionNotifications).toBe(false);
  expect(preferences.questionNotifications).toBe(false);
  await page.reload();
  await openSettings("General");
  await expect(
    settings().getByRole("combobox", {
      name: "Turn completion notifications",
      exact: true,
    }),
  ).toContainText("Always");
  await expect(
    settings().getByRole("switch", {
      name: "Permission notifications",
      exact: true,
    }),
  ).not.toBeChecked();
  await expect(
    settings().getByRole("switch", {
      name: "Question notifications",
      exact: true,
    }),
  ).not.toBeChecked();
  await app.evaluate(({ Notification }) => {
    (globalThis as any).settingsNotificationRecords = [];
    Notification.isSupported = () => true;
    Notification.prototype.show = function () {
      (globalThis as any).settingsNotificationRecords.push({
        title: this.title,
        body: this.body,
      });
    };
    Notification.prototype.close = function () {
      this.emit("close");
    };
  });
  await closeSettings();
  const completed = await client.startTurn(thread.id, {
    text: "Notification settings proof",
    requestId: crypto.randomUUID(),
  });
  await client.waitTurn(completed.id);
  await expect
    .poll(async () =>
      app.evaluate(
        () => (globalThis as any).settingsNotificationRecords.length,
      ),
    )
    .toBe(1);
  await openSettings("General");
  await choose("Turn completion notifications", "Never");
  await closeSettings();
  const silent = await client.startTurn(thread.id, {
    text: "Silent completion settings proof",
    requestId: crypto.randomUUID(),
  });
  await client.waitTurn(silent.id);
  await expect
    .poll(async () =>
      app.evaluate(
        () => (globalThis as any).settingsNotificationRecords.length,
      ),
    )
    .toBe(1);
  expect(errors).toEqual([]);
});

test("site boundaries cover redirects, script requests, and cached back navigation", async () => {
  await closeSettings();
  const blockedOrigin = siteURL.replace("127.0.0.1", "localhost");
  const tab = (await tool("tabs_open", { url: siteURL })).value.id;
  await savePolicy((policy) => {
    policy.sites = [
      {
        origin: blockedOrigin,
        browsing: "deny",
        downloads: "deny",
        uploads: "deny",
      },
    ];
  });
  const before = protectedRequests;
  await expect(
    tool("evaluate", {
      tabID: tab,
      script: `fetch(${JSON.stringify(blockedOrigin + "/protected")}).then(r => r.text())`,
    }),
  ).rejects.toThrow(/blocked|fetch|failed|permission/i);
  expect(protectedRequests).toBe(before);
  await expect(
    tool("navigate", { tabID: tab, url: siteURL + "/redirect-protected" }),
  ).rejects.toThrow(/blocked|failed|permission/i);
  expect(protectedRequests).toBe(before);
  // A WebSocket counts as its http(s) origin: the page cannot open one to a
  // blocked site during an agent operation.
  await tool("navigate", { tabID: tab, url: siteURL });
  const sockets = blockedSockets;
  const socketTo = (url: string) =>
    `new Promise((resolve) => { const socket = new WebSocket(${JSON.stringify(url)}); socket.onopen = () => { socket.close(); resolve("open"); }; socket.onerror = () => resolve("refused"); })`;
  await expect(
    tool("evaluate", {
      tabID: tab,
      script: socketTo(blockedOrigin.replace(/^http/, "ws") + "/ws"),
    }),
  ).rejects.toThrow(/Browser site permission blocked http:\/\/localhost:\d+\. This site is blocked/);
  expect(blockedSockets).toBe(sockets);
  // A trailing dot spells the same blocked site, for the server and the page alike.
  const dotted = blockedOrigin.replace("localhost", "localhost.");
  await expect(
    tool("evaluate", {
      tabID: tab,
      script: socketTo(dotted.replace(/^http/, "ws") + "/ws"),
    }),
  ).rejects.toThrow(/Browser site permission blocked http:\/\/localhost:\d+\. This site is blocked/);
  await expect(
    tool("evaluate", {
      tabID: tab,
      script: `fetch(${JSON.stringify(dotted + "/protected")}).then(r => r.text())`,
    }),
  ).rejects.toThrow(/blocked|fetch|failed|permission/i);
  await expect(
    tool("navigate", { tabID: tab, url: dotted + "/protected" }),
  ).rejects.toThrow(/denied|blocked/i);
  expect(blockedSockets).toBe(sockets);
  expect(protectedRequests).toBe(before);
  // The page's own origin stays reachable, sockets included.
  expect(
    (await tool("evaluate", { tabID: tab, script: socketTo(siteURL.replace(/^http/, "ws") + "/ws") })).value.value,
  ).toBe("open");
  // Manual browsing remains possible, but an agent cannot revisit the cached page with Back.
  await page.evaluate(
    ({ connection, thread, tab, url }) =>
      window.haarness.browserCommand(connection, thread, {
        type: "navigate",
        tabID: tab,
        url,
      }),
    { connection, thread: thread.id, tab, url: blockedOrigin + "/protected" },
  );
  expect(protectedRequests).toBeGreaterThan(before);
  await tool("navigate", { tabID: tab, url: siteURL });
  await expect(tool("back", { tabID: tab })).rejects.toThrow(/denied|blocked/i);
  await savePolicy((policy) => {
    policy.sites = [];
  });
});

test("plan mode can inspect a real browser page while mutation remains denied", async () => {
  await closeSettings();
  const plan = await client.createThread({
    title: "Plan browser observations", provider: "echo", model: "echo-1", agent: "plan",
  });
  await select(plan);
  const opened = await page.evaluate(
    ({ connection, id, url }) => window.haarness.browserCommand(connection, id, { type: "tabs.open", url }),
    { connection, id: plan.id, url: siteURL },
  ) as { id: string };
  const snapshot = await tool("snapshot", { tabID: opened.id }, plan);
  expect(snapshot.value.content).toContain("Haarness browser fixture");
  await expect(tool("evaluate", { tabID: opened.id, script: "document.body.textContent='PLAN_MUTATION'" }, plan)).rejects.toThrow(/denied|disabled/i);
  await expect(tool("navigate", { tabID: opened.id, url: siteURL + "/popup" }, plan)).rejects.toThrow(/denied|disabled/i);
  expect((await tool("snapshot", { tabID: opened.id }, plan)).value.content).toContain("Haarness browser fixture");
  expect(await client.listPermissions()).toHaveLength(0);
  await page.screenshot({ path: test.info().outputPath("plan-browser-observation.png") });
  await select(thread);
});

test("an attached TUI observes Desktop browser settings on actual chat tool calls", async () => {
  const tui = "haarness-browser-settings-" + crypto.randomUUID().slice(0, 8);
  const target = await client.createThread({
    title: "Settings TUI proof",
    provider: "echo",
    model: "echo-1",
  });
  await closeSettings();
  await select(target);
  await openSettings();
  try {
    execFileSync(
      "tmux",
      [
        "new-session",
        "-d",
        "-s",
        tui,
        "-x",
        "140",
        "-y",
        "44",
        "env",
        "HAARNESS_SERVER_TOKEN=" + endpoint.token,
        "HAARNESS_HOME=" + join(temporary, "tui"),
        binary,
        "attach",
        endpoint.url,
        "--thread",
        target.id,
      ],
      { env },
    );
    await expect
      .poll(() =>
        execFileSync("tmux", ["capture-pane", "-p", "-t", tui], {
          encoding: "utf8",
        }),
      )
      .toContain("Settings TUI proof");
    async function send() {
      const before = (await client.listItems(target.id)).length;
      execFileSync("tmux", [
        "send-keys",
        "-t",
        tui,
        "-l",
        " !tool browser_tabs_list {}",
      ]);
      execFileSync("tmux", ["send-keys", "-t", tui, "Enter"]);
      await expect
        .poll(async () =>
          (await client.listItems(target.id))
            .slice(before)
            .some((item: any) => item.data.type === "assistant_message"),
        )
        .toBe(true);
      return (await client.listItems(target.id)).slice(before);
    }
    await settings()
      .getByRole("switch", { name: "Agent browser control" })
      .uncheck();
    await expect.poll(async () => (await policy()).policy.enabled).toBe(false);
    const denied = await send();
    expect(JSON.stringify(denied)).toContain(
      "Agent browser control is disabled",
    );
    await settings()
      .getByRole("switch", { name: "Agent browser control" })
      .check();
    await expect.poll(async () => (await policy()).policy.enabled).toBe(true);
    const allowed = await send();
    expect(
      allowed.some(
        (item: any) =>
          item.data.type === "tool_result" &&
          item.data.is_error === false &&
          String(item.data.output).includes("focusedTabID"),
      ),
    ).toBe(true);
    await savePolicy((policy) => {
      policy.defaults.browsing = "ask";
      policy.rememberApprovals = false;
    });
    for (const answer of ["y", "n"]) {
      execFileSync("tmux", ["send-keys", "-t", tui, "-l", " !tool browser_tabs_list {}"]);
      execFileSync("tmux", ["send-keys", "-t", tui, "Enter"]);
      await expect.poll(async () => (await client.listPermissions()).length).toBe(1);
      expect((await client.listPermissions())[0].allowAlways).toBe(false);
      await expect.poll(() => execFileSync("tmux", ["capture-pane", "-p", "-t", tui], { encoding: "utf8" })).toContain("Allow once");
      const capture = execFileSync("tmux", ["capture-pane", "-p", "-t", tui], { encoding: "utf8" });
      expect(capture).not.toContain("Always");
      expect(capture).not.toContain("Repository");
      if (evidence) await writeFile(join(evidence, `forced-approval-${answer}.txt`), capture);
      execFileSync("tmux", ["send-keys", "-t", tui, answer]);
      await expect.poll(async () => (await client.listPermissions()).length).toBe(0);
      await expect.poll(async () => await client.activeTurn(target.id)).toBeNull();
    }
    await savePolicy((policy) => { policy.defaults.browsing = "allow"; });
    if (evidence) {
      await writeFile(
        join(evidence, "settings-tui.txt"),
        execFileSync("tmux", ["capture-pane", "-p", "-S", "-500", "-t", tui], {
          encoding: "utf8",
        }),
      );
      await writeFile(
        join(evidence, "settings-tui-items.json"),
        JSON.stringify(await client.listItems(target.id), null, 2),
      );
    }
  } finally {
    try {
      execFileSync("tmux", ["kill-session", "-t", tui]);
    } catch {}
  }
  await closeSettings();
  await select(thread);
});

test("real natural-language chat honors browser enable and disable settings", async () => {
  test.skip(
    process.env.HAARNESS_BROWSER_LIVE !== "1",
    "Set HAARNESS_BROWSER_LIVE=1 for real-model validation",
  );
  test.setTimeout(240000);
  await closeSettings();
  const live = await client.createThread({
    title: "Browser settings live proof",
    provider: "openai",
    model: process.env.HAARNESS_BROWSER_MODEL ?? "gpt-5.6-luna",
  });
  await select(live);
  const marker = "settings-proof-" + crypto.randomUUID().slice(0, 8);
  const turn = await client.startTurn(live.id, {
    text: `Use the native browser tools to open ${siteURL}, fill the Name field with ${marker}, submit the form, and verify the saved text. Use the browser only, no shell or fetch.`,
    requestId: crypto.randomUUID(),
  });
  expect(
    (
      await client.waitTurn(
        turn.id,
        { timeout_ms: 180000 },
        { timeoutMs: 185000 },
      )
    ).status,
  ).toBe("completed");
  const inventory = (await tool("tabs_list", {}, live)).value;
  const proof = (
    await tool(
      "evaluate",
      {
        tabID: inventory.tabs[0].id,
        script: 'document.getElementById("result").textContent',
      },
      live,
    )
  ).value.value;
  expect(proof).toContain("Saved " + marker);
  await openSettings();
  await settings()
    .getByRole("switch", { name: "Agent browser control" })
    .uncheck();
  await expect.poll(async () => (await policy()).policy.enabled).toBe(false);
  const disabled = await client.startTurn(live.id, {
    text: "Call browser_tabs_list exactly once to check whether browser access is allowed now. If it is disabled, report that and stop. Do not use another tool or change any setting.",
    requestId: crypto.randomUUID(),
  });
  expect(
    (
      await client.waitTurn(
        disabled.id,
        { timeout_ms: 90000 },
        { timeoutMs: 95000 },
      )
    ).status,
  ).toBe("completed");
  const items = await client.listItems(live.id);
  expect(
    JSON.stringify(items.filter((item) => item.turnId === disabled.id)),
  ).toContain("Agent browser control is disabled");
  if (evidence)
    await writeFile(
      join(evidence, "settings-live-items.json"),
      JSON.stringify(items, null, 2),
    );
  await settings()
    .getByRole("switch", { name: "Agent browser control" })
    .check();
  await expect.poll(async () => (await policy()).policy.enabled).toBe(true);
  await closeSettings();
  await select(thread);
});

test("browser settings remain usable in light, dark, and smaller windows", async () => {
  await savePolicy((policy) => {
    policy.sites = [
      {
        origin: "https://example.com",
        browsing: "ask",
        downloads: "deny",
        uploads: "allow",
      },
    ];
  });
  for (const [theme, width, height] of [
    ["dark", 1500, 980],
    ["light", 1500, 980],
    ["dark", 1000, 740],
  ] as const) {
    await openSettings("General");
    await choose("Appearance", theme === "dark" ? "Dark" : "Light");
    await app.evaluate(
      ({ BrowserWindow }, size) =>
        BrowserWindow.getAllWindows()[0].setContentSize(
          size.width,
          size.height,
        ),
      { width, height },
    );
    await openSettings();
    await settings()
      .locator('[data-setting="Agent site permissions"]')
      .scrollIntoViewIfNeeded();
    await expect(
      settings().getByRole("combobox", {
        name: "https://example.com browsing",
        exact: true,
      }),
    ).toBeVisible();
    expect(
      await settings()
        .locator(".settings-content")
        .evaluate((element) => element.scrollWidth <= element.clientWidth + 1),
    ).toBe(true);
    if (evidence)
      await page.screenshot({
        path: join(evidence, `browser-settings-${theme}-${width}.png`),
      });
  }
  await choose("https://example.com browsing", "Always allow");
  await expect
    .poll(async () => (await policy()).policy.sites[0].browsing)
    .toBe("allow");
  await savePolicy((policy) => {
    policy.sites = [];
  });
  await app.evaluate(({ BrowserWindow }) =>
    BrowserWindow.getAllWindows()[0].setContentSize(1500, 980),
  );
  await closeSettings();
  expect(errors).toEqual([]);
});
