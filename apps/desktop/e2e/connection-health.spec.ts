import {
  test,
  expect,
  _electron as electron,
  type ElectronApplication,
  type Page,
} from "@playwright/test";
import { Haarness, type Thread } from "@haarness/sdk";
import { mkdtemp, mkdir, rm, realpath, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";
import { connect, createServer, type Server, type Socket } from "node:net";

// A server connection that dies without closing: the machine slept, the
// network changed, a proxy froze. Desktop notices it and connects again by
// itself, and leaves a healthy one alone.

const binary = resolve(process.env.HAARNESS_BIN ?? "../../target/debug/haarness");
let app: ElectronApplication;
let page: Page;
let temporary: string;
let env: Record<string, string>;
let workspaces: string[];
const errors: string[] = [];

/**
 * A TCP proxy in front of a server. `freeze` stops passing bytes on the
 * event streams and terminal sockets open now, without closing them; `down`
 * ends what is open and answers 502 to every request until it is up again.
 */
async function proxy(target: string) {
  const upstream = new URL(target);
  const pairs = new Set<{ client: Socket; server: Socket; stream: boolean; frozen: boolean }>();
  let down = false;
  let streams = 0;
  const server: Server = createServer((client) => {
    if (down) {
      // As a gateway whose server cannot be reached answers.
      client.once("data", () =>
        client.end("HTTP/1.1 502 Bad Gateway\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"),
      );
      client.on("error", () => {});
      return;
    }
    const pair = {
      client,
      server: connect(Number(upstream.port), upstream.hostname),
      stream: false,
      frozen: false,
    };
    pairs.add(pair);
    client.on("data", (chunk) => {
      // A pooled connection may carry other requests before its stream.
      const text = chunk.toString("latin1");
      if (/GET \S*\/v1\/events/.test(text) || /\r\nupgrade: websocket/i.test(text)) {
        pair.stream = true;
        streams++;
      }
      if (!pair.frozen) pair.server.write(chunk);
    });
    pair.server.on("data", (chunk) => {
      if (!pair.frozen) client.write(chunk);
    });
    const end = () => {
      client.destroy();
      pair.server.destroy();
      pairs.delete(pair);
    };
    for (const socket of [client, pair.server]) {
      socket.on("close", end);
      socket.on("error", end);
    }
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  return {
    url: `http://127.0.0.1:${(server.address() as { port: number }).port}`,
    /** Event streams and terminal sockets asked for so far. */
    streams: () => streams,
    freeze() {
      for (const pair of pairs) if (pair.stream) pair.frozen = true;
    },
    down(value: boolean) {
      down = value;
      if (value) for (const pair of pairs) pair.client.destroy();
    },
    close: () =>
      new Promise<void>((resolve) => {
        for (const pair of pairs) pair.client.destroy();
        server.close(() => resolve());
      }),
  };
}

async function launch() {
  app = await electron.launch({ args: ["."], env });
  page = await app.firstWindow();
  page.on("pageerror", (error) => errors.push(error.message));
  await expect(page.getByRole("button", { name: "Switch server", exact: true })).toBeVisible();
}

async function addServer(name: string, url: string, token: string) {
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  const pane = page.getByRole("dialog", { name: "Settings", exact: true });
  await pane.getByRole("button", { name: "Servers", exact: true }).click();
  await pane.getByRole("button", { name: "Add remote server", exact: true }).click();
  const form = page.locator("dialog[open]").last();
  await form.getByRole("textbox", { name: "Server URL", exact: true }).fill(url);
  await form.locator("summary").click();
  await form.getByLabel("Password", { exact: true }).fill(token);
  await form.getByRole("textbox", { name: "Name", exact: true }).fill(name);
  await form.getByRole("button", { name: "Connect", exact: true }).click();
  await expect(pane.locator(".setting-row").filter({ hasText: name })).toContainText("Selected");
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
  await page.getByRole("button", { name: "Close settings", exact: true }).click();
}

async function endpoint(name: string) {
  const path = join(temporary, name);
  await mkdir(join(path, ".haarness"), { recursive: true });
  await writeFile(join(path, ".haarness/config.toml"), '[defaults]\nprovider="echo"\nmodel="echo-1"\n');
  workspaces.push(path);
  const secret = "health-e2e-" + name;
  execFileSync(binary, ["--workspace", path, "serve", "--daemon", "--port", "0"], {
    env: { ...env, HAARNESS_SERVER_TOKEN: secret, HAARNESS_SERVER_PASSWORD: secret },
    encoding: "utf8",
    timeout: 30000,
  });
  const result = JSON.parse(
    execFileSync(binary, ["--workspace", path, "desktop"], {
      input: JSON.stringify({ action: "connect" }),
      env,
      encoding: "utf8",
      timeout: 120000,
    }),
  );
  return { url: result.url as string, token: result.token as string, secret };
}

async function chat(client: Haarness, title: string) {
  const thread = await client.createThread({ title, provider: "echo", model: "echo-1" });
  const turn = await client.startTurn(thread.id, { requestId: crypto.randomUUID(), text: title + " start" });
  await client.waitTurn(turn.id);
  return thread;
}

async function turn(client: Haarness, thread: Thread, text: string) {
  const started = await client.startTurn(thread.id, { requestId: crypto.randomUUID(), text });
  return client.waitTurn(started.id);
}

test.beforeEach(async () => {
  temporary = await realpath(await mkdtemp(join(tmpdir(), "haarness-health-")));
  workspaces = [];
  errors.length = 0;
  env = Object.fromEntries(
    Object.entries(process.env).filter((value): value is [string, string] => value[1] !== undefined),
  );
  Object.assign(env, {
    HAARNESS_BIN: binary,
    HAARNESS_DATA_DIR: join(temporary, "data"),
    HAARNESS_HOME: join(temporary, "home"),
    HAARNESS_DESKTOP_DATA: join(temporary, "desktop"),
    HAARNESS_TEST_PROVIDER: "1",
    HAARNESS_DESKTOP_TEST: "1",
    // Past the server's 15 s keepalive, so a healthy stream outlasts it.
    HAARNESS_DESKTOP_EVENT_IDLE_MS: "20000",
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

test("a frozen event stream is noticed and a reply sent afterwards shows up, while a healthy one is left alone", async () => {
  test.setTimeout(150000);
  const remote = await endpoint("frozen");
  const front = await proxy(remote.url);
  const client = new Haarness({ baseUrl: remote.url, token: remote.token });
  try {
    const thread = await chat(client, "Frozen stream chat");
    await launch();
    await addServer("Frozen server", front.url, remote.secret);
    await page.locator(".project-list").getByRole("button", { name: thread.title!, exact: true }).click();
    await expect(page.locator(".transcript")).toContainText("Echo: Frozen stream chat start");
    // Healthy and quiet for longer than the limit: nothing reconnects.
    const opened = front.streams();
    await page.waitForTimeout(23000);
    expect(front.streams()).toBe(opened);
    await turn(client, thread, "healthy reply");
    await expect(page.locator(".transcript")).toContainText("Echo: healthy reply");

    front.freeze();
    await turn(client, thread, "reply after the freeze");
    await expect(page.locator(".transcript")).toContainText("Echo: reply after the freeze", {
      timeout: 40000,
    });
    expect(front.streams()).toBeGreaterThan(opened);
  } finally {
    client.close();
    await app?.close().catch(() => {});
    await front.close();
  }
});

test("a wake ends the backoff of a notification stream that is trying again", async () => {
  test.setTimeout(150000);
  const remote = await endpoint("wake");
  const front = await proxy(remote.url);
  const client = new Haarness({ baseUrl: remote.url, token: remote.token });
  try {
    const shown = await chat(client, "Shown chat");
    const background = await chat(client, "Background chat");
    await launch();
    await app.evaluate(({ Notification }) => {
      (globalThis as any).notificationRecords = [];
      Notification.isSupported = () => true;
      Notification.prototype.show = function () {
        (globalThis as any).notificationRecords.push(this);
      };
    });
    await addServer("Wake server", front.url, remote.secret);
    await page.locator(".project-list").getByRole("button", { name: shown.title!, exact: true }).click();
    await expect(page.locator(".transcript")).toContainText("Echo: Shown chat start");
    const window = await app.browserWindow(page);
    await window.evaluate((win) => win.hide());
    await expect.poll(() => window.evaluate((win) => win.isFocused())).toBe(false);

    // Away long enough that the notification stream waits 16 s between tries.
    front.down(true);
    await page.waitForTimeout(17000);
    front.down(false);
    await app.evaluate(({ powerMonitor }) => powerMonitor.emit("resume"));
    await turn(client, background, "finished while away");
    await expect
      .poll(
        () =>
          app.evaluate(() =>
            (globalThis as any).notificationRecords.some((record: { body: string }) =>
              record.body.startsWith("Background chat · "),
            ),
          ),
        { timeout: 6000 },
      )
      .toBe(true);
  } finally {
    client.close();
    await app?.close().catch(() => {});
    await front.close();
  }
});

test("a wake follows a notification stream again that still looks open but died while asleep", async () => {
  test.setTimeout(150000);
  const remote = await endpoint("asleep");
  const front = await proxy(remote.url);
  const client = new Haarness({ baseUrl: remote.url, token: remote.token });
  try {
    const shown = await chat(client, "Shown chat");
    const background = await chat(client, "Background chat");
    await launch();
    await app.evaluate(({ Notification }) => {
      (globalThis as any).notificationRecords = [];
      Notification.isSupported = () => true;
      Notification.prototype.show = function () {
        (globalThis as any).notificationRecords.push(this);
      };
    });
    await addServer("Asleep server", front.url, remote.secret);
    await page.locator(".project-list").getByRole("button", { name: shown.title!, exact: true }).click();
    await expect(page.locator(".transcript")).toContainText("Echo: Shown chat start");
    const window = await app.browserWindow(page);
    await window.evaluate((win) => win.hide());
    await expect.poll(() => window.evaluate((win) => win.isFocused())).toBe(false);

    // Every open stream went dead without closing, as across a sleep.
    front.freeze();
    await app.evaluate(({ powerMonitor }) => powerMonitor.emit("resume"));
    await turn(client, background, "finished after the wake");
    await expect
      .poll(
        () =>
          app.evaluate(() =>
            (globalThis as any).notificationRecords.some((record: { body: string }) =>
              record.body.startsWith("Background chat · "),
            ),
          ),
        { timeout: 6000 },
      )
      .toBe(true);
  } finally {
    client.close();
    await app?.close().catch(() => {});
    await front.close();
  }
});
