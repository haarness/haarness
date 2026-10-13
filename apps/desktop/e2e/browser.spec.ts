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
  second: Thread,
  connection: string;
let tabID: string;
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
    await mkdtemp(join(tmpdir(), "haarness-browser-")),
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
  wss.on("connection", (socket) =>
    socket.send("WebSocket connected through server"),
  );
  await new Promise<void>((resolve) => site.listen(0, "127.0.0.1", resolve));
  siteURL = "http://127.0.0.1:" + (site.address() as any).port;
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
  second = await client.createThread({
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
test("browser UI, server tools, forms, frames, console and WebSocket work together", async () => {
  await page
    .getByRole("button", { name: "Show side panel", exact: true })
    .click();
  await page.getByRole("button", { name: "Open Browser", exact: true }).click();
  await page.getByRole("textbox", { name: "Browser address" }).fill(siteURL);
  await page.getByRole("button", { name: "Go", exact: true }).click();
  await expect
    .poll(async () => (await tool("tabs_list")).value.tabs[0]?.title)
    .toBe("Browser fixture");
  tabID = (await tool("tabs_list")).value.tabs[0].id;
  let snapshot = await inspect();
  await tool("fill", {
    tabID,
    ref: ref(snapshot, "Name "),
    text: "Haarness 日本",
  });
  await tool("select", { tabID, ref: ref(snapshot, "Tier "), values: ["two"] });
  await tool("check", { tabID, ref: ref(snapshot, " Agree"), checked: true });
  await tool("click", { tabID, ref: ref(snapshot, "Submit order") });
  await tool("wait", {
    tabID,
    condition: "text",
    text: "Saved Haarness 日本 two true",
  });
  expect(
    (
      await tool("evaluate", {
        tabID,
        script:
          '({saved:document.getElementById("result").textContent,socket:document.getElementById("socket").textContent,node:typeof process,bridge:typeof window.haarness})',
      })
    ).value.value,
  ).toEqual({
    saved: "Saved Haarness 日本 two true",
    socket: "WebSocket connected through server",
    node: "undefined",
    bridge: "undefined",
  });
  const frames = (await tool("frames", { tabID })).value.frames;
  const frame = frames.find((frame: any) => frame.url.endsWith("/frame"));
  expect(frame).toBeTruthy();
  expect(
    (await tool("snapshot", { tabID, frameID: frame.id })).value.content,
  ).toContain("Frame name");
  expect(
    (await tool("console", { tabID })).value.messages.some((message: any) =>
      message.text.includes("fixture console ready"),
    ),
  ).toBeTruthy();
  const requests = (await tool("network_list", { tabID, urlContains: "/api" }))
    .value.requests;
  expect(requests.length).toBe(1);
  expect(
    (
      await tool("network_get", {
        tabID,
        id: requests[0].id,
        includeBody: true,
      })
    ).value.responseBody,
  ).toBeTruthy();
  await expect(
    tool("click", { tabID, ref: ref(snapshot, "Submit order") }),
  ).rejects.toThrow(/ref|snapshot|element/i);
});
test("screenshots, upload and downloads cross the server boundary as real bytes", async () => {
  await tool("tabs_focus", { tabID });
  await expect(page.getByRole("region", { name: "Web browser" })).toBeVisible();
  const shot = await tool("screenshot", { tabID, maxWidth: 1000 });
  expect(shot.metadata.attachments).toHaveLength(1);
  expect(shot.value.files[0].workspaceRelativePath).toMatch(
    /^\.\/\.haarness-chat-attachments\//,
  );
  expect(join(directories[0], shot.value.files[0].workspaceRelativePath)).toBe(
    shot.value.files[0].path,
  );
  const bytes = await readFile(shot.metadata.attachments[0]);
  expect(bytes.subarray(1, 4).toString()).toBe("PNG");
  if (evidence) {
    await mkdir(evidence, { recursive: true });
    await writeFile(join(evidence, "browser-capture.png"), bytes);
    const windowId = await app.evaluate(
      ({ BrowserWindow }) =>
        BrowserWindow.getAllWindows()[0].getMediaSourceId().split(":")[1],
    );
    try {
      execFileSync(
        "/usr/sbin/screencapture",
        ["-x", "-l", windowId, join(evidence, "desktop-browser.png")],
        { stdio: "pipe" },
      );
    } catch {
      await rm(join(evidence, "desktop-browser.png"), { force: true });
      await writeFile(
        join(evidence, "window-capture-unavailable.txt"),
        "The OS did not permit a window capture. Browser pixels are in browser-capture.png.",
      );
    }
  }
  let snapshot = await inspect();
  await tool("files_upload", {
    tabID,
    ref: ref(snapshot, "Upload document"),
    paths: ["upload.txt"],
  });
  await tool("wait", { tabID, condition: "text", text: "Server upload 日本" });
  snapshot = await inspect();
  await tool("click", { tabID, ref: ref(snapshot, "Download fixture") });
  let file: any;
  await expect
    .poll(async () => {
      file = (await tool("files_list", { tabID })).value.files.find(
        (file: any) => file.name === "fixture.txt",
      );
      return file?.state;
    })
    .toBe("completed");
  const output = await tool("files_get", { tabID, fileID: file.id });
  expect(await readFile(output.value.files[0].path, "utf8")).toBe(
    "Downloaded from fixture",
  );
  await writeFile(
    join(directories[0], "large.bin"),
    Buffer.alloc(5 * 1024 * 1024 + 1),
  );
  snapshot = await inspect();
  await expect(
    tool("files_upload", {
      tabID,
      ref: ref(snapshot, "Upload document"),
      paths: ["large.bin"],
    }),
  ).rejects.toThrow(/5 MiB/);
});
test("browser controls remain responsive during waits and pages yield to dialogs", async () => {
  const id = (await tool("tabs_open", { url: siteURL + "/popup" })).value.id;
  const visible = () =>
    app.evaluate(
      ({ BrowserWindow }, url) =>
        BrowserWindow.getAllWindows()[0].contentView.children.some(
          (view) =>
            "webContents" in view &&
            (view.webContents as Electron.WebContents).getURL() === url &&
            view.getVisible(),
        ),
      siteURL + "/popup",
    );
  await expect.poll(visible).toBe(true);
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  await expect(
    page.getByRole("dialog", { name: "Settings", exact: true }),
  ).toBeVisible();
  await expect.poll(visible).toBe(false);
  await page
    .getByRole("button", { name: "Close settings", exact: true })
    .click();
  await expect.poll(visible).toBe(true);
  const marker = "never-appears-" + crypto.randomUUID();
  const pending = tool("wait", {
    tabID: id,
    condition: "text",
    text: marker,
    timeoutMs: 30000,
  }).then(
    () => "unexpected success",
    (error) => String(error),
  );
  await expect
    .poll(async () =>
      (await client.listItems(thread.id)).some(
        (item) =>
          item.data.type === "tool_call" &&
          (item.data as any).arguments?.text === marker,
      ),
    )
    .toBe(true);
  const started = Date.now();
  await page
    .getByRole("button", {
      name: "Close browser tab Popup fixture",
      exact: true,
    })
    .click();
  expect(await pending).toMatch(/cancel|closed|abort/i);
  expect(Date.now() - started).toBeLessThan(5000);
  expect(
    (await tool("tabs_list")).value.tabs.some((tab: any) => tab.id === id),
  ).toBe(false);
  if (tabID) await tool("tabs_focus", { tabID });
});

/** The window as the user sees it: the page with each visible native browser view drawn over it. */
async function composite(path: string) {
  const shots = await app.evaluate(async ({ BrowserWindow }) => {
    const win = BrowserWindow.getAllWindows()[0];
    const views = [];
    for (const view of win.contentView.children) {
      if (!("webContents" in view) || !view.getVisible()) continue;
      const contents = view.webContents as Electron.WebContents;
      if (contents === win.webContents) continue;
      views.push({ bounds: view.getBounds(), data: (await contents.capturePage()).toDataURL() });
    }
    return {
      base: (await win.webContents.capturePage()).toDataURL(),
      width: win.getContentSize()[0],
      views,
    };
  });
  const data = await page.evaluate(async (shots) => {
    const load = (src: string) =>
      new Promise<HTMLImageElement>((resolve, reject) => {
        const image = new Image();
        image.onload = () => resolve(image);
        image.onerror = reject;
        image.src = src;
      });
    const base = await load(shots.base);
    const canvas = document.createElement("canvas");
    canvas.width = base.naturalWidth;
    canvas.height = base.naturalHeight;
    const scale = base.naturalWidth / shots.width;
    const context = canvas.getContext("2d")!;
    context.drawImage(base, 0, 0);
    for (const view of shots.views)
      context.drawImage(
        await load(view.data),
        view.bounds.x * scale,
        view.bounds.y * scale,
        view.bounds.width * scale,
        view.bounds.height * scale,
      );
    return canvas.toDataURL("image/png");
  }, shots);
  await mkdir(join(path, ".."), { recursive: true });
  await writeFile(path, Buffer.from(data.split(",")[1], "base64"));
}
test("the Browser comes back when the agent takes the shown tab to another page, not for a reload or a background tab", async () => {
  const browser = page.getByRole("region", { name: "Web browser" });
  const sidePanel = page.locator(".side-panel");
  const hide = async () => {
    await page.getByRole("button", { name: "Hide side panel", exact: true }).click();
    await expect(sidePanel).toBeHidden();
  };
  /** Waits for a command to settle, then checks the panel stayed as it was. */
  const stays = async (check: () => Promise<void>) => {
    await page.waitForTimeout(750);
    await check();
  };
  await tool("tabs_focus", { tabID });
  await expect(browser).toBeVisible();
  // The user closed the side panel; the agent takes the shown tab elsewhere.
  await hide();
  await tool("navigate", { tabID, url: siteURL + "/frame" });
  await expect(browser).toBeVisible();
  await composite("test-results/browser-reopened.png");
  // The user closed the Browser tab itself, leaving the panel launcher.
  await page.getByRole("button", { name: "Close browser panel", exact: true }).click();
  await expect(browser).toBeHidden();
  await tool("navigate", { tabID, url: siteURL + "/popup" });
  await expect(browser).toBeVisible();
  // A reload is the same page again.
  await hide();
  await tool("reload", { tabID });
  await stays(() => expect(sidePanel).toBeHidden());
  // Back and Forward are other pages.
  await tool("back", { tabID });
  await expect(browser).toBeVisible();
  await hide();
  await tool("forward", { tabID });
  await expect(browser).toBeVisible();
  // A tab the agent keeps in the background loads pages where it is.
  const background = (await tool("tabs_open", { url: siteURL + "/popup", focus: false })).value.id;
  await hide();
  await tool("navigate", { tabID: background, url: siteURL + "/frame" });
  await stays(() => expect(sidePanel).toBeHidden());
  // Another panel on screen keeps the side panel; the Browser tab gets a dot.
  await page.getByRole("button", { name: "Show side panel", exact: true }).click();
  await page.getByRole("button", { name: "Open panel", exact: true }).click();
  await page.getByRole("button", { name: "Open Context", exact: true }).click();
  const browserTab = page.getByRole("button", { name: "Show browser panel", exact: true });
  await expect(page.locator(".dock-tab.active")).toContainText("Context");
  await tool("navigate", { tabID, url: siteURL });
  await expect(browserTab.locator(".dock-tab-dot")).toBeVisible();
  await expect(browserTab).toHaveAttribute("aria-description", "The agent opened another page");
  await expect(page.locator(".dock-tab.active")).toContainText("Context");
  await expect(browser).toBeHidden();
  await composite("test-results/browser-dot.png");
  await browserTab.click();
  await expect(browser).toBeVisible();
  await expect(browserTab.locator(".dock-tab-dot")).toHaveCount(0);
  await expect(browserTab).not.toHaveAttribute("aria-description", /./);
  // The dot is about this chat's page; another chat shows none.
  await page.getByRole("button", { name: "Show context panel", exact: true }).click();
  await tool("navigate", { tabID, url: siteURL + "/frame" });
  await expect(browserTab.locator(".dock-tab-dot")).toBeVisible();
  await select(second);
  await expect(browserTab.locator(".dock-tab-dot")).toHaveCount(0);
  await select(thread);
  await expect(page.locator(".dock-tab.active")).toContainText("Context");
  await page.getByRole("button", { name: "Close context panel", exact: true }).click();
  await tool("tabs_close", { tabID: background });
  // Leave the tab on the fixture page the following tests read.
  await tool("navigate", { tabID, url: siteURL });
  await tool("tabs_focus", { tabID });
  await expect(browser).toBeVisible();
  expect(errors).toEqual([]);
});

test("browser transport requires authentication and the owning host lease", async () => {
  const base = endpoint.url + "/v1/threads/" + thread.id + "/browser";
  const unauthorized = await fetch(base + "/tool", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ name: "browser_tabs_list", arguments: {} }),
  });
  expect(unauthorized.status).toBe(401);
  const forged = await fetch(base + "/tunnel/open", {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      Authorization: "Bearer " + endpoint.token,
    },
    body: JSON.stringify({
      lease: crypto.randomUUID(),
      host: "127.0.0.1",
      port: 80,
    }),
  });
  expect(forged.ok).toBe(false);
  expect(await forged.text()).toContain("disconnected or replaced");
});

test("CPU, trace, heap and Lighthouse produce usable server artifacts", async () => {
  test.setTimeout(240000);
  await tool("cpu_start", { tabID });
  await tool("evaluate", {
    tabID,
    script:
      "(()=>{let n=0;for(let i=0;i<200000;i++)n+=Math.sqrt(i);return n})()",
  });
  const cpu = (await tool("cpu_stop", { tabID })).value;
  expect(cpu.files.length).toBe(1);
  expect(
    (await tool("cpu_analyze", { tabID, fileID: cpu.files[0].id })).value
      .functions,
  ).toBeDefined();
  await tool("trace_start", { tabID, durationMs: 1000 });
  await tool("evaluate", {
    tabID,
    script: "document.body.getBoundingClientRect().width",
  });
  const trace = (await tool("trace_stop", { tabID })).value;
  expect(trace.files.length).toBe(1);
  expect(
    (await tool("trace_analyze", { tabID, fileID: trace.files[0].id })).value
      .metrics.length,
  ).toBeGreaterThan(0);
  const heap = (await tool("heap_snapshot", { tabID })).value;
  const summary = (
    await tool("heap_summary", { tabID, fileID: heap.files[0].id })
  ).value;
  expect(summary.nodes).toBeGreaterThan(0);
  const nodes = (
    await tool("heap_query", { tabID, fileID: heap.files[0].id, limit: 3 })
  ).value.nodes;
  if (nodes.length)
    expect(
      (
        await tool("heap_object", {
          tabID,
          fileID: heap.files[0].id,
          id: nodes[0].id,
        })
      ).value.node.id,
    ).toBe(nodes[0].id);
  const audit = (await tool("lighthouse", { tabID })).value;
  expect(
    audit.scores.some((score: any) => score.id === "accessibility"),
  ).toBeTruthy();
  expect(audit.files).toHaveLength(2);
});
test("conversation isolation, restore, close and blocked schemes are enforced", async () => {
  const snapshot = await inspect();
  await select(second);
  expect((await tool("tabs_list", {}, second)).value.tabs).toEqual([]);
  await expect(tool("snapshot", { tabID }, second)).rejects.toThrow(
    /belong|closed/i,
  );
  await select(thread);
  expect(
    (await tool("tabs_list")).value.tabs.some((tab: any) => tab.id === tabID),
  ).toBeTruthy();
  await page.reload();
  await expect
    .poll(async () => {
      try {
        return attachedTabs((await tool("tabs_list")).value);
      } catch {
        return 0;
      }
    })
    .toBe(1);
  await tool("navigate", { tabID, url: siteURL + "/popup" });
  await expect(
    tool("click", { tabID, ref: ref(snapshot, "Submit order") }),
  ).rejects.toThrow(/ref|snapshot|element/i);
  await expect(
    tool("navigate", { tabID, url: "file:///etc/passwd" }),
  ).rejects.toThrow(/http|URL|scheme|navigate/i);
  const extra = (await tool("tabs_open", { url: siteURL + "/popup" })).value;
  await tool("tabs_close", { tabID: extra.id });
  await expect(tool("snapshot", { tabID: extra.id })).rejects.toThrow(
    /belong|closed/i,
  );
  expect(errors).toEqual([]);
});
test("remote workspace browser uses the routed authenticated server for files and networking", async () => {
  const remote = await page.evaluate(
    (input) => window.haarness.connect(input),
    {
      url: endpoint.url,
      token: endpoint.token,
      workspace: directories[1],
      label: "Browser remote server",
    },
  );
  expect(remote?.remote).toBe(true);
  const base = endpoint.url + "/v1/host/workspaces/" + remote!.remoteWorkspace;
  const remoteClient = new Haarness({ baseUrl: base, token: endpoint.token });
  const target = await remoteClient.createThread({
    title: "Remote browser",
    provider: "echo",
    model: "echo-1",
  });
  await page.evaluate(
    async ({ connection, thread }) => {
      const recent = (await window.haarness.boot()).recent.find(
        (item) => item.id === connection,
      );
      const server = recent?.remote
        ? "remote:" + new URL(recent.url!).origin
        : "local";
      await window.haarness.storage("active-server", server);
      await window.haarness.storage("server-project:" + server, connection);
      await window.haarness.storage("active-project", connection);
      await window.haarness.storage("session:" + connection, thread);
    },
    { connection: remote!.id, thread: target.id },
  );
  await page.reload();
  await expect
    .poll(async () => {
      try {
        return attachedTabs(
          (await tool("tabs_list", {}, target, base)).value,
        );
      } catch {
        return -1;
      }
    })
    .toBe(0);
  const remoteTab = (await tool("tabs_open", { url: siteURL }, target, base))
    .value.id;
  const snapshot = (await tool("snapshot", { tabID: remoteTab }, target, base))
    .value.content;
  await writeFile(join(directories[1], "upload.txt"), "REMOTE SERVER BYTES");
  await tool(
    "files_upload",
    {
      tabID: remoteTab,
      ref: ref(snapshot, "Upload document"),
      paths: ["upload.txt"],
    },
    target,
    base,
  );
  await tool(
    "wait",
    { tabID: remoteTab, condition: "text", text: "REMOTE SERVER BYTES" },
    target,
    base,
  );
  await tool("tabs_focus", { tabID: remoteTab }, target, base);
  const shot = (await tool("screenshot", { tabID: remoteTab }, target, base))
    .value;
  expect(shot.files[0].path.startsWith(directories[1])).toBeTruthy();
  await expect(tool("snapshot", { tabID }, target, base)).rejects.toThrow(
    /belong|closed/i,
  );
  remoteClient.close();
});

test("real model operates the browser and receives a screenshot attachment", async () => {
  test.skip(
    process.env.HAARNESS_BROWSER_LIVE !== "1",
    "Opt-in live model verification",
  );
  test.setTimeout(300000);
  const live = await client.createThread({
    title: "Live browser proof",
    provider: "openai",
    model: "gpt-5.6-luna",
  });
  await page.evaluate(
    async ({ connection, thread }) => {
      const recent = (await window.haarness.boot()).recent.find(
        (item) => item.id === connection,
      );
      const server = recent?.remote
        ? "remote:" + new URL(recent.url!).origin
        : "local";
      await window.haarness.storage("active-server", server);
      await window.haarness.storage("server-project:" + server, connection);
      await window.haarness.storage("active-project", connection);
      await window.haarness.storage("session:" + connection, thread);
    },
    { connection, thread: live.id },
  );
  await page.reload();
  await expect
    .poll(async () => {
      try {
        return attachedTabs((await tool("tabs_list", {}, live)).value);
      } catch {
        return -1;
      }
    })
    .toBe(0);
  const marker = "browser-proof-" + crypto.randomUUID().slice(0, 8);
  const prompt = `Use the native browser tools to open ${siteURL}. Take an accessibility snapshot, fill the Name field with ${marker}, click Submit order, and verify the saved text. Focus that browser tab and take a screenshot using browser_screenshot. Then describe the background color you see in that image and include the saved text. Use browser tools for all of this, no shell, fetch, or filesystem tools. Do not ask me questions.`;
  const tui = "haarness-browser-" + crypto.randomUUID().slice(0, 8);
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
        live.id,
      ],
      { env },
    );
    await expect
      .poll(
        () =>
          execFileSync("tmux", ["capture-pane", "-p", "-t", tui], {
            encoding: "utf8",
          }),
        { timeout: 30000 },
      )
      .toContain("Live browser proof");
    execFileSync("tmux", ["send-keys", "-t", tui, "-l", prompt]);
    execFileSync("tmux", ["send-keys", "-t", tui, "Enter"]);
    await expect
      .poll(
        async () => {
          const items = await client.listItems(live.id);
          const turn = items.find(
            (item) => item.turnId && item.data.type === "user_message",
          );
          return turn?.turnId
            ? (await client.getTurn(turn.turnId)).status
            : undefined;
        },
        { timeout: 240000, intervals: [500, 1000] },
      )
      .toBe("completed");
    if (evidence)
      await writeFile(
        join(evidence, "real-model-tui.txt"),
        execFileSync("tmux", ["capture-pane", "-p", "-S", "-500", "-t", tui], {
          encoding: "utf8",
        }),
      );
  } finally {
    try {
      execFileSync("tmux", ["kill-session", "-t", tui]);
    } catch {}
  }
  const items = await client.listItems(live.id);
  const calls = items
    .filter((item) => item.data.type === "tool_call")
    .map((item) => (item.data as any).name);
  for (const name of [
    "browser_tabs_open",
    "browser_snapshot",
    "browser_click",
    "browser_screenshot",
  ])
    expect(calls).toContain(name);
  expect(
    calls.some(
      (name) => name === "browser_fill" || name === "browser_fill_form",
    ),
  ).toBe(true);
  const screenshots = items.filter(
    (item) =>
      item.data.type === "tool_result" &&
      (item.data as any).metadata?.attachments?.length,
  );
  expect(screenshots.length).toBeGreaterThan(0);
  const final = items
    .filter((item) => item.data.type === "assistant_message")
    .map((item) => (item.data as any).text)
    .join("\n");
  expect(final).toContain(marker);
  if (evidence)
    await writeFile(
      join(evidence, "real-model-items.json"),
      JSON.stringify(items, null, 2),
    );
});

test("browser permissions bind the target across an approval and survive configuration reload", async () => {
  const approval = await client.createThread({
    title: "Browser approval race",
    provider: "echo",
    model: "echo-1",
  });
  await page.evaluate(
    async ({ connection, thread }) => {
      const recent = (await window.haarness.boot()).recent.find(
        (item) => item.id === connection,
      );
      const server = recent?.remote
        ? "remote:" + new URL(recent.url!).origin
        : "local";
      await window.haarness.storage("active-server", server);
      await window.haarness.storage("server-project:" + server, connection);
      await window.haarness.storage("active-project", connection);
      await window.haarness.storage("session:" + connection, thread);
    },
    { connection, thread: approval.id },
  );
  await page.reload();
  await expect
    .poll(async () => {
      try {
        return attachedTabs((await tool("tabs_list", {}, approval)).value);
      } catch {
        return -1;
      }
    })
    .toBe(0);
  const id = (await tool("tabs_open", { url: siteURL }, approval)).value.id;
  const snapshot = (await tool("snapshot", { tabID: id }, approval)).value
    .content;
  const config = join(directories[0], ".haarness/config.toml");
  const original = await readFile(config, "utf8");
  await writeFile(
    config,
    original +
      '\n[permissions.browser]\n"click:*"="ask"\n"evaluate:*"="deny"\n',
  );
  // The configuration endpoint reports the applied state; a denied evaluation proves
  // this is the new engine while ordinary browser tools retain their original host.
  await expect
    .poll(
      async () => {
        try {
          await tool("evaluate", { tabID: id, script: "1" }, approval);
          return false;
        } catch (error) {
          return String(error).includes("denied");
        }
      },
      { timeout: 30000 },
    )
    .toBe(true);
  const pending = tool(
    "click",
    { tabID: id, ref: ref(snapshot, "Submit order") },
    approval,
  ).then(
    (value) => ({ value, error: "" }),
    (error) => ({ error: String(error) }),
  );
  await expect
    .poll(
      async () =>
        (await client.listPermissions()).find((p) => p.threadId === approval.id)
          ?.permissionId,
    )
    .toBeTruthy();
  const permission = (await client.listPermissions()).find(
    (p) => p.threadId === approval.id,
  )!;
  expect(permission.operation).toContain(siteURL);
  // This is an explicit user navigation while the model's click is pending.
  await page.evaluate(
    ({ connection, thread, id, url }) =>
      window.haarness.browserCommand(connection, thread, {
        type: "navigate",
        tabID: id,
        url,
      }),
    { connection, thread: approval.id, id, url: siteURL + "/popup" },
  );
  await client.replyPermission(permission.permissionId, { decision: "once" });
  expect((await pending).error).toMatch(/changed|ref|snapshot|element/i);
  await writeFile(config, original);
});
test("application restart restores tab URLs, invalidates old refs and reconnects its browser host", async () => {
  await page.evaluate(
    async ({ connection, thread }) => {
      const recent = (await window.haarness.boot()).recent.find(
        (item) => item.id === connection,
      );
      const server = recent?.remote
        ? "remote:" + new URL(recent.url!).origin
        : "local";
      await window.haarness.storage("active-server", server);
      await window.haarness.storage("server-project:" + server, connection);
      await window.haarness.storage("active-project", connection);
      await window.haarness.storage("session:" + connection, thread);
    },
    { connection, thread: thread.id },
  );
  await page.reload();
  if (!(await tool("tabs_list")).value.tabs.length)
    await tool("tabs_open", { url: siteURL });
  const before = (await tool("tabs_list")).value;
  const focused = before.tabs[0].id;
  await tool("tabs_focus", { tabID: focused });
  await app.close();
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
  await expect
    .poll(async () => {
      try {
        return attachedTabs((await tool("tabs_list")).value);
      } catch {
        return -1;
      }
    })
    .toBe(before.tabs.length);
  const after = (await tool("tabs_list")).value;
  expect(after.tabs.map((tab: any) => tab.url)).toEqual(
    before.tabs.map((tab: any) => tab.url),
  );
  expect(after.tabs[0].generation).toBeGreaterThan(before.tabs[0].generation);
  await tool("snapshot", { tabID: focused });
  expect(errors).toEqual([]);
});

test("server restart cancels pending browser work and reconnects the same conversation", async () => {
  test.setTimeout(180000);
  await select(thread);
  const id = (await tool("tabs_open", { url: siteURL + "/popup" })).value.id;
  const before = (await tool("tabs_list")).value.tabs.find(
    (tab: any) => tab.id === id,
  );
  const original = await client.info();
  const marker = "disconnect-wait-" + crypto.randomUUID();
  const pending = tool("wait", {
    tabID: id,
    condition: "text",
    text: marker,
    timeoutMs: 30000,
  }).then(
    () => "unexpected success",
    (error) => String(error),
  );
  await expect
    .poll(async () =>
      (await client.listItems(thread.id)).some(
        (item) =>
          item.data.type === "tool_call" &&
          (item.data as any).arguments?.text === marker,
      ),
    )
    .toBe(true);
  await client.shutdownServer();
  expect(await pending).toMatch(
    /disconnect|cancel|closed|abort|fetch|network|shutdown|stopping/i,
  );
  await expect
    .poll(
      async () => {
        try {
          endpoint = JSON.parse(
            execFileSync(
              binary,
              ["--workspace", directories[0], "--trust-project", "desktop"],
              {
                env,
                input: '{"action":"connect"}',
                encoding: "utf8",
                timeout: 30000,
              },
            ),
          );
          client.close();
          client = new Haarness({
            baseUrl: endpoint.url,
            token: endpoint.token,
          });
          return (
            (await client.info()).serverInstanceId !== original.serverInstanceId
          );
        } catch {
          return false;
        }
      },
      { timeout: 60000 },
    )
    .toBe(true);
  await expect
    .poll(
      async () => {
        try {
          return (await tool("tabs_list")).value.tabs.find(
            (tab: any) => tab.id === id,
          )?.generation;
        } catch {
          return 0;
        }
      },
      { timeout: 60000 },
    )
    .toBeGreaterThan(before.generation);
  expect((await tool("snapshot", { tabID: id })).value.content).toContain(
    "Popup isolated",
  );
  const replayed = (await client.listItems(thread.id)).filter(
    (item) =>
      item.data.type === "tool_call" &&
      (item.data as any).arguments?.text === marker,
  );
  expect(replayed).toHaveLength(1);
});

test("confined code mode invokes browser tools through the normal persisted turn", async () => {
  await select(thread);
  const before = (await tool("tabs_list")).value.tabs.length;
  const code = `const tab = await tool("browser_tabs_open", {url:${JSON.stringify(siteURL + "/popup")}}); const snapshot = await tool("browser_snapshot", {tabID:JSON.parse(tab.text).id}); return JSON.parse(snapshot.text).content;`;
  const turn = await client.startTurn(thread.id, {
    requestId: crypto.randomUUID(),
    text: "!tool execute " + JSON.stringify({ code }),
  });
  await client.waitTurn(turn.id);
  const items = (await client.listItems(thread.id)).filter(
    (item) => item.turnId === turn.id,
  );
  const calls = items
    .filter((item) => item.data.type === "tool_call")
    .map((item) => (item.data as any).name);
  // Code mode intentionally persists the parent call and its returned result.
  expect(calls).toEqual(["execute"]);
  expect((await tool("tabs_list")).value.tabs.length).toBe(before + 1);
  const results = items
    .filter((item) => item.data.type === "tool_result")
    .map((item) => item.data as any);
  expect(results.every((item) => !(item.is_error ?? item.isError))).toBe(true);
  expect(results.some((item) => item.output.includes("Popup isolated"))).toBe(
    true,
  );
});

test("shallow snapshots show nested form controls and values, redact passwords, and clicks return observations", async () => {
  await select(thread);
  const id = (await tool("tabs_open", { url: siteURL })).value.id;
  await tool("evaluate", {
    tabID: id,
    script: `(()=>{const input=document.getElementById('name');input.value='Visible field';let node=input.parentElement;for(let i=0;i<12;i++){const wrapper=document.createElement('div');node.replaceWith(wrapper);wrapper.appendChild(node);node=wrapper}const label=document.createElement('label');label.textContent='Password field';const secret=document.createElement('input');secret.type='password';secret.value='must-not-leak-529';label.appendChild(secret);document.getElementById('form').appendChild(label);const dateLabel=document.createElement('label');dateLabel.textContent='Reservation date';const date=document.createElement('input');date.type='date';date.id='reservationDate';date.value='2026-10-12';dateLabel.appendChild(date);document.getElementById('form').appendChild(dateLabel);})()`,
  });
  const snapshot = (await tool("snapshot", { tabID: id, depth: 2 })).value
    .content;
  expect(snapshot).toContain('value="Visible field"');
  expect(snapshot).toContain("Password field");
  expect(snapshot).toContain("[redacted]");
  expect(snapshot).not.toContain("must-not-leak-529");
  expect(snapshot).toContain('format="YYYY-MM-DD" value="2026-10-12"');
  expect(snapshot).not.toContain("[spinbutton]");
  await tool("fill", {
    tabID: id,
    ref: ref(snapshot, "Reservation date"),
    text: "2026-10-13",
  });
  expect(
    (
      await tool("evaluate", {
        tabID: id,
        script: "document.getElementById('reservationDate').value",
      })
    ).value.value,
  ).toBe("2026-10-13");
  const click = (
    await tool("click", { tabID: id, ref: ref(snapshot, "Submit order") })
  ).value;
  expect(click.dispatched).toBe(true);
  expect(click.observation.text).toContain("Saved Visible field");
  await tool("evaluate", {
    tabID: id,
    script: `(()=>{const region=document.createElement('div');region.setAttribute('role','region');region.setAttribute('aria-label','Drop zone');region.textContent='Drop here';document.body.appendChild(region);const button=document.createElement('button');button.textContent='Confirm action';button.onclick=()=>{window.dialogDecision=confirm('Proceed with action?')};document.body.appendChild(button)})()`,
  });
  const updated = (await tool("snapshot", { tabID: id, depth: 3 })).value
    .content;
  expect(ref(updated, "Drop zone")).toBeTruthy();
  const confirmation = (
    await tool("click", { tabID: id, ref: ref(updated, "Confirm action") })
  ).value;
  expect(confirmation.dispatched).toBe(true);
  expect(confirmation.dialog.message).toBe("Proceed with action?");
  expect(
    (await tool("dialog", { tabID: id, action: "get" })).value.dialog.message,
  ).toBe("Proceed with action?");
  await tool("dialog", { tabID: id, action: "dismiss" });
  expect(
    (await tool("evaluate", { tabID: id, script: "window.dialogDecision" }))
      .value.value,
  ).toBe(false);
  await tool("click", { tabID: id, ref: ref(updated, "Confirm action") });
  await tool("dialog", { tabID: id, action: "accept" });
  expect(
    (await tool("evaluate", { tabID: id, script: "window.dialogDecision" }))
      .value.value,
  ).toBe(true);
  await tool("tabs_close", { tabID: id });
});
