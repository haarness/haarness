import {
  test,
  expect,
  _electron as electron,
  type ElectronApplication,
  type Page,
} from "@playwright/test";
import { attachFiles } from "./fixtures/composer";
import { Haarness } from "@haarness/sdk";
import {
  mkdtemp,
  mkdir,
  writeFile,
  readFile,
  rm,
  realpath,
  chmod,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";
import {
  createServer,
  request,
  type IncomingMessage,
  type ServerResponse,
} from "node:http";

const binary = resolve(
  process.env.HAARNESS_BIN ?? "../../target/debug/haarness",
);
let app: ElectronApplication;
let page: Page;
let temporary: string;
let env: Record<string, string>;
let workspaces: string[];
const errors: string[] = [];

test("remote profile form has no outer border and keeps pointer focus quiet", async () => {
  const remote = await endpoint("profile-form");
  await launch();
  await addServer("Profile server", remote.url, remote.token);
  const pane = await settings("Profiles");
  await pane.getByRole("button", { name: "Save profile", exact: true }).click();
  const fieldset = pane.locator(".management-panel fieldset");
  await expect(fieldset).toHaveCSS("border-top-width", "0px");
  const name = pane.getByRole("textbox", { name: "Name", exact: true });
  await expect(name).toHaveAttribute("type", "text");
  await name.fill("Profile layout proof");
  const scope = pane.getByRole("combobox", { name: "Scope", exact: true });
  await scope.click();
  await page.getByRole("option", { name: "both", exact: true }).click();
  await expect(scope).toHaveCSS("box-shadow", "none");
  await expect(scope).toHaveCSS("outline-style", "none");
  await expect(name).toHaveValue("Profile layout proof");
  await page.screenshot({
    path: test.info().outputPath("profile-form-dark.png"),
  });
  await page.evaluate(() => window.haarness.preferences({ theme: "light" }));
  await expect(fieldset).toHaveCSS("border-top-width", "0px");
  await page.screenshot({
    path: test.info().outputPath("profile-form-light.png"),
  });
  await pane.getByRole("button", { name: "Save", exact: true }).click();
  await expect(
    pane.getByRole("button", { name: "Profile layout proof", exact: true }),
  ).toBeVisible();
});

test("remote opening retries startup failures without probing unrelated saved projects", async () => {
  const bin = join(temporary, "bin");
  const unrelated = join(temporary, "unrelated");
  const probes = join(temporary, "unrelated-git-probes");
  await mkdir(bin);
  await mkdir(unrelated);
  const git = execFileSync("which", ["git"], { encoding: "utf8" }).trim();
  await writeFile(
    join(bin, "git"),
    `#!/usr/bin/env node
const { appendFileSync } = require('node:fs');
const { spawnSync } = require('node:child_process');
if (process.cwd() === ${JSON.stringify(unrelated)}) appendFileSync(${JSON.stringify(probes)}, 'probe\\n');
const result = spawnSync(${JSON.stringify(git)}, process.argv.slice(2), { stdio: 'inherit' });
process.exit(result.status ?? 1);
`,
  );
  await chmod(join(bin, "git"), 0o755);
  env.PATH = bin + ":" + env.PATH;
  const remote = await endpoint("performance-host");
  const project = join(remote.path, "cold-project");
  await mkdir(project);
  workspaces.push(project);
  const registryPath = join(env.HAARNESS_DATA_DIR, "data/workspaces.json");
  const registry = JSON.parse(await readFile(registryPath, "utf8"));
  for (let index = 0; index < 60; index++)
    registry.workspaces.push({
      ...registry.workspaces[0],
      id: "unrelated-" + index,
      name: "Unrelated " + index,
      path: unrelated,
    });
  await writeFile(registryPath, JSON.stringify(registry));
  let failOpen = true;
  const proxy = createServer((req, res) => {
    if (req.url === "/v1/host/open" && failOpen) {
      failOpen = false;
      req.resume();
      res.writeHead(503, { "Content-Type": "application/json" });
      res.end(
        JSON.stringify({
          error: {
            message: "Haarness server did not become ready within 8 seconds",
          },
        }),
      );
      return;
    }
    const upstream = request(
      new URL(req.url!, remote.url),
      { method: req.method, headers: req.headers },
      (incoming) => {
        res.writeHead(incoming.statusCode!, incoming.headers);
        incoming.pipe(res);
      },
    );
    upstream.on("error", () => {
      if (!res.destroyed) {
        res.writeHead(502);
        res.end();
      }
    });
    res.on("close", () => upstream.destroy());
    req.pipe(upstream);
  });
  await new Promise<void>((resolve) => proxy.listen(0, "127.0.0.1", resolve));
  try {
    await launch();
    await addServer(
      "Performance server",
      `http://127.0.0.1:${(proxy.address() as { port: number }).port}`,
      remote.token,
    );
    await page
      .getByRole("button", { name: "Open another project", exact: true })
      .click();
    const picker = page.getByRole("dialog", {
      name: "Open remote project",
      exact: true,
    });
    const address = picker.getByLabel("Remote folder", { exact: true });
    await address.fill(project);
    await picker
      .getByRole("button", { name: "Open project", exact: true })
      .click();
    await expect(picker.getByRole("alert")).toHaveText(
      "The server took too long to open this project. Try again in a moment.",
    );
    await expect(address).toHaveValue(project);
    const start = performance.now();
    await picker
      .getByRole("button", { name: "Open project", exact: true })
      .click();
    await expect(picker).not.toBeVisible({ timeout: 15000 });
    await expect(page.locator(".application")).toHaveAttribute(
      "data-project-ready",
      "true",
    );
    await test.info().attach("cold-open", {
      body: JSON.stringify({
        milliseconds: Math.round(performance.now() - start),
        savedProjects: 61,
      }),
      contentType: "application/json",
    });
    // A large registry must not cause Git/filesystem probes in other projects.
    expect(await readFile(probes, "utf8").catch(() => "")).toBe("");
    await page.screenshot({
      path: test.info().outputPath("remote-project-open.png"),
    });
    // A routed connection must still recover when its workspace daemon exits.
    execFileSync(binary, ["--workspace", project, "serve", "--stop"], {
      env,
      timeout: 30000,
    });
    const opened = await (
      await fetch(remote.url + "/v1/host/open", {
        method: "POST",
        headers: {
          Authorization: "Bearer " + remote.token,
          "Content-Type": "application/json",
        },
        body: JSON.stringify({ path: project }),
      })
    ).json();
    const response = await fetch(
      remote.url + "/v1/host/workspaces/" + opened.id + "/v1/info",
      { headers: { Authorization: "Bearer " + remote.token } },
    );
    expect(response.status).toBe(200);
    expect((await response.json()).workspace).toBe(project);
    await page
      .getByRole("textbox", { name: "Message" })
      .fill("Remote opening is ready");
    await page
      .getByRole("button", { name: "Send message", exact: true })
      .click();
    await expect(page.locator(".transcript")).toContainText(
      "Echo: Remote opening is ready",
      { timeout: 30000 },
    );
    expect(await readFile(probes, "utf8").catch(() => "")).toBe("");
  } finally {
    await app?.close();
    proxy.closeAllConnections();
    await new Promise<void>((resolve) => proxy.close(() => resolve()));
  }
});

async function launch() {
  app = await electron.launch({ args: ["."], env });
  page = await app.firstWindow();
  page.on("pageerror", (error) => errors.push(error.message));
  await expect(
    page.getByRole("button", { name: "Switch server", exact: true }),
  ).toBeVisible();
}
async function settings(section: string) {
  const dialog = page.getByRole("dialog", { name: "Settings", exact: true });
  if (!(await dialog.isVisible()))
    await page.getByRole("button", { name: "Settings", exact: true }).click();
  await dialog.getByRole("button", { name: section, exact: true }).click();
  await expect(
    dialog.getByRole("button", { name: section, exact: true }),
  ).toHaveClass(/active/);
  return dialog;
}
async function closeSettings() {
  await page
    .getByRole("button", { name: "Close settings", exact: true })
    .click();
}
async function switchTo(name: string) {
  await page
    .getByRole("button", { name: "Switch server", exact: true })
    .click();
  await page.getByRole("menuitem", { name, exact: true }).click();
  await expect(page.locator(".server-switcher-button")).toHaveText(name);
  const hasProject = await page.evaluate(async () => {
    const server = await window.haarness.storage("active-server");
    return (await window.haarness.boot()).recent.some(
      (item) => (item.remote ? item.id : "local") === server,
    );
  });
  if (hasProject)
    await expect(page.locator(".application")).toHaveAttribute(
      "data-project-ready",
      "true",
    );
}
async function addServer(name: string, url: string, token: string) {
  const pane = await settings("Servers");
  await pane
    .getByRole("button", { name: "Add remote server", exact: true })
    .click();
  const form = page.locator("dialog[open]").last();
  await form
    .getByRole("textbox", { name: "Server URL", exact: true })
    .fill(url);
  await form.locator("summary").click();
  await form.getByLabel("Password", { exact: true }).fill(token);
  await form.getByRole("textbox", { name: "Name", exact: true }).fill(name);
  await form.getByRole("button", { name: "Connect", exact: true }).click();
  await expect(
    pane.locator(".setting-row").filter({ hasText: name }).locator(".current-tag"),
  ).toHaveText("Current");
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
  );
  await closeSettings();
}
async function endpoint(name: string) {
  const path = join(temporary, name);
  await mkdir(path);
  workspaces.push(path);
  const secret = "server-e2e-" + name;
  execFileSync(
    binary,
    ["--workspace", path, "serve", "--daemon", "--port", "0"],
    {
      env: {
        ...env,
        HAARNESS_SERVER_TOKEN: secret,
        HAARNESS_SERVER_PASSWORD: secret,
      },
      encoding: "utf8",
      timeout: 30000,
    },
  );
  const result = JSON.parse(
    execFileSync(binary, ["--workspace", path, "desktop"], {
      input: JSON.stringify({ action: "connect" }),
      env,
      encoding: "utf8",
      timeout: 120000,
    }),
  );
  return { path, url: result.url as string, token: result.token as string };
}
/** An HTTP proxy in front of `target` that a test can make refuse, hold or
 * cut requests, as a flaky network or a proxy in between would. `intercept`
 * answers a request itself (`true`), lets it through (`false`), or holds it
 * until a promise settles. */
async function relay(target: string) {
  const streams = new Set<ServerResponse>();
  /** Event streams that stay open but carry nothing more, as a connection
   * that died while the computer slept. */
  const frozen = new Set<ServerResponse>();
  const seen: string[] = [];
  const answered: string[] = [];
  let intercept:
    | ((req: IncomingMessage, res: ServerResponse) => boolean | Promise<void>)
    | undefined;
  const forward = (req: IncomingMessage, res: ServerResponse, line: string) => {
    if (req.url?.startsWith("/v1/events")) {
      streams.add(res);
      res.on("close", () => streams.delete(res));
    }
    res.on("finish", () => answered.push(line));
    const upstream = request(
      new URL(req.url!, target),
      { method: req.method, headers: req.headers },
      (incoming) => {
        res.writeHead(incoming.statusCode!, incoming.headers);
        incoming.on("data", (chunk) => {
          if (!frozen.has(res)) res.write(chunk);
        });
        incoming.on("end", () => res.end());
      },
    );
    upstream.on("error", () => {
      if (res.headersSent) res.destroy();
      else {
        res.writeHead(502);
        res.end();
      }
    });
    res.on("close", () => upstream.destroy());
    req.pipe(upstream);
  };
  const server = createServer((req, res) => {
    const line = req.method + " " + new URL(req.url!, target).pathname;
    seen.push(line);
    const decision = intercept?.(req, res) ?? false;
    if (decision === true) return;
    void Promise.resolve(decision).then(() => forward(req, res, line));
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  return {
    url: `http://127.0.0.1:${(server.address() as { port: number }).port}`,
    /** How many requests named `line` (`GET /v1/threads`) arrived. */
    count: (line: string) => seen.filter((item) => item === line).length,
    /** How many requests whose line ends with `end` arrived. */
    ending: (end: string) => seen.filter((item) => item.endsWith(end)).length,
    /** How many of them were answered by the server behind. */
    answered: (line: string) => answered.filter((item) => item === line).length,
    intercept(next: typeof intercept) {
      intercept = next;
    },
    /** Ends the event streams in flight, as a dropped connection does. */
    cut() {
      for (const stream of streams) stream.destroy();
    },
    /** Keeps the event streams in flight open, carrying nothing more. */
    freeze() {
      for (const stream of streams) frozen.add(stream);
    },
    async close() {
      server.closeAllConnections();
      await new Promise<void>((resolve) => server.close(() => resolve()));
    },
  };
}

test("a project whose event stream is refused waits longer each time instead of reading every chat again twice a second", async () => {
  test.setTimeout(120000);
  const remote = await endpoint("refused-stream");
  const proxy = await relay(remote.url);
  const client = new Haarness({ baseUrl: remote.url, token: remote.token });
  try {
    await launch();
    await addServer("Refusing server", proxy.url, remote.token);
    // Everything but the event stream is answered.
    proxy.intercept((req, res) => {
      if (!req.url?.startsWith("/v1/events")) return false;
      req.resume();
      res.writeHead(502);
      res.end();
      return true;
    });
    const before = proxy.count("GET /v1/threads");
    proxy.cut();
    await page.waitForTimeout(6000);
    // Waits of 0.5, 1, 2 and then 4 s: three reads of every chat in six
    // seconds, where a wait that started over each time the stream opened
    // read them about ten times.
    const reads = proxy.count("GET /v1/threads") - before;
    expect(reads).toBeGreaterThanOrEqual(2);
    expect(reads).toBeLessThanOrEqual(4);
    // Accepted again, the stream connects at the next attempt and carries
    // changes: a chat created on the server shows without another read.
    proxy.intercept(undefined);
    const refused = proxy.count("GET /v1/threads");
    await expect
      .poll(() => proxy.count("GET /v1/threads"), { timeout: 15000 })
      .toBeGreaterThan(refused);
    await expect(page.locator(".application")).toHaveAttribute(
      "data-project-ready",
      "true",
    );
    await page.waitForTimeout(1000);
    const settled = proxy.count("GET /v1/threads");
    await client.createThread({
      title: "Arrived over the stream",
      provider: "echo",
      model: "echo-1",
    });
    await expect(
      page.getByRole("button", { name: "Arrived over the stream", exact: true }),
    ).toBeVisible();
    expect(proxy.count("GET /v1/threads")).toBe(settled);
  } finally {
    client.close();
    await app?.close();
    await proxy.close();
  }
});

test("a project waiting to reconnect connects within 2 s of a wake or the network coming back, and a connected one is left alone by the network and the window", async () => {
  test.setTimeout(120000);
  const remote = await endpoint("outage");
  const proxy = await relay(remote.url);
  const ready = () => page.locator(".application");
  // Nine seconds without the server: attempts after 0.5, 1, 2 and 4 s leave
  // an 8 s wait before the next.
  const outage = async () => {
    proxy.intercept((req) => {
      req.socket.destroy();
      return true;
    });
    proxy.cut();
    await expect(ready()).toHaveAttribute("data-project-ready", "false");
    await page.waitForTimeout(9000);
    proxy.intercept(undefined);
  };
  try {
    await launch();
    await addServer("Outage server", proxy.url, remote.token);
    await outage();
    await app.evaluate(({ BrowserWindow }) =>
      BrowserWindow.getAllWindows()[0].webContents.send("haarness:event", {
        kind: "connection",
        data: "resume",
      }),
    );
    await expect(ready()).toHaveAttribute("data-project-ready", "true", {
      timeout: 2000,
    });
    await outage();
    await page.evaluate(() => window.dispatchEvent(new Event("online")));
    await expect(ready()).toHaveAttribute("data-project-ready", "true", {
      timeout: 2000,
    });
    // A connected project is not disturbed by the network or the window
    // coming back: nothing is read again.
    await page.waitForTimeout(1000);
    const reads = proxy.count("GET /v1/threads");
    await page.evaluate(() => {
      window.dispatchEvent(new Event("online"));
      document.dispatchEvent(new Event("visibilitychange"));
    });
    await page.waitForTimeout(1500);
    expect(proxy.count("GET /v1/threads")).toBe(reads);
    await expect(ready()).toHaveAttribute("data-project-ready", "true");
  } finally {
    await app?.close();
    await proxy.close();
  }
});

test("a project removed from the sidebar while it reconnects stays removed after relaunch", async () => {
  test.setTimeout(120000);
  const remote = await endpoint("removed-while-connecting");
  const proxy = await relay(remote.url);
  try {
    await launch();
    await addServer("Removal server", proxy.url, remote.token);
    const id = "remote:" + new URL(proxy.url).origin;
    // The open project's next read of its server fails, so it connects again
    // from the start, and that connection waits until released.
    let release!: () => void;
    const gate = new Promise<void>((resolve) => (release = resolve));
    let holding!: () => void;
    const held = new Promise<void>((resolve) => (holding = resolve));
    let infos = 0;
    proxy.intercept((req, res) => {
      if (new URL(req.url!, proxy.url).pathname !== "/v1/info") return false;
      if (++infos === 1) {
        req.resume();
        res.writeHead(503, { "Content-Type": "application/json" });
        res.end(JSON.stringify({ error: { message: "Server is busy" } }));
        return true;
      }
      holding();
      return gate;
    });
    proxy.cut();
    await held;
    const name = "removed-while-connecting";
    const heading = () =>
      page.locator(".sidebar .project-heading").filter({ hasText: name });
    await page
      .getByRole("button", { name: `Project options for ${name}`, exact: true })
      .click();
    await page.getByRole("menuitem", { name: "Remove project", exact: true }).click();
    await page
      .getByRole("dialog", { name: `Remove ${name} from the sidebar?`, exact: true })
      .getByRole("button", { name: "Remove project", exact: true })
      .click();
    await expect(heading()).toHaveCount(0);
    const answered = proxy.answered("GET /v1/info");
    proxy.intercept(undefined);
    release();
    await expect
      .poll(() => proxy.answered("GET /v1/info"))
      .toBeGreaterThan(answered);
    await page.waitForTimeout(1000);
    const saved = () =>
      page.evaluate(
        async (id) =>
          (await window.haarness.boot()).recent.some((item) => item.id === id),
        id,
      );
    expect(await saved()).toBe(false);
    await app.close();
    await launch();
    await expect(page.locator(".server-switcher-button")).toHaveText(
      "Removal server",
    );
    await expect(heading()).toHaveCount(0);
    expect(await saved()).toBe(false);
  } finally {
    await app?.close();
    await proxy.close();
  }
});

test("a wake leaves a connected project connected: it subscribes again from its last event, quietly", async () => {
  test.setTimeout(120000);
  const remote = await endpoint("quiet-wake");
  const proxy = await relay(remote.url);
  const client = new Haarness({ baseUrl: remote.url, token: remote.token });
  try {
    await launch();
    await addServer("Wake server", proxy.url, remote.token);
    await page.waitForTimeout(1000);
    // The stream died while the computer slept: open, and silent.
    proxy.freeze();
    await page.evaluate(() => {
      const root = document.querySelector(".application")!;
      const seen: string[] = [];
      (window as unknown as { readySeen: string[] }).readySeen = seen;
      new MutationObserver(() =>
        seen.push(root.getAttribute("data-project-ready") ?? ""),
      ).observe(root, {
        attributes: true,
        attributeFilter: ["data-project-ready"],
      });
    });
    const reads = () =>
      proxy.count("GET /v1/threads") +
      proxy.count("GET /v1/turns/active") +
      proxy.count("GET /v1/permissions");
    const before = reads();
    const streams = proxy.count("GET /v1/events");
    await app.evaluate(({ BrowserWindow }) =>
      BrowserWindow.getAllWindows()[0].webContents.send("haarness:event", {
        kind: "connection",
        data: "resume",
      }),
    );
    await expect
      .poll(() => proxy.count("GET /v1/events"))
      .toBeGreaterThan(streams);
    await client.createThread({
      title: "Arrived after the wake",
      provider: "echo",
      model: "echo-1",
    });
    await expect(
      page.getByRole("button", { name: "Arrived after the wake", exact: true }),
    ).toBeVisible();
    // It stayed Connected throughout, and nothing was read again.
    expect(reads()).toBe(before);
    expect(
      await page.evaluate(
        () => (window as unknown as { readySeen: string[] }).readySeen,
      ),
    ).toEqual([]);
    await expect(page.locator(".application")).toHaveAttribute(
      "data-project-ready",
      "true",
    );
  } finally {
    client.close();
    await app?.close();
    await proxy.close();
  }
});

test("a project connecting again starts a new attempt when the network comes back, even while one still hangs", async () => {
  test.setTimeout(120000);
  const remote = await endpoint("hanging-attempt");
  const proxy = await relay(remote.url);
  try {
    await launch();
    await addServer("Hanging server", proxy.url, remote.token);
    // Requests now go unanswered, as on a network that is not there yet.
    proxy.intercept(() => new Promise<void>(() => {}));
    const infos = proxy.count("GET /v1/info");
    proxy.cut();
    await expect
      .poll(() => proxy.count("GET /v1/info"))
      .toBeGreaterThan(infos);
    await expect(page.locator(".application")).toHaveAttribute(
      "data-project-ready",
      "false",
    );
    // The network is back, while the attempt in flight still waits.
    proxy.intercept(undefined);
    await page.evaluate(() => window.dispatchEvent(new Event("online")));
    await expect(page.locator(".application")).toHaveAttribute(
      "data-project-ready",
      "true",
      { timeout: 2000 },
    );
  } finally {
    await app?.close();
    await proxy.close();
  }
});

test("the open project removed in another window while it reconnects is left, not reconnected forever", async () => {
  test.setTimeout(120000);
  const remote = await endpoint("removed-elsewhere");
  const proxy = await relay(remote.url);
  try {
    await launch();
    await addServer("Elsewhere server", proxy.url, remote.token);
    const id = "remote:" + new URL(proxy.url).origin;
    let release!: () => void;
    const gate = new Promise<void>((resolve) => (release = resolve));
    let holding!: () => void;
    const held = new Promise<void>((resolve) => (holding = resolve));
    let infos = 0;
    proxy.intercept((req, res) => {
      if (new URL(req.url!, proxy.url).pathname !== "/v1/info") return false;
      if (++infos === 1) {
        req.resume();
        res.writeHead(503, { "Content-Type": "application/json" });
        res.end(JSON.stringify({ error: { message: "Server is busy" } }));
        return true;
      }
      holding();
      return gate;
    });
    proxy.cut();
    await held;
    // Another window removes it, through the same main process.
    await page.evaluate((id) => window.haarness.forget(id), id);
    proxy.intercept(undefined);
    release();
    await expect(page.getByRole("region", { name: "Project home" })).toBeVisible({
      timeout: 10000,
    });
    await expect(
      page.getByRole("textbox", { name: "Message", exact: true }),
    ).toHaveCount(0);
    expect(
      await page.evaluate(
        async (id) =>
          (await window.haarness.boot()).recent.some((item) => item.id === id),
        id,
      ),
    ).toBe(false);
  } finally {
    await app?.close();
    await proxy.close();
  }
});

test("an attempt a newer one replaced leaves the connected project alone when it fails late", async () => {
  test.setTimeout(120000);
  const remote = await endpoint("late-failure");
  const proxy = await relay(remote.url);
  try {
    await launch();
    await addServer("Late server", proxy.url, remote.token);
    // The open project's read of its server fails, so it connects again from
    // the start, and that connection waits, to fail later.
    let fail!: () => void;
    const failing = new Promise<void>((resolve) => (fail = resolve));
    let holding!: () => void;
    const held = new Promise<void>((resolve) => (holding = resolve));
    let infos = 0;
    proxy.intercept((req, res) => {
      if (new URL(req.url!, proxy.url).pathname !== "/v1/info") return false;
      if (++infos > 2) return false;
      req.resume();
      const refuse = () => {
        res.writeHead(503, { "Content-Type": "application/json" });
        res.end(JSON.stringify({ error: { message: "Server is busy" } }));
      };
      if (infos === 1) refuse();
      else {
        holding();
        void failing.then(refuse);
      }
      return true;
    });
    proxy.cut();
    await held;
    // The network is back: a new attempt connects while the first waits.
    await page.evaluate(() => window.dispatchEvent(new Event("online")));
    await expect(page.locator(".application")).toHaveAttribute(
      "data-project-ready",
      "true",
      { timeout: 5000 },
    );
    await page.waitForTimeout(500);
    await page.evaluate(() => {
      const root = document.querySelector(".application")!;
      const seen: string[] = [];
      (window as unknown as { readySeen: string[] }).readySeen = seen;
      new MutationObserver(() =>
        seen.push(root.getAttribute("data-project-ready") ?? ""),
      ).observe(root, {
        attributes: true,
        attributeFilter: ["data-project-ready"],
      });
    });
    const reads = proxy.count("GET /v1/threads");
    fail();
    await page.waitForTimeout(2000);
    expect(
      await page.evaluate(
        () => (window as unknown as { readySeen: string[] }).readySeen,
      ),
    ).toEqual([]);
    expect(proxy.count("GET /v1/threads")).toBe(reads);
    await expect(page.locator(".error-banner")).toHaveCount(0);
  } finally {
    await app?.close();
    await proxy.close();
  }
});

test("opening another project while the one before waits to reconnect leaves the new one undisturbed", async () => {
  test.setTimeout(180000);
  const remote = await endpoint("switch-first");
  const other = join(temporary, "switch-second");
  await mkdir(other);
  workspaces.push(other);
  const proxy = await relay(remote.url);
  const ready = () => page.locator(".application");
  const heading = (name: string) =>
    page.locator(".sidebar .project-heading").filter({ hasText: name });
  try {
    await launch();
    await addServer("Switch server", proxy.url, remote.token);
    // A second project on the same server, then back to the first.
    await page
      .getByRole("button", { name: "Open another project", exact: true })
      .click();
    const picker = page.getByRole("dialog", {
      name: "Open remote project",
      exact: true,
    });
    await picker.getByLabel("Remote folder", { exact: true }).fill(other);
    await picker
      .getByRole("button", { name: "Open project", exact: true })
      .click();
    await expect(picker).not.toBeVisible({ timeout: 60000 });
    await expect(heading("switch-second")).toHaveClass(/current/);
    await expect(ready()).toHaveAttribute("data-project-ready", "true");
    await heading("switch-first").click();
    await expect(heading("switch-first")).toHaveClass(/current/);
    await expect(ready()).toHaveAttribute("data-project-ready", "true");
    // Collapsed, so no live index of its list reads it once it is left: what
    // is counted below is the project's own reconnect.
    await page.getByRole("button", { name: "Collapse project switch-first", exact: true }).click();
    // The first project's stream is refused: its waits grow to 4 s.
    proxy.intercept((req, res) => {
      if (new URL(req.url!, proxy.url).pathname !== "/v1/events") return false;
      req.resume();
      res.writeHead(502);
      res.end();
      return true;
    });
    proxy.cut();
    await page.waitForTimeout(4000);
    await heading("switch-second").click();
    await expect(heading("switch-second")).toHaveClass(/current/);
    await expect(ready()).toHaveAttribute("data-project-ready", "true");
    await page.evaluate(() => {
      const root = document.querySelector(".application")!;
      const seen: string[] = [];
      (window as unknown as { readySeen: string[] }).readySeen = seen;
      new MutationObserver(() =>
        seen.push(root.getAttribute("data-project-ready") ?? ""),
      ).observe(root, {
        attributes: true,
        attributeFilter: ["data-project-ready"],
      });
    });
    const reads = proxy.ending("/v1/threads");
    // Past the moment the first project's next attempt was due.
    await page.waitForTimeout(6000);
    expect(proxy.ending("/v1/threads")).toBe(reads);
    expect(
      await page.evaluate(
        () => (window as unknown as { readySeen: string[] }).readySeen,
      ),
    ).toEqual([]);
  } finally {
    await app?.close();
    await proxy.close();
  }
});

test.beforeEach(async () => {
  temporary = await realpath(
    await mkdtemp(join(tmpdir(), "haarness-servers-")),
  );
  workspaces = [];
  errors.length = 0;
  env = Object.fromEntries(
    Object.entries(process.env).filter(
      (value): value is [string, string] => value[1] !== undefined,
    ),
  );
  Object.assign(env, {
    HAARNESS_BIN: binary,
    HAARNESS_DATA_DIR: join(temporary, "data"),
    HAARNESS_HOME: join(temporary, "home"),
    HAARNESS_DESKTOP_DATA: join(temporary, "desktop"),
    HAARNESS_TEST_PROVIDER: "1",
    HAARNESS_DESKTOP_TEST: "1",
  });
  delete env.ELECTRON_RUN_AS_NODE;
  await mkdir(join(temporary, "desktop"));
});
test.afterEach(async () => {
  await app?.close().catch(() => {});
  for (const workspace of workspaces) {
    try {
      execFileSync(binary, ["--workspace", workspace, "serve", "--stop"], {
        env,
        stdio: "ignore",
        timeout: 30000,
      });
    } catch {}
  }
  await rm(temporary, { recursive: true, force: true });
  expect(errors).toEqual([]);
});

test("server switching scopes projects and chats, restores selection, and preserves saved servers when projects are forgotten", async () => {
  test.setTimeout(180000);
  const local = await endpoint("local-project");
  const one = await endpoint("remote-one");
  const two = await endpoint("remote-two");
  const client = new Haarness({ baseUrl: one.url, token: one.token });
  const thread = await client.createThread({
    title: "Only on server one",
    provider: "echo",
    model: "echo-1",
  });
  client.close();
  await launch();
  await app.evaluate(({ dialog }, directory) => {
    dialog.showOpenDialog = async () => ({
      canceled: false,
      filePaths: [directory],
    });
  }, local.path);
  await page
    .getByRole("button", { name: "Open another project", exact: true })
    .click();
  await expect(page.locator(".sidebar .project-heading")).toHaveText([
    "local-project",
  ]);
  await addServer("Server One", one.url, one.token);
  await expect(page.locator(".sidebar .project-heading")).toHaveText([
    "remote-one",
  ]);
  await page
    .getByRole("button", { name: "Only on server one", exact: true })
    .click();
  await expect(
    page.getByRole("button", { name: "Only on server one", exact: true }),
  ).toHaveAttribute("aria-current", "true");
  const composer = page.getByRole("textbox", {
    name: "Message",
    exact: true,
  });
  await composer.fill("Draft for server one");
  await addServer("Server Two", two.url, two.token);
  await expect(page.locator(".sidebar .project-heading")).toHaveText([
    "remote-two",
  ]);
  await expect(composer).toHaveValue("");
  await page.getByRole("button", { name: "Search", exact: true }).click();
  await page
    .getByRole("combobox", { name: "Search Haarness", exact: true })
    .fill("Only on server one");
  await expect(
    page.getByRole("option", { name: "Only on server one", exact: true }),
  ).toHaveCount(0);
  await page.keyboard.press("Escape");
  await switchTo("Server One");
  await expect(composer).toHaveValue("Draft for server one");
  await expect
    .poll(() =>
      page.evaluate(
        (id) => window.haarness.storage("session:" + id),
        "remote:" + new URL(one.url).origin,
      ),
    )
    .toBe(thread.id);
  await page.reload();
  await expect(page.locator(".server-switcher-button")).toHaveText(
    "Server One",
  );
  await expect(page.locator(".sidebar .project-heading")).toHaveText([
    "remote-one",
  ]);
  await expect(composer).toHaveValue("Draft for server one");
  const pane = await settings("Projects");
  await expect(
    pane.getByRole("heading", { name: "Projects", exact: true }),
  ).toBeVisible();
  await expect(
    pane.getByRole("button", { name: "Add remote server", exact: true }),
  ).toHaveCount(0);
  // The sidebar's verb: Remove project, with the same confirmation.
  await pane.getByRole("button", { name: "Remove project remote-one", exact: true }).click();
  await page
    .locator("dialog[open]")
    .last()
    .getByRole("button", { name: "Remove project", exact: true })
    .click();
  await expect(pane).toContainText("No projects yet");
  const saved = await page.evaluate(() => window.haarness.boot());
  expect(saved.servers).toHaveLength(2);
  expect(
    saved.recent.some(
      (item) => item.id === "remote:" + new URL(one.url).origin,
    ),
  ).toBe(false);
  await pane.getByRole("button", { name: "Open project", exact: true }).click();
  const remotePicker = page.getByRole("dialog", {
    name: "Open remote project",
    exact: true,
  });
  await expect(remotePicker).toBeVisible();
  await remotePicker
    .getByLabel("Remote folder", { exact: true })
    .fill(one.path);
  await expect(
    remotePicker.getByRole("button", { name: "Open project", exact: true }),
  ).toBeEnabled();
  await remotePicker
    .getByRole("button", { name: "Open project", exact: true })
    .click();
  await expect(remotePicker).toHaveCount(0);
  await expect(pane).toContainText(one.path);
  await expect(pane).not.toContainText("No projects yet");
  await closeSettings();
  await switchTo("This computer");
  await expect(page.locator(".sidebar .project-heading")).toHaveText([
    "local-project",
  ]);
  await switchTo("Server Two");
  await settings("Servers");
  await page.screenshot({ path: "test-results/servers-settings.png" });
  await settings("Projects");
  await page.screenshot({ path: "test-results/projects-settings.png" });
  await closeSettings();
  execFileSync(binary, ["--workspace", one.path, "serve", "--stop"], {
    env,
    stdio: "ignore",
    timeout: 30000,
  });
  await page
    .getByRole("button", { name: "Switch server", exact: true })
    .click();
  await page.getByRole("menuitem", { name: "Server One", exact: true }).click();
  const recovery = page.getByRole("region", { name: "Project home" });
  await expect(
    recovery.getByRole("heading", { name: "Couldn’t open project" }),
  ).toBeVisible();
  await expect(
    recovery.getByRole("button", { name: "Retry", exact: true }),
  ).toBeVisible();
  await expect(page.locator(".server-switcher-button")).toHaveText(
    "Server One",
  );
  await expect(page.locator(".sidebar .project-heading")).toHaveText([
    "remote-one",
  ]);
  await expect(
    page.getByRole("textbox", { name: "Message", exact: true }),
  ).toHaveCount(0);
  await switchTo("Server Two");
  await expect(
    page.getByRole("textbox", { name: "Message", exact: true }),
  ).toBeEnabled();
});

test("legacy remote connections migrate independently and empty local selection survives relaunch", async () => {
  const remote = await endpoint("legacy-project");
  await launch();
  await addServer("Legacy server", remote.url, remote.token);
  const legacyCredential = await app.evaluate(
    ({ safeStorage }, token) =>
      safeStorage.encryptString(token).toString("base64"),
    remote.token,
  );
  await app.close();
  const file = join(temporary, "desktop", "desktop.json");
  const raw = JSON.parse(await readFile(file, "utf8"));
  raw.credentials["remote:" + new URL(remote.url).origin] = legacyCredential;
  delete raw.servers;
  delete raw.values["active-server"];
  raw.recent[0].label = "Legacy server";
  await writeFile(file, JSON.stringify(raw));
  await launch();
  await expect(page.locator(".server-switcher-button")).toHaveText(
    "Legacy server",
  );
  await expect(page.locator(".sidebar .project-heading")).toHaveText([
    "legacy-project",
  ]);
  const pane = await settings("Servers");
  await pane.getByRole("button", { name: "Rename", exact: true }).click();
  const form = page.locator("dialog[open]").last();
  await form
    .getByRole("textbox", { name: "Name", exact: true })
    .fill("Renamed server");
  await form.getByRole("button", { name: "Save", exact: true }).click();
  await closeSettings();
  await switchTo("This computer");
  // No project here yet: Home offers Open project, and the sidebar lists none.
  await expect(page.getByRole("region", { name: "Project home" })).toBeVisible();
  await expect(page.locator(".sidebar .project-heading")).toHaveCount(0);
  await app.close();
  await launch();
  await expect(page.locator(".server-switcher-button")).toHaveText(
    "This computer",
  );
  await expect(
    page.getByRole("textbox", { name: "Message", exact: true }),
  ).toHaveCount(0);
  await expect(
    page.getByRole("region", { name: "Project home" }),
  ).toBeVisible();
  await switchTo("Renamed server");
  const finalPane = await settings("Servers");
  await finalPane.getByRole("button", { name: "Remove", exact: true }).click();
  await page
    .getByRole("dialog", { name: "Remove server Renamed server?", exact: true })
    .getByRole("button", { name: "Remove server", exact: true })
    .click();
  await expect(finalPane).not.toContainText("Renamed server");
  const saved = await page.evaluate(() => window.haarness.boot());
  expect(saved.servers).toEqual([]);
  expect(saved.recent).toEqual([]);
  const client = new Haarness({ baseUrl: remote.url, token: remote.token });
  expect((await client.info()).workspace).toBe(remote.path);
  client.close();
});

test("switching to a server without a remembered project shows its project home", async () => {
  const remote = await endpoint("remote-choice");
  await launch();
  await addServer("Choose server", remote.url, remote.token);
  const server = await page.evaluate(() =>
    window.haarness.storage("active-server"),
  );
  const attachment = join(temporary, "retained.png");
  await writeFile(
    attachment,
    Buffer.from(
      "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aQx8AAAAASUVORK5CYII=",
      "base64",
    ),
  );
  await app.evaluate(({ dialog }, path) => {
    dialog.showOpenDialog = async () => ({
      canceled: false,
      filePaths: [path],
    });
  }, attachment);
  await attachFiles(page);
  await expect(
    page.getByRole("button", { name: "Remove retained.png", exact: true }),
  ).toBeVisible();
  await page
    .getByRole("textbox", { name: "Message", exact: true })
    .fill("Keep this remote draft");
  await switchTo("This computer");
  await page.evaluate(async (id) => {
    await window.haarness.storage("server-project:" + id, null);
  }, server);
  await page
    .getByRole("button", { name: "Switch server", exact: true })
    .click();
  await page
    .getByRole("menuitem", { name: "Choose server", exact: true })
    .click();
  const home = page.getByRole("region", { name: "Project home" });
  await expect(
    home.getByRole("heading", { name: "Choose a project", exact: true }),
  ).toBeVisible();
  await expect(home).toContainText("on this server");
  await expect(home).toContainText(remote.path);
  await expect(
    page.getByRole("textbox", { name: "Message", exact: true }),
  ).toHaveCount(0);
  await home
    .getByRole("button", { name: "Open another project", exact: true })
    .click();
  const picker = page.getByRole("dialog", {
    name: "Open remote project",
    exact: true,
  });
  await expect(picker).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(home).toBeVisible();
  await home.getByRole("button", { name: /remote-choice/ }).click();
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
  );
  await expect(
    page.getByRole("textbox", { name: "Message", exact: true }),
  ).toBeEnabled();
  await expect(
    page.getByRole("button", { name: "Remove retained.png", exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("textbox", { name: "Message", exact: true }),
  ).toHaveValue("Keep this remote draft");
  await page.reload();
  await expect(page.locator(".application")).toHaveAttribute(
    "data-project-ready",
    "true",
  );
  await expect(page.locator(".server-switcher-button")).toHaveText(
    "Choose server",
  );
});

test("opening a chat shows the centered loading mark without home flashes and ignores late history", async () => {
  const remote = await endpoint("conversation-loading");
  const client = new Haarness({ baseUrl: remote.url, token: remote.token });
  const first = await client.createThread({
    title: "Delayed conversation",
    provider: "echo",
    model: "echo-1",
  });
  const second = await client.createThread({
    title: "Latest conversation",
    provider: "echo",
    model: "echo-1",
  });
  for (const thread of [first, second]) {
    const turn = await client.startTurn(thread.id, {
      requestId: crypto.randomUUID(),
      text: thread.id,
    });
    await client.waitTurn(turn.id);
  }
  client.close();
  let release = () => {};
  let gate: Promise<void> | undefined;
  let arrived = 0;
  // Delay the actual history response; all data and mutations still use the real server.
  const proxy = createServer(async (req, res) => {
    if (req.url?.split("?")[0] === `/v1/threads/${first.id}/items` && gate) {
      arrived++;
      await gate;
    }
    if (res.destroyed) return;
    const upstream = request(
      new URL(req.url!, remote.url),
      { method: req.method, headers: req.headers },
      (incoming) => {
        res.writeHead(incoming.statusCode!, incoming.headers);
        incoming.pipe(res);
      },
    );
    upstream.on("error", () => {
      if (!res.destroyed) {
        res.writeHead(502);
        res.end();
      }
    });
    res.on("close", () => upstream.destroy());
    req.pipe(upstream);
  });
  await new Promise<void>((resolve) => proxy.listen(0, "127.0.0.1", resolve));
  const address = proxy.address() as { port: number };
  try {
    await launch();
    await addServer(
      "Loading server",
      `http://127.0.0.1:${address.port}`,
      remote.token,
    );
    const firstRow = page
      .locator(".project-list")
      .getByRole("button", { name: first.title!, exact: true });
    const secondRow = page
      .locator(".project-list")
      .getByRole("button", { name: second.title!, exact: true });
    await expect(firstRow).toBeVisible();
    await expect(secondRow).toBeVisible();
    await page.evaluate(() => {
      (window as any).homeFlashes = 0;
      (window as any).watchNavigation = false;
      document.addEventListener(
        "click",
        (event) => {
          if ((event.target as Element)?.closest(".project-list button"))
            (window as any).watchNavigation = true;
        },
        true,
      );
      const watch = () => {
        if (
          (window as any).watchNavigation &&
          [
            ...document.querySelectorAll(".project-home, .conversation-empty"),
          ].some((element) => element.getBoundingClientRect().height > 0)
        )
          (window as any).homeFlashes++;
        requestAnimationFrame(watch);
      };
      requestAnimationFrame(watch);
    });
    gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    await firstRow.click();
    await expect.poll(() => arrived).toBe(1);
    const loading = page.getByRole("status", {
      name: "Loading chat…",
      exact: true,
    });
    await expect(loading).toBeVisible();
    await expect(loading.locator(".start-mark .brand-mark")).toBeVisible();
    await expect(page.locator(".conversation-empty")).not.toBeVisible();
    await expect(
      page.getByRole("region", { name: "Project home" }),
    ).toHaveCount(0);
    await page.emulateMedia({ reducedMotion: "reduce" });
    await expect(loading.locator(".start-mark")).toHaveCSS(
      "animation-name",
      "none",
    );
    await page.screenshot({ path: "test-results/conversation-loading.png" });
    release();
    gate = undefined;
    await expect(page.locator(".transcript")).toContainText(
      "Echo: " + first.id,
    );
    await expect(loading).toHaveCount(0);
    await page.evaluate(() => {
      (window as any).watchNavigation = false;
    });
    await page
      .getByRole("button", { name: "New chat", exact: true })
      .click();
    gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    await firstRow.click();
    await expect.poll(() => arrived).toBe(2);
    await expect(loading).toBeVisible();
    await secondRow.click();
    await expect(page.locator(".transcript")).toContainText(
      "Echo: " + second.id,
    );
    await expect(loading).toHaveCount(0);
    release();
    gate = undefined;
    await expect
      .poll(() => page.evaluate(() => (window as any).homeFlashes))
      .toBe(0);
    // A subsequent real read is ordered after the released history request.
    await page.evaluate(async (id) => {
      const boot = await window.haarness.boot();
      await window.haarness.api(
        boot.recent.find((item) => item.remote)!.id,
        "listItems",
        [id],
      );
    }, first.id);
    await expect(secondRow).toHaveAttribute("aria-current", "true");
    await expect(page.locator(".transcript")).toContainText(
      "Echo: " + second.id,
    );
    await expect(page.locator(".transcript")).not.toContainText(
      "Echo: " + first.id,
    );
    expect(await page.evaluate(() => (window as any).homeFlashes)).toBe(0);
  } finally {
    release();
    await app?.close().catch(() => {});
    proxy.closeAllConnections();
    await new Promise<void>((resolve) => proxy.close(() => resolve()));
  }
});

test("explicit server shares real chat and file execution with Desktop and tui --attach", async () => {
  test.setTimeout(180000);
  const serverFolder = join(temporary, "served-project");
  const clientFolder = join(temporary, "separate-tui-folder");
  await mkdir(serverFolder);
  await mkdir(clientFolder);
  workspaces.push(serverFolder);
  const token = crypto.randomUUID();
  Object.assign(env, {
    HAARNESS_SERVER_TOKEN: token,
    HAARNESS_SERVER_PASSWORD: token,
    HAARNESS_DEFAULT_PROVIDER: "echo",
    HAARNESS_DEFAULT_MODEL: "echo-1",
  });
  const startServer = (port: string) =>
    execFileSync(
      binary,
      [
        "--workspace",
        serverFolder,
        "--yolo",
        "serve",
        "--daemon",
        "--host",
        "127.0.0.1",
        "--port",
        port,
      ],
      { env, encoding: "utf8", timeout: 30000 },
    );
  startServer("0");
  const registry = JSON.parse(
    await readFile(join(env.HAARNESS_DATA_DIR, "data/workspaces.json"), "utf8"),
  );
  const workspace = registry.workspaces.find(
    (item: { path: string }) => item.path === serverFolder,
  );
  expect(workspace).toBeTruthy();
  const runtime = JSON.parse(
    await readFile(
      join(
        env.HAARNESS_DATA_DIR,
        "cache/workspaces",
        workspace.id,
        "runtime.json",
      ),
      "utf8",
    ),
  );
  const url = runtime.url as string;
  const client = new Haarness({ baseUrl: url, token });
  const tmuxName = "haarness-desktop-attach-" + crypto.randomUUID();
  const tmux = (...args: string[]) =>
    execFileSync("tmux", args, {
      encoding: "utf8",
      timeout: 10000,
      stdio: ["ignore", "pipe", "pipe"],
    });
  const capture = () => {
    try {
      return tmux("capture-pane", "-p", "-t", tmuxName + ":0.0");
    } catch {
      return "";
    }
  };
  const stopTui = () => {
    try {
      tmux("kill-session", "-t", tmuxName);
    } catch {}
  };
  const sendDesktop = async (text: string) => {
    await page
      .getByRole("textbox", { name: "Message", exact: true })
      .fill(text);
    await page
      .getByRole("button", { name: "Send message", exact: true })
      .click();
    await expect(page.locator(".transcript")).toContainText("Echo: " + text);
    await expect.poll(async () => (await client.activeTurns()).length).toBe(0);
  };
  try {
    expect((await fetch(url + "/v1/info")).status).toBe(401);
    await launch();
    const pane = await settings("Servers");
    await pane
      .getByRole("button", { name: "Add remote server", exact: true })
      .click();
    const form = page.getByRole("dialog", {
      name: "Connect to server",
      exact: true,
    });
    await form.getByLabel("Server URL", { exact: true }).fill(url);
    await form.locator("summary").click();
    await form
      .getByLabel("Password", { exact: true })
      .fill("incorrect-test-token");
    await form.getByLabel("Name", { exact: true }).fill("Rejected server");
    await form.getByRole("button", { name: "Connect", exact: true }).click();
    await expect(form.getByRole("alert")).toContainText(
      "Check your username and password",
    );
    await expect(page.getByRole("alert")).not.toContainText(
      "incorrect-test-token",
    );
    expect(
      (await page.evaluate(() => window.haarness.boot())).servers,
    ).toHaveLength(0);
    await form.getByLabel("Password", { exact: true }).fill(token);
    await form.getByLabel("Name", { exact: true }).fill("Explicit test server");
    await form.getByRole("button", { name: "Connect", exact: true }).click();
    await expect(form).toHaveCount(0);
    await closeSettings();
    await sendDesktop("desktop-to-tui-proof");
    const [thread] = await client.listThreads();
    expect(thread).toBeTruthy();
    const startTui = () => {
      const args = [
        "env",
        "HAARNESS_DATA_DIR=" + join(temporary, "tui-data"),
        "HAARNESS_HOME=" + join(temporary, "tui-home"),
        "HAARNESS_SERVER_PASSWORD=" + token,
        "HAARNESS_TEST_PROVIDER=1",
        "HAARNESS_DEFAULT_PROVIDER=echo",
        "HAARNESS_DEFAULT_MODEL=echo-1",
        binary,
        "--workspace",
        clientFolder,
        "--session",
        thread.id,
        "tui",
        "--attach",
        url,
      ];
      const shell = args
        .map((value) => "'" + value.replaceAll("'", "'\\''") + "'")
        .join(" ");
      tmux("new-session", "-d", "-x", "150", "-y", "42", "-s", tmuxName, shell);
    };
    startTui();
    await expect
      .poll(capture, { timeout: 30000 })
      .toContain("Echo: desktop-to-tui-proof");
    tmux("send-keys", "-l", "-t", tmuxName, "tui-to-desktop-proof");
    tmux("send-keys", "-t", tmuxName, "Enter");
    await expect(page.locator(".transcript")).toContainText(
      "Echo: tui-to-desktop-proof",
    );
    await expect.poll(capture).toContain("Echo: tui-to-desktop-proof");
    await writeFile(
      test.info().outputPath("tui-shared-conversation.txt"),
      capture(),
    );
    await page.screenshot({
      path: test.info().outputPath("desktop-shared-conversation.png"),
    });
    await expect.poll(async () => (await client.activeTurns()).length).toBe(0);
    // Leading space keeps the Echo directive in chat; ! on an empty TUI composer opens shell mode.
    tmux(
      "send-keys",
      "-l",
      "-t",
      tmuxName,
      ' !tool write {"path":"attach-owned.txt","content":"Written through attached TUI"}',
    );
    tmux("send-keys", "-t", tmuxName, "Enter");
    await expect
      .poll(() =>
        readFile(join(serverFolder, "attach-owned.txt"), "utf8").catch(
          () => "",
        ),
      )
      .toBe("Written through attached TUI");
    expect(
      await readFile(join(clientFolder, "attach-owned.txt"), "utf8").catch(
        () => null,
      ),
    ).toBeNull();
    await expect(page.locator(".transcript")).toContainText("Tool write =>");
    await expect.poll(async () => (await client.activeTurns()).length).toBe(0);
    stopTui();
    await sendDesktop("while-tui-detached-proof");
    startTui();
    await expect
      .poll(capture, { timeout: 30000 })
      .toContain("Echo: while-tui-detached-proof");
    await writeFile(test.info().outputPath("tui-reattached.txt"), capture());
    await page.reload();
    await expect(page.locator(".transcript")).toContainText(
      "Echo: while-tui-detached-proof",
    );
    const original = await client.info();
    execFileSync(binary, ["--workspace", serverFolder, "serve", "--stop"], {
      env,
      timeout: 30000,
    });
    startServer(new URL(url).port);
    await expect
      .poll(async () => (await client.info()).serverInstanceId, {
        timeout: 30000,
      })
      .not.toBe(original.serverInstanceId);
    await sendDesktop("after-server-restart-proof");
    await expect
      .poll(capture, { timeout: 30000 })
      .toContain("Echo: after-server-restart-proof");
    await writeFile(
      test.info().outputPath("tui-after-server-restart.txt"),
      capture(),
    );
    await page.screenshot({
      path: test.info().outputPath("desktop-after-server-restart.png"),
    });
    await writeFile(
      test.info().outputPath("shared-items.json"),
      JSON.stringify(await client.listItems(thread.id), null, 2),
    );
    expect(await client.listThreads()).toHaveLength(1);
  } finally {
    stopTui();
    client.close();
  }
});

for (const password of ["", "日本:desktop-password"]) {
  test(`${password ? "password" : "anonymous"} server connects with optional name, restores credentials and opens remote projects`, async () => {
    test.setTimeout(180000);
    const folder = join(temporary, "server-project");
    const other = join(temporary, "another project 日本");
    await mkdir(folder);
    await mkdir(other);
    workspaces.push(folder, other);
    const serverEnv = {
      ...env,
      HAARNESS_SERVER_PASSWORD: password,
      HAARNESS_SERVER_USERNAME: "",
      HAARNESS_SERVER_TOKEN: "",
      HAARNESS_DEFAULT_PROVIDER: "echo",
      HAARNESS_DEFAULT_MODEL: "echo-1",
    };
    execFileSync(
      binary,
      ["--workspace", folder, "--yolo", "serve", "--daemon", "--port", "0"],
      { env: serverEnv, timeout: 30000 },
    );
    const registry = JSON.parse(
      await readFile(
        join(env.HAARNESS_DATA_DIR, "data/workspaces.json"),
        "utf8",
      ),
    );
    const workspace = registry.workspaces.find(
      (item: { path: string }) => item.path === folder,
    );
    const runtime = JSON.parse(
      await readFile(
        join(
          env.HAARNESS_DATA_DIR,
          "cache/workspaces",
          workspace.id,
          "runtime.json",
        ),
        "utf8",
      ),
    );
    const auth = password
      ? { type: "basic" as const, username: "haarness", password }
      : { type: "none" as const };
    const client = new Haarness({ baseUrl: runtime.url, auth });
    try {
      expect((await fetch(runtime.url + "/v1/info")).status).toBe(
        password ? 401 : 200,
      );
      await launch();
      const pane = await settings("Servers");
      await pane
        .getByRole("button", { name: "Add remote server", exact: true })
        .click();
      const form = page.getByRole("dialog", {
        name: "Connect to server",
        exact: true,
      });
      await expect(
        form.getByRole("button", { name: "Connect", exact: true }),
      ).toBeDisabled();
      await expect(form.getByLabel("Bearer token")).toHaveCount(0);
      await expect(
        form.getByLabel("Password", { exact: true }),
      ).not.toBeVisible();
      await form.getByLabel("Server URL", { exact: true }).fill(runtime.url);
      if (password) {
        await form.locator("summary").click();
        await form.getByLabel("Username", { exact: true }).fill("wrong-user");
        await form.getByLabel("Password", { exact: true }).fill(password);
        await form
          .getByRole("button", { name: "Connect", exact: true })
          .click();
        await expect(form.getByRole("alert")).toContainText(
          "Check your username and password",
        );
        await expect(
          form.getByLabel("Server URL", { exact: true }),
        ).toHaveValue(runtime.url);
        await expect(form.getByLabel("Password", { exact: true })).toHaveValue(
          password,
        );
        await expect(
          page.getByRole("region", { name: "Project home" }),
        ).not.toContainText("Couldn’t open project");
        await form.getByLabel("Username", { exact: true }).fill("");
        await form
          .getByRole("button", { name: "Show password", exact: true })
          .click();
        await expect(
          form.getByLabel("Password", { exact: true }),
        ).toHaveAttribute("type", "text");
        await form
          .getByRole("button", { name: "Hide password", exact: true })
          .click();
      }
      await page.screenshot({
        path: test.info().outputPath("connection-form.png"),
      });
      await form.getByRole("button", { name: "Connect", exact: true }).click();
      await expect(form).toHaveCount(0);
      await closeSettings();
      await expect(page.locator(".server-switcher-button")).toHaveText(
        "127.0.0.1",
      );
      let message = page.getByRole("textbox", {
        name: "Message",
        exact: true,
      });
      await message.fill("optional-auth-chat-proof");
      await page
        .getByRole("button", { name: "Send message", exact: true })
        .click();
      await expect(page.locator(".transcript")).toContainText(
        "Echo: optional-auth-chat-proof",
      );
      await expect
        .poll(async () => (await client.activeTurns()).length)
        .toBe(0);
      await app.close();
      const saved = await readFile(
        join(temporary, "desktop/desktop.json"),
        "utf8",
      );
      if (password) expect(saved).not.toContain(password);
      await launch();
      message = page.getByRole("textbox", { name: "Message", exact: true });
      await expect(page.locator(".transcript")).toContainText(
        "Echo: optional-auth-chat-proof",
      );
      await page
        .getByRole("button", { name: "Open another project", exact: true })
        .click();
      const picker = page.getByRole("dialog", {
        name: "Open remote project",
        exact: true,
      });
      await picker.getByLabel("Remote folder", { exact: true }).fill(other);
      await picker
        .getByRole("button", { name: "Open project", exact: true })
        .click();
      await expect(picker).toHaveCount(0);
      await message.fill("remote-project-auth-proof");
      await page
        .getByRole("button", { name: "Send message", exact: true })
        .click();
      await expect(page.locator(".transcript")).toContainText(
        "Echo: remote-project-auth-proof",
      );
      await page
        .getByRole("button", { name: "Toggle terminal", exact: true })
        .click();
      const terminal = page.locator(".xterm-helper-textarea");
      await expect(terminal).toBeFocused();
      await terminal.pressSequentially(
        "printf terminal-auth-proof > terminal-auth.txt",
        { delay: 5 },
      );
      await terminal.press("Enter");
      await expect
        .poll(() =>
          readFile(join(other, "terminal-auth.txt"), "utf8").catch(() => ""),
        )
        .toBe("terminal-auth-proof");
      expect(
        await readFile(join(folder, "terminal-auth.txt"), "utf8").catch(
          () => null,
        ),
      ).toBeNull();
      await page.screenshot({
        path: test.info().outputPath("remote-terminal.png"),
      });
    } finally {
      client.close();
    }
  });
}

test("remote folder explorer browses, completes paths, paginates and ignores stale suggestions", async () => {
  test.setTimeout(180000);
  const remote = await endpoint("folder-explorer");
  const names = [
    "alpha",
    "alpine",
    "beta 日本",
    ".hidden",
    ...Array.from(
      { length: 205 },
      (_, i) => `folder-${String(i).padStart(3, "0")}`,
    ),
  ];
  await Promise.all(names.map((name) => mkdir(join(remote.path, name))));
  await writeFile(join(remote.path, "ordinary-file.txt"), "not a folder");
  const headers = { Authorization: "Bearer " + remote.token };
  const listing = async (query: Record<string, string>) => {
    const response = await fetch(
      remote.url +
        "/v1/host/directories?" +
        new URLSearchParams({ path: remote.path, ...query }),
      { headers },
    );
    expect(response.status).toBe(200);
    return response.json();
  };
  const first = await listing({ limit: "2" });
  expect(first.directories.map((d: { name: string }) => d.name)).toEqual([
    "alpha",
    "alpine",
  ]);
  expect(first.nextCursor).toBe("alpine");
  const next = await listing({ limit: "2", after: first.nextCursor });
  expect(next.directories[0].name).toBe("beta 日本");
  expect((await listing({ prefix: ".h" })).directories[0].name).toBe(".hidden");
  expect((await listing({ prefix: "BETA" })).directories[0].name).toBe(
    "beta 日本",
  );
  expect(
    (await fetch(remote.url + "/v1/host/directories?limit=999", { headers }))
      .status,
  ).toBe(400);
  expect((await fetch(remote.url + "/v1/host/directories")).status).toBe(401);
  let release = () => {};
  let gate: Promise<void> | undefined;
  let delayed = 0;
  let catalogCalls = 0;
  const proxy = createServer(async (req, res) => {
    const url = new URL(req.url!, remote.url);
    if (url.pathname === "/v1/host/projects") {
      catalogCalls++;
      res.writeHead(503);
      res.end();
      return;
    }
    if (url.searchParams.get("prefix") === "al" && gate) {
      delayed++;
      await gate;
    }
    if (res.destroyed) return;
    const upstream = request(
      url,
      { method: req.method, headers: req.headers },
      (incoming) => {
        res.writeHead(incoming.statusCode!, incoming.headers);
        incoming.pipe(res);
      },
    );
    upstream.on("error", () => {
      if (!res.destroyed) {
        res.writeHead(502);
        res.end();
      }
    });
    res.on("close", () => upstream.destroy());
    req.pipe(upstream);
  });
  await new Promise<void>((resolve) => proxy.listen(0, "127.0.0.1", resolve));
  try {
    await launch();
    await addServer(
      "Explorer host",
      `http://127.0.0.1:${(proxy.address() as { port: number }).port}`,
      remote.token,
    );
    const beforeCatalog = catalogCalls;
    await page
      .getByRole("button", { name: "Open another project", exact: true })
      .click();
    const picker = page.getByRole("dialog", {
      name: "Open remote project",
      exact: true,
    });
    const path = picker.getByRole("combobox", {
      name: "Remote folder",
      exact: true,
    });
    const folders = picker.getByRole("list", {
      name: "Remote folders",
      exact: true,
    });
    await expect(path).toHaveValue(remote.path);
    await expect(
      picker.getByText("Known projects and working folders"),
    ).toHaveCount(0);
    await expect(
      picker.getByRole("button", { name: "Browse", exact: true }),
    ).toHaveCount(0);
    await expect(folders.getByRole("button")).toHaveCount(200);
    await expect(folders).not.toContainText("ordinary-file.txt");
    await expect(
      folders.getByRole("button", { name: ".hidden", exact: true }),
    ).toBeVisible();
    await expect(picker.getByLabel("Hidden folders")).toHaveCount(0);
    await picker
      .getByRole("button", { name: "Show more folders", exact: true })
      .click();
    await expect(folders.getByRole("button")).toHaveCount(209);
    gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    await path.fill(remote.path + "/al");
    await expect.poll(() => delayed).toBe(1);
    await path.fill(remote.path + "/bet");
    const suggestions = picker.getByRole("listbox", {
      name: "Suggested folders",
      exact: true,
    });
    await expect(suggestions.getByRole("option")).toHaveText(["beta 日本"]);
    release();
    gate = undefined;
    await expect(suggestions.getByRole("option")).toHaveText(["beta 日本"]);
    await path.press("ArrowDown");
    await expect(path).toHaveAttribute(
      "aria-activedescendant",
      "remote-path-option-0",
    );
    await page.screenshot({
      path: test.info().outputPath("remote-autocomplete.png"),
    });
    await path.press("Tab");
    await expect(path).toHaveValue(join(remote.path, "beta 日本") + "/");
    await path.press("Enter");
    await expect(path).toHaveValue(join(remote.path, "beta 日本"));
    await expect(picker).toContainText("No subfolders");
    await picker.getByRole("button", { name: "Back", exact: true }).click();
    await expect(path).toHaveValue(remote.path);
    await folders.getByRole("button", { name: "alpha", exact: true }).click();
    await expect(path).toHaveValue(join(remote.path, "alpha"));
    await picker
      .getByRole("button", { name: "Up one folder", exact: true })
      .click();
    await expect(path).toHaveValue(remote.path);
    await path.fill(join(remote.path, "does-not-exist"));
    await path.press("Enter");
    await expect(picker.getByRole("alert")).toContainText(
      "Couldn’t read this folder",
    );
    await expect(path).toHaveValue(join(remote.path, "does-not-exist"));
    await picker.getByRole("button", { name: "Home", exact: true }).click();
    await expect(picker.getByRole("alert")).toHaveCount(0);
    await expect(path).not.toHaveValue(join(remote.path, "does-not-exist"));
    await path.fill(remote.path);
    await path.press("Enter");
    await expect(
      folders.getByRole("button", { name: "alpha", exact: true }),
    ).toBeVisible();
    await page.screenshot({
      path: test.info().outputPath("remote-explorer-dark.png"),
    });
    await app.evaluate(({ BrowserWindow }) =>
      BrowserWindow.getAllWindows()[0].setSize(720, 640),
    );
    await page.evaluate(() => window.haarness.preferences({ theme: "light" }));
    await expect
      .poll(() =>
        picker.evaluate(
          (element) => element.scrollWidth <= element.clientWidth,
        ),
      )
      .toBe(true);
    await page.screenshot({
      path: test.info().outputPath("remote-explorer-light-narrow.png"),
    });
    expect(catalogCalls).toBe(beforeCatalog);
    await folders
      .getByRole("button", { name: "beta 日本", exact: true })
      .click();
    await expect(path).toHaveValue(join(remote.path, "beta 日本"));
    await picker
      .getByRole("button", { name: "Open project", exact: true })
      .click();
    await expect(picker).toHaveCount(0);
    workspaces.push(join(remote.path, "beta 日本"));
    await expect(page.locator(".application")).toHaveAttribute(
      "data-project-ready",
      "true",
    );
    await page
      .getByRole("textbox", { name: "Message", exact: true })
      .fill("explorer-selected-project-proof");
    await page
      .getByRole("button", { name: "Send message", exact: true })
      .click();
    await expect(page.locator(".transcript")).toContainText(
      "Echo: explorer-selected-project-proof",
      { timeout: 60_000 },
    );
  } finally {
    release();
    await app?.close().catch(() => {});
    proxy.closeAllConnections();
    await new Promise<void>((resolve) => proxy.close(() => resolve()));
  }
});

test("remote tools use standard permissions without an operating system sandbox", async () => {
  test.setTimeout(90000);
  await mkdir(join(env.HAARNESS_DATA_DIR, "config"), { recursive: true });
  await mkdir(join(env.HAARNESS_DATA_DIR, "data"), { recursive: true });
  await writeFile(
    join(env.HAARNESS_DATA_DIR, "config/config.toml"),
    '[defaults]\nprovider="echo"\nmodel="echo-1"\n[permissions]\nbash = { "printf blocked*" = "deny" }\n',
  );
  const legacy = join(env.HAARNESS_DATA_DIR, "data/sandbox.json");
  await writeFile(
    legacy,
    JSON.stringify({
      profile: "offline",
      allowedHosts: [],
      unsandboxedTools: [],
    }),
  );
  const remote = await endpoint("permission-host");
  const client = new Haarness({ baseUrl: remote.url, token: remote.token });
  const web = createServer((_, response) =>
    response.end("network permission proof"),
  );
  await new Promise<void>((done) => web.listen(0, "127.0.0.1", done));
  const url = `http://127.0.0.1:${(web.address() as { port: number }).port}`;
  const outside = join(temporary, "outside.txt");
  await writeFile(
    join(remote.path, "service.env"),
    "TEST_ONLY=permission-proof",
  );
  await writeFile(join(remote.path, ".env.example"), "EXAMPLE=public-proof");
  let threadId: string;
  const sendTool = async (name: string, args: unknown) => {
    await page
      .getByRole("textbox", { name: "Message", exact: true })
      .fill(`!tool ${name} ${JSON.stringify(args)}`);
    await page
      .getByRole("button", { name: "Send message", exact: true })
      .click();
    await expect
      .poll(async () => (await client.listThreads()).length)
      .toBeGreaterThan(0);
    threadId = (await client.listThreads())[0].id;
  };
  const finished = async () => {
    await expect.poll(async () => (await client.activeTurns()).length).toBe(0);
    await expect(
      page.getByRole("button", { name: "Stop response", exact: true }),
    ).toHaveCount(0);
  };
  const permission = async (category: string) => {
    await expect
      .poll(async () => (await client.listPermissions())[0]?.permission)
      .toBe(category);
    await expect(
      page.getByRole("button", { name: "Allow once", exact: true }),
    ).toBeVisible();
  };
  try {
    await launch();
    await addServer("Permissions server", remote.url, remote.token);
    const pane = await settings("General");
    await expect(
      pane.getByRole("button", { name: "Sandbox", exact: true }),
    ).toHaveCount(0);
    await expect(
      client.manageResource("sandbox", { action: "save", profile: "strict" }),
    ).rejects.toThrow();
    await closeSettings();
    await sendTool("write", {
      path: "ordinary.txt",
      content: "ordinary write proof",
    });
    await expect
      .poll(() =>
        readFile(join(remote.path, "ordinary.txt"), "utf8").catch(() => ""),
      )
      .toBe("ordinary write proof");
    await finished();
    await sendTool("bash", { command: `curl -fsS --max-time 5 ${url}` });
    await expect(page.locator(".transcript")).toContainText(
      "network permission proof",
    );
    await finished();
    await writeFile(legacy, '{"profile":"strict","allowedHosts":[]}');
    await sendTool("webfetch", { url });
    await finished();
    const fetchItems = await client.listItems(threadId!);
    expect(
      fetchItems.some(
        (item) =>
          item.data.type === "tool_result" &&
          !item.data.is_error &&
          item.data.output.includes("network permission proof"),
      ),
    ).toBe(true);
    await writeFile(legacy, "invalid retired policy");
    await sendTool("write", {
      path: outside,
      content: "approved outside proof",
    });
    await permission("external_directory");
    await expect(page.locator(".approval .prompt-title")).toContainText(
      "Access a folder outside this project",
    );
    expect(await readFile(outside, "utf8").catch(() => null)).toBeNull();
    await page.screenshot({
      path: test.info().outputPath("external-folder-approval.png"),
    });
    await page.getByRole("button", { name: "Allow once", exact: true }).click();
    await expect
      .poll(() => readFile(outside, "utf8").catch(() => ""))
      .toBe("approved outside proof");
    await finished();
    await sendTool("write", { path: outside, content: "must remain denied" });
    await permission("external_directory");
    await expect(page.locator(".approval .prompt-title")).toContainText(
      "Access a folder outside this project",
    );
    await page.getByRole("button", { name: "Deny", exact: true }).click();
    await finished();
    expect(await readFile(outside, "utf8")).toBe("approved outside proof");
    await sendTool("read", { path: "service.env" });
    await permission("read");
    await page.screenshot({
      path: test.info().outputPath("environment-file-approval.png"),
    });
    await page.getByRole("button", { name: "Allow once", exact: true }).click();
    await expect(page.locator(".transcript")).toContainText(
      "TEST_ONLY=permission-proof",
    );
    await finished();
    await sendTool("read", { path: ".env.example" });
    await expect(page.locator(".transcript")).toContainText(
      "EXAMPLE=public-proof",
    );
    await finished();
    await sendTool("bash", { command: "printf blocked > blocked.txt" });
    await finished();
    expect(
      await readFile(join(remote.path, "blocked.txt"), "utf8").catch(
        () => null,
      ),
    ).toBeNull();
    expect(await client.listPermissions()).toHaveLength(0);
    await writeFile(
      test.info().outputPath("permission-items.json"),
      JSON.stringify(await client.listItems(threadId!), null, 2),
    );
    await page.screenshot({
      path: test.info().outputPath("ordinary-tools-completed.png"),
    });
  } finally {
    client.close();
    web.closeAllConnections();
    await new Promise<void>((done) => web.close(() => done()));
  }
});

for (const connectionMode of ["local", "remote"] as const) {
  test(`${connectionMode} definition editors save overrides, retain plugin files, and reset defaults`, async () => {
    test.setTimeout(90000);
    const plugin = join(env.HAARNESS_DATA_DIR, "config/plugins/local/editor-kit");
    const sourceFiles = {
      ".claude-plugin/plugin.json": JSON.stringify({ name: "editor-kit", version: "1.0.0" }),
      "agents/helper.md": "---\ndescription: Plugin agent\nmodel: inherit\nmode: primary\ntools: {\"bash\":false}\npermissions: [{\"pattern\":\"bash:*\",\"decision\":\"deny\"}]\n---\nOriginal agent instructions.\n",
      "commands/check.md": "---\ndescription: Plugin command\nargument-hint: file\n---\nOriginal command $ARGUMENTS\n",
      "skills/editor-skill/SKILL.md": "---\nname: editor-skill\ndescription: Plugin skill\ncustom_metadata: retained\n---\nOriginal skill instructions. Read references/guide.txt.\n",
      "skills/editor-skill/references/guide.txt": "Keep the skill support files.\n",
      "skills/legacy/SKILL.md": "---\nname: Legacy Skill\ndescription: >-\n  A multiline\n  legacy description\n---\nLegacy instructions.\n",
      "skills/legacy/references/guide.txt": "Keep the skill support files.\n",
    };
    for (const [file, content] of Object.entries(sourceFiles)) {
      await mkdir(join(plugin, file, ".."), { recursive: true });
      await writeFile(join(plugin, file), content);
    }
    const remote = await endpoint("editor-project");
    await launch();
    if (connectionMode === "remote") await addServer("Editor server", remote.url, remote.token);
    else {
      await app.evaluate(({ dialog }, directory) => {
        dialog.showOpenDialog = async () => ({ canceled: false, filePaths: [directory] });
      }, remote.path);
      await page.getByRole("button", { name: "Open another project", exact: true }).click();
      await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
    }
    const current = connectionMode === "local" ? JSON.parse(execFileSync(binary,
      ["--workspace", remote.path, "--trust-project", "desktop"], {
        input: JSON.stringify({ action: "connect" }), env, encoding: "utf8", timeout: 30000,
      })) : remote;
    const client = new Haarness({ baseUrl: current.url, token: current.token });
    try {
      for (const internal of ["title", "compaction"]) {
        expect((await client.agents()).find(agent => agent.id === internal)?.hidden).toBe(true);
        for (const action of ["get", "create", "edit", "customize", "delete"])
          await expect(client.manageResource("assets", { kind: "agent", action, name: internal,
            scope: "global", description: "Blocked", prompt: "Blocked" })).rejects.toThrow(/managed internally/);
      }
      // Universal search stays at settings and controls; the Settings filter
      // is where installed agents are found, and the internal ones never are.
      const filtering = await settings("Agents");
      const filter = filtering.getByRole("textbox", { name: "Filter settings", exact: true });
      const results = filtering.getByRole("navigation", { name: "Settings search results", exact: true });
      await filter.fill("editor-kit:helper");
      await expect(results.getByTitle("editor-kit:helper", { exact: true })).toBeVisible();
      for (const internal of ["title", "compaction"]) {
        await filter.fill(internal);
        await expect(results).toBeVisible();
        await expect(results.getByTitle(internal, { exact: true })).toHaveCount(0);
      }
      await filter.fill("");
      for (const [section, kind, name, bodyLabel] of [
        ["Agents", "agent", "build", "Instructions"],
        ["Agents", "agent", "editor-kit:helper", "Instructions"],
        ["Commands", "command", "editor-kit:check", "Prompt template"],
        ["Commands", "command", "editor-kit:editor-skill", "Prompt template"],
        ["Skills", "skill", "editor-kit:editor-skill", "Instructions"],
        ["Skills", "skill", "editor-kit:legacy", "Instructions"],
      ]) {
        let pane = await settings(section);
        if (section === "Agents") {
          for (const internal of ["title", "compaction"])
            await expect(pane.locator(".setting-row").filter({ has: page.getByText(internal, { exact: true }) })).toHaveCount(0);
        }
        // Match exact definition titles, including namespaced plugin definitions.
        const entry = () => pane.locator(".setting-row").filter({ has: page.getByText(name, { exact: true }) });
        await expect(entry()).toHaveCount(1);
        await entry().getByRole("button", { name: "Edit", exact: true }).click();
        let form = page.getByRole("dialog", { name: `Edit ${kind}`, exact: true });
        const original = await form.getByLabel(bodyLabel, { exact: true }).inputValue();
        expect(original).not.toBe("");
        await expect(form.getByLabel(bodyLabel, { exact: true })).toBeEditable();
        await expect(form.getByRole("combobox", { name: "Available in", exact: true })).toBeEnabled();
        await form.getByLabel(bodyLabel, { exact: true }).fill(`Saved ${connectionMode} ${name} instructions.`);
        await form.getByRole("button", { name: "Save changes", exact: true }).click();
        await expect(form).toHaveCount(0);
        await expect(entry().getByRole("button", { name: "Reset to default", exact: true })).toBeVisible();
        if (kind === "agent") {
          await expect.poll(async () => (await client.agents()).filter(agent => agent.id === name).length).toBe(1);
          const path = join(env.HAARNESS_DATA_DIR, "config/agents", name.replaceAll(":", "/") + ".md");
          const text = await readFile(path, "utf8");
          if (name === "editor-kit:helper") {
            expect(text).toContain('"bash":false');
            expect(text).toContain('"decision":"deny"');
          }
        }
        if (kind === "command") {
          await expect.poll(async () => (await client.commands()).find(command => command.name === name)?.template)
            .toBe(`Saved ${connectionMode} ${name} instructions.`);
        }
        if (kind === "skill") {
          expect(await readFile(join(env.HAARNESS_DATA_DIR, `config/skills/${name.replaceAll(":", "/")}/references/guide.txt`), "utf8"))
            .toBe(sourceFiles["skills/editor-skill/references/guide.txt"]);
          await expect.poll(async () => (await client.loadSkill(name)).instructions).toContain(`Saved ${connectionMode}`);
        }
        // Reload the actual renderer and reopen to prove the saved draft is durable.
        await closeSettings();
        await page.reload();
        await page.locator(".project-heading").first().click();
        pane = await settings(section);
        await entry().getByRole("button", { name: "Edit", exact: true }).click();
        form = page.getByRole("dialog", { name: `Edit ${kind}`, exact: true });
        await expect(form.getByLabel(bodyLabel, { exact: true })).toHaveValue(`Saved ${connectionMode} ${name} instructions.`);
        await expect(form.getByRole("combobox", { name: "Available in", exact: true })).toBeDisabled();
        await form.getByLabel(bodyLabel, { exact: true }).fill(`Updated ${connectionMode} ${name} instructions.`);
        await page.screenshot({ path: test.info().outputPath(`${kind}-${name.replaceAll(":", "-")}-editable.png`), animations: "disabled" });
        await form.getByRole("button", { name: "Save changes", exact: true }).click();
        await expect(form).toHaveCount(0);
        await entry().getByRole("button", { name: "Edit", exact: true }).click();
        form = page.getByRole("dialog", { name: `Edit ${kind}`, exact: true });
        await expect(form.getByLabel(bodyLabel, { exact: true })).toHaveValue(`Updated ${connectionMode} ${name} instructions.`);
        await form.getByRole("button", { name: "Cancel", exact: true }).click();
        await entry().getByRole("button", { name: "Reset to default", exact: true }).click();
        await page.getByRole("dialog", { name: `Reset ${name}`, exact: true }).getByRole("button", { name: "Reset", exact: true }).click();
        await expect(entry().getByRole("button", { name: "Reset to default", exact: true })).toHaveCount(0);
        await entry().getByRole("button", { name: "Edit", exact: true }).click();
        form = page.getByRole("dialog", { name: `Edit ${kind}`, exact: true });
        await expect(form.getByLabel(bodyLabel, { exact: true })).toHaveValue(original);
        await form.getByRole("button", { name: "Cancel", exact: true }).click();
      }
      for (const [file, content] of Object.entries(sourceFiles)) expect(await readFile(join(plugin, file), "utf8")).toBe(content);
    } finally { client.close(); }
  });
}
