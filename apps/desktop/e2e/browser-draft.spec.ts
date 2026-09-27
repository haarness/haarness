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
  connection: string;
const errors: string[] = [];
const directories: string[] = [];
const evidence = process.env.HAARNESS_BROWSER_EVIDENCE;
const binary = resolve(
  process.env.HAARNESS_BIN ?? "../../target/debug/haarness",
);
test.describe.configure({ mode: "serial" });
const html = `<!doctype html><html lang="en"><head><title>Browser fixture</title><meta name="description" content="Haarness browser integration fixture"><meta name="viewport" content="width=device-width"></head><body style="font:18px system-ui;background:#edf2ff;color:#14213d;padding:25px"><h1>Haarness browser fixture</h1><form id="form"><label>Name <input id="name" name="name"></label><label>Tier <select id="tier"><option value="one">One</option><option value="two">Two</option></select></label><label><input type="checkbox" id="agree"> Agree</label><button>Submit order</button></form><p id="result" role="status">Ready</p><label>Upload document<input type="file" id="upload"></label><p id="uploaded"></p><a href="/download" download="fixture.txt">Download fixture</a><button onclick="window.open('/popup')">Open popup</button><iframe title="Child form" src="/frame"></iframe><p id="socket">Connecting socket</p><script>form.onsubmit=e=>{e.preventDefault();result.textContent='Saved '+nameInput.value+' '+tier.value+' '+agree.checked;fetch('/api',{method:'POST',body:nameInput.value})};const nameInput=document.getElementById('name');upload.onchange=async()=>uploaded.textContent=await upload.files[0].text();console.log('fixture console ready');const socket=new WebSocket(location.origin.replace('http','ws')+'/ws');socket.onmessage=e=>document.getElementById('socket').textContent=e.data;</script></body></html>`;
async function tool(
  name: string,
  args: object = {},
  target: Thread,
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
test.beforeAll(async () => {
  test.setTimeout(240000);
  temporary = await realpath(
    await mkdtemp(join(tmpdir(), "haarness-browser-draft-")),
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
const panel = () =>
  page.getByRole("region", { name: "Web browser", exact: true });
async function showBrowser() {
  await app.evaluate(({ BrowserWindow }) =>
    BrowserWindow.getAllWindows()[0].webContents.send("haarness:event", {
      kind: "menu",
      data: "browser",
    }),
  );
  await expect(panel()).toBeVisible();
}
async function native<T>(script: string, url = siteURL): Promise<T> {
  return app.evaluate(
    async ({ webContents }, { script, url }) => {
      const contents = webContents
        .getAllWebContents()
        .find((contents) => contents.getURL() === url);
      if (!contents) throw new Error("Fixture page was not loaded");
      return contents.executeJavaScript(script);
    },
    { script, url },
  );
}
async function inventory(id = connection, owner = "") {
  return page.evaluate(
    ({ id, owner }) => window.haarness.browserAttach(id, owner),
    { id, owner },
  );
}
async function activate(id: string) {
  await page.evaluate(async (id) => {
    const recent = (await window.haarness.boot()).recent.find(
      (entry) => entry.id === id,
    )!;
    const server =
      recent.remote && recent.url
        ? "remote:" + new URL(recent.url).origin
        : "local";
    await window.haarness.storage("active-server", server);
    await window.haarness.storage("server-project:" + server, id);
    await window.haarness.storage("active-project", id);
    await window.haarness.storage("session:" + id, "");
  }, id);
  await page.reload();
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
  );
  await expect
    .poll(() => page.evaluate(() => window.haarness.storage("active-project")))
    .toBe(id);
  await showBrowser();
}
let originalTab: string;
let remoteId: string;
let transferred: Thread;
test("new-chat screen opens and navigates real browser tabs without creating a conversation", async () => {
  expect(await client.listThreads()).toHaveLength(0);
  await showBrowser();
  await expect(
    panel().getByRole("button", { name: "Go", exact: true }),
  ).toBeEnabled();
  await panel()
    .getByRole("button", { name: "New browser tab", exact: true })
    .click();
  await expect(panel().getByRole("tab")).toHaveCount(1);
  const address = panel().getByRole("textbox", { name: "Browser address" });
  await address.fill(siteURL.replace("http://", ""));
  await inventory(); // A host update must not replace the URL being typed.
  await expect(address).toHaveValue(siteURL.replace("http://", ""));
  await address.press("Enter");
  await expect
    .poll(async () => (await inventory()).tabs[0]?.url)
    .toBe(siteURL + "/");
  originalTab = (await inventory()).tabs[0].id;
  await native(
    'document.getElementById("name").value="before-chat 日本";document.cookie="draft_login=kept;path=/";localStorage.setItem("draft","kept");window.draftProof="same page"',
    siteURL + "/",
  );
  await native('document.querySelector("a[download]").click()', siteURL + "/");
  await expect
    .poll(
      async () =>
        (
          await page.evaluate(
            (id) => window.haarness.browserData(id),
            connection,
          )
        ).downloads.length,
    )
    .toBe(1);
  await panel()
    .getByRole("button", { name: "New browser tab", exact: true })
    .click();
  await address.fill(siteURL + "/second");
  await panel().getByRole("button", { name: "Go", exact: true }).click();
  await expect.poll(async () => (await inventory()).tabs.length).toBe(2);
  await expect
    .poll(
      async () =>
        (await inventory()).tabs.find((tab) => tab.url === siteURL + "/second")
          ?.loading,
    )
    .toBe(false);
  await expect
    .poll(() =>
      app.evaluate(({ BrowserWindow }, url) => {
        return BrowserWindow.getAllWindows()[0].contentView.children.some(
          (view) => {
            const contents = (view as any).webContents;
            const bounds = view.getBounds();
            return (
              contents?.getURL() === url &&
              view.getVisible() &&
              bounds.width > 100 &&
              bounds.height > 100
            );
          },
        );
      }, siteURL + "/second"),
    )
    .toBe(true);
  expect(await client.listThreads()).toHaveLength(0);
  await page.screenshot({
    path: evidence
      ? join(evidence, "browser-before-chat.png")
      : test.info().outputPath("browser-before-chat.png"),
  });
  if (evidence) {
    const png = await app.evaluate(async ({ webContents }, url) => {
      const contents = webContents
        .getAllWebContents()
        .find((contents) => contents.getURL() === url)!;
      return (await contents.capturePage()).toPNG().toString("base64");
    }, siteURL + "/second");
    await writeFile(
      join(evidence, "browser-page-before-chat.png"),
      Buffer.from(png, "base64"),
    );
  }
});
test("draft browser survives renderer reload and stays isolated between local and remote projects", async () => {
  await page.reload();
  await showBrowser();
  expect((await inventory()).tabs.map((tab) => tab.id)).toContain(originalTab);
  expect(await native("window.draftProof", siteURL + "/")).toBe("same page");
  const remote = await page.evaluate(
    (input) => window.haarness.connect(input),
    {
      url: endpoint.url,
      token: endpoint.token,
      workspace: directories[1],
      label: "Draft remote fixture",
    },
  );
  remoteId = remote!.id;
  await activate(remoteId);
  expect((await inventory(remoteId)).tabs).toHaveLength(0);
  await panel()
    .getByRole("textbox", { name: "Browser address" })
    .fill(siteURL + "/remote");
  await panel().getByRole("button", { name: "Go", exact: true }).click();
  await expect
    .poll(async () => (await inventory(remoteId)).tabs[0]?.url)
    .toBe(siteURL + "/remote");
  expect(await native("document.cookie", siteURL + "/remote")).not.toContain(
    "draft_login",
  );
  expect(
    await page.evaluate(
      (id) => window.haarness.api(id, "listThreads", []),
      remoteId,
    ),
  ).toHaveLength(0);
  await activate(connection);
  expect((await inventory()).tabs).toHaveLength(2);
  expect(await native("window.draftProof", siteURL + "/")).toBe("same page");
});
test("first chat adopts live pages, form input, cookies, downloads and working network into agent tools", async () => {
  await page
    .getByRole("textbox", { name: "Message", exact: true })
    .fill("!tool browser_tabs_list {}");
  await page.getByRole("button", { name: "Send message", exact: true }).click();
  await expect.poll(async () => (await client.listThreads()).length).toBe(1);
  transferred = (await client.listThreads())[0];
  await expect(page.locator(".transcript")).toContainText(originalTab);
  expect(
    (await inventory(connection, transferred.id)).tabs.map((tab) => tab.id),
  ).toContain(originalTab);
  const result = (
    await tool(
      "evaluate",
      {
        tabID: originalTab,
        script:
          '({proof:window.draftProof,name:document.getElementById("name").value,cookie:document.cookie,storage:localStorage.getItem("draft")})',
      },
      transferred,
    )
  ).value.value;
  expect(result).toEqual({
    proof: "same page",
    name: "before-chat 日本",
    cookie: "draft_login=kept",
    storage: "kept",
  });
  expect(
    (
      await tool(
        "evaluate",
        {
          tabID: originalTab,
          script: 'fetch("/api").then(response=>response.json())',
        },
        transferred,
      )
    ).value.value,
  ).toEqual({ saved: true });
  const files = (await tool("files_list", { tabID: originalTab }, transferred))
    .value;
  const list = Array.isArray(files) ? files : files.files;
  expect(list.length).toBeGreaterThan(0);
  const file = await tool(
    "files_get",
    { tabID: originalTab, fileID: list[0].id },
    transferred,
  );
  expect(await readFile(file.value.files[0].path, "utf8")).toBe(
    "Downloaded from fixture",
  );
  const items = await client.listItems(transferred.id);
  expect(JSON.stringify(items)).toContain("browser_tabs_list");
  if (evidence)
    await writeFile(
      join(evidence, "first-chat-items.json"),
      JSON.stringify(items, null, 2),
    );
  expect((await inventory()).tabs).toHaveLength(0);
});
test("a real model reads the page carried into its new conversation", async () => {
  test.skip(
    process.env.HAARNESS_BROWSER_LIVE !== "1",
    "Set HAARNESS_BROWSER_LIVE=1 for real-model proof",
  );
  test.setTimeout(180000);
  await client.updateThread(transferred.id, {
    provider: "openai",
    model: process.env.HAARNESS_BROWSER_MODEL ?? "gpt-5.6-luna",
  });
  // This new value is deliberately absent from prior tool results and prompts,
  // so the model must observe the live page instead of answering from history.
  const marker = "live-draft-" + crypto.randomUUID().slice(0, 8);
  await native(`document.getElementById("name").value=${JSON.stringify(marker)}`, siteURL + "/");
  const turn = await client.startTurn(transferred.id, {
    text: "I just changed the Name field manually. Inspect the existing browser tabs. In the tab at the root URL, read the current Name field with a fresh browser observation and tell me its exact value. Keep the existing page; do not open or navigate any tab, edit anything, use shell or fetch, or ask me questions.",
    requestId: crypto.randomUUID(),
  });
  expect(
    (
      await client.waitTurn(
        turn.id,
        { timeout_ms: 120000 },
        { timeoutMs: 125000 },
      )
    ).status,
  ).toBe("completed");
  const items = (await client.listItems(transferred.id)).filter(
    (item) => item.turnId === turn.id,
  );
  const records = JSON.stringify(items);
  expect(records).toContain(marker);
  expect(JSON.stringify(items.filter(item => item.data.type === "tool_result"))).toContain(marker);
  expect((await inventory(connection, transferred.id)).tabs).toHaveLength(2);
  expect(await native("window.draftProof", siteURL + "/")).toBe("same page");
  if (evidence)
    await writeFile(
      join(evidence, "real-model-adopted-browser.json"),
      JSON.stringify(items, null, 2),
    );
});
test("another new chat gets its own tabs and restart restores its URLs without a phantom conversation", async () => {
  await page.getByRole("button", { name: "New session", exact: true }).click();
  await showBrowser();
  expect((await inventory()).tabs).toHaveLength(0);
  await panel()
    .getByRole("textbox", { name: "Browser address" })
    .fill(siteURL + "/restart-draft");
  await panel().getByRole("button", { name: "Go", exact: true }).click();
  await expect
    .poll(async () => (await inventory()).tabs[0]?.url)
    .toBe(siteURL + "/restart-draft");
  await app.close();
  app = await electron.launch({ args: ["."], env, timeout: 120000 });
  app.context().on("dialog", () => {});
  page = await app.firstWindow();
  page.on("pageerror", (error) => errors.push(error.message));
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
  );
  await showBrowser();
  await expect
    .poll(async () => (await inventory()).tabs[0]?.url)
    .toBe(siteURL + "/restart-draft");
  expect(await client.listThreads()).toHaveLength(1);
  expect(
    (await inventory(connection, transferred.id)).tabs.map((tab) => tab.id),
  ).toContain(originalTab);
  expect((await inventory()).tabs.map((tab) => tab.id)).not.toContain(
    originalTab,
  );
  expect(errors).toEqual([]);
});
test("draft browser endpoints require authentication and a matching lease and project", async () => {
  const draft = (await page.evaluate(
    (id) => window.haarness.storage("browser-draft:" + id),
    connection,
  )) as string;
  const path = endpoint.url + "/v1/browser/drafts/" + draft;
  const request = {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ thread: transferred.id, lease: "wrong" }),
  };
  expect((await fetch(path + "/adopt", request)).status).toBe(401);
  expect(
    (
      await fetch(path + "/adopt", {
        ...request,
        headers: {
          ...request.headers,
          Authorization: "Bearer " + endpoint.token,
        },
      })
    ).ok,
  ).toBe(false);
  const result = await fetch(path + "/tunnel/open", {
    ...request,
    headers: { ...request.headers, Authorization: "Bearer " + endpoint.token },
    body: JSON.stringify({
      lease: "wrong",
      target: { host: "127.0.0.1", port: 80 },
    }),
  });
  expect(result.ok).toBe(false);
});
test("manual browsing works with agent control disabled and clearing data includes new-chat tabs", async () => {
  const before = (
    await client.managementView("browser-settings", {
      action: "get",
      scope: "project",
    } as any)
  ).data as any;
  await client.managementView("browser-settings", {
    action: "set",
    scope: "project",
    revision: before.revision,
    policy: { ...before.policy, enabled: false },
  } as any);
  await panel()
    .getByRole("textbox", { name: "Browser address" })
    .fill(siteURL + "/disabled-manual");
  await panel().getByRole("button", { name: "Go", exact: true }).click();
  await expect
    .poll(async () => (await inventory()).tabs[0]?.url)
    .toBe(siteURL + "/disabled-manual");
  expect(await client.listThreads()).toHaveLength(1);
  await page.evaluate(
    (id) => window.haarness.browserData(id, { action: "clear", kind: "all" }),
    connection,
  );
  expect((await inventory()).tabs).toHaveLength(0);
  const records = await page.evaluate(
    (id) => window.haarness.browserData(id),
    connection,
  );
  expect(records).toEqual({ history: [], downloads: [] });
  const fresh = (
    await client.managementView("browser-settings", {
      action: "get",
      scope: "project",
    } as any)
  ).data as any;
  await client.managementView("browser-settings", {
    action: "set",
    scope: "project",
    revision: fresh.revision,
    policy: before.policy,
  } as any);
  expect(errors).toEqual([]);
});
