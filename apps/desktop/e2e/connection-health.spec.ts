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
import { execFile, execFileSync } from "node:child_process";
import { promisify } from "node:util";
import { connect, createServer, type Server, type Socket } from "node:net";
import { createServer as createHttpServer } from "node:http";

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
  await expect(pane.locator(".setting-row").filter({ hasText: name }).locator(".current-tag")).toHaveText("Current");
  await expect(page.locator(".application")).toHaveAttribute("data-project-ready", "true");
  await page.getByRole("button", { name: "Close settings", exact: true }).click();
}

async function endpoint(
  name: string,
  config = '[defaults]\nprovider="echo"\nmodel="echo-1"\n',
  extra: Record<string, string> = {},
) {
  const path = join(temporary, name);
  await mkdir(join(path, ".haarness"), { recursive: true });
  await writeFile(join(path, ".haarness/config.toml"), config);
  workspaces.push(path);
  const secret = "health-e2e-" + name;
  // Not synchronously: a fake endpoint this test serves answers the
  // server's model list while it starts.
  await promisify(execFile)(binary, ["--workspace", path, "serve", "--daemon", "--port", "0"], {
    env: { ...env, ...extra, HAARNESS_SERVER_TOKEN: secret, HAARNESS_SERVER_PASSWORD: secret },
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

test("a read that reaches a connection already closed shows no dialog, nor does a server stopped under live streams, while any other error still shows Electron's", async () => {
  test.setTimeout(150000);
  const remote = await endpoint("closed");
  const client = new Haarness({ baseUrl: remote.url, token: remote.token });
  try {
    const thread = await chat(client, "Closed connection chat");
    await launch();
    let stderr = "";
    app.process().stderr?.on("data", (chunk) => (stderr += chunk));
    // Electron's own listener opens its dialog through dialog.showErrorBox;
    // the monitor sees every uncaught exception, handled or not.
    await app.evaluate(({ dialog }) => {
      const shown: string[] = [];
      const uncaught: string[] = [];
      Object.assign(globalThis, { shownErrors: shown, uncaughtErrors: uncaught });
      dialog.showErrorBox = (title, content) => void shown.push(title + "\n" + content);
      process.on("uncaughtExceptionMonitor", (error) => void uncaught.push(error.stack ?? String(error)));
    });
    const seen = () =>
      app.evaluate(() => globalThis as unknown as { shownErrors: string[]; uncaughtErrors: string[] }).then(
        ({ shownErrors, uncaughtErrors }) => ({ shown: shownErrors, uncaught: uncaughtErrors }),
      );
    await addServer("Closed server", remote.url, remote.secret);
    await page.locator(".project-list").getByRole("button", { name: thread.title!, exact: true }).click();
    await expect(page.locator(".transcript")).toContainText("Echo: Closed connection chat start");

    // Twice, a socket as code that ends it inside one of its own reads leaves
    // it (an inspector's evaluate runs between any two calls): marked
    // destroyed, its handle still reading. 216 bytes arrive, and Node throws
    // from the read itself.
    for (let time = 0; time < 2; time++)
      await app.evaluate(
        () =>
          new Promise<void>((resolve) => {
            const net = process.getBuiltinModule("node:net");
            let socket: import("node:net").Socket;
            const server = net.createServer((peer) => {
              setTimeout(() => peer.write(Buffer.alloc(216, 120)), 50);
              setTimeout(() => {
                (socket as unknown as { _handle?: { close(): void } })._handle?.close();
                peer.destroy();
                server.close();
                resolve();
              }, 500);
            });
            server.listen(0, "127.0.0.1", () => {
              const { port } = server.address() as import("node:net").AddressInfo;
              socket = net.connect(port, "127.0.0.1", () => {
                (socket as { destroyed: boolean }).destroyed = true;
              });
              socket.on("data", () => {});
            });
          }),
      );
    let state = await seen();
    expect(state.uncaught).toHaveLength(2);
    for (const stack of state.uncaught)
      expect(stack).toMatch(/^RangeError \[ERR_OUT_OF_RANGE\][^\n]*Received 216\n.*getSystemErrorName.*\n.*ErrnoException.*\n.*TCP\.onStreamRead/);
    expect(state.shown).toEqual([]);
    // Noted once.
    await expect.poll(() => stderr.split("a read that reached a connection already closed").length - 1).toBe(1);

    // The server stops while a reply streams to the window and to the
    // notification watch: nothing interrupts, and the app keeps answering.
    const started = await client.startTurn(thread.id, { requestId: crypto.randomUUID(), text: "::long 20000" });
    await expect(page.locator(".transcript")).toContainText("marker-", { timeout: 30000 });
    execFileSync(binary, ["--workspace", workspaces.at(-1)!, "serve", "--stop"], { env, stdio: "ignore", timeout: 30000 });
    await client.waitTurn(started.id).catch(() => {});
    await page.waitForTimeout(3000);
    await page.getByRole("button", { name: "Settings", exact: true }).click();
    await expect(page.getByRole("dialog", { name: "Settings", exact: true })).toBeVisible();
    await page.getByRole("button", { name: "Close settings", exact: true }).click();
    state = await seen();
    expect(state.shown).toEqual([]);

    // Any other uncaught exception still opens Electron's dialog, as before.
    await app.evaluate(() => {
      setTimeout(() => {
        throw new Error("Another failure in main");
      });
    });
    await expect.poll(async () => (await seen()).shown).toEqual([
      expect.stringMatching(
        /^A JavaScript error occurred in the main process\nUncaught Exception:\nError: Another failure in main\n {4}at /,
      ),
    ]);
    expect(stderr.split("a read that reached a connection already closed").length - 1).toBe(1);
  } finally {
    client.close();
    await app?.close().catch(() => {});
  }
});

test("quitting in the middle of an event stream's read ends that stream after the read, and nothing is thrown", async () => {
  test.setTimeout(150000);
  const remote = await endpoint("quit");
  const client = new Haarness({ baseUrl: remote.url, token: remote.token });
  try {
    const thread = await chat(client, "Quit chat");
    await launch();
    let stderr = "";
    app.process().stderr?.on("data", (chunk) => (stderr += chunk));
    const exited = new Promise<number | null>((resolve) => app.process().once("exit", (code) => resolve(code)));
    await addServer("Quit server", remote.url, remote.secret);
    await page.locator(".project-list").getByRole("button", { name: thread.title!, exact: true }).click();
    await expect(page.locator(".transcript")).toContainText("Echo: Quit chat start");
    // app.quit() runs inside a read of an event stream, before Node looks
    // whether that socket is still open: where an inspector's evaluate (as
    // Playwright closes the app) can land, since it runs between any two
    // calls.
    await app.evaluate(({ app: electronApp }) => {
      const net = process.getBuiltinModule("node:net");
      const socket = net.Socket.prototype as unknown as {
        push(chunk: unknown, ...rest: unknown[]): boolean;
        _unrefTimer(): void;
      };
      const push = socket.push;
      const unref = socket._unrefTimer;
      const streams = new WeakSet<object>();
      socket.push = function (chunk, ...rest) {
        if (Buffer.isBuffer(chunk) && /^(?:id|data): /m.test(chunk.toString("latin1"))) streams.add(this);
        return push.call(this, chunk, ...rest);
      };
      let armed = true;
      socket._unrefTimer = function () {
        if (armed && streams.has(this) && /onStreamRead/.test(new Error().stack ?? "")) {
          armed = false;
          process.stderr.write("quitting inside an event stream's read\n");
          electronApp.quit();
        }
        return unref.call(this);
      };
      process.on("uncaughtExceptionMonitor", (error) => void process.stderr.write("uncaught: " + error.stack + "\n"));
    });
    await client.startTurn(thread.id, { requestId: crypto.randomUUID(), text: "::long 2000" });
    expect(await exited).toBe(0);
    expect(stderr).toContain("quitting inside an event stream's read");
    expect(stderr).not.toContain("uncaught:");
  } finally {
    client.close();
    await app?.close().catch(() => {});
  }
});

/** An Anthropic-format endpoint that lists its models in four pages of
 * 2.2 s each (inside the server's 3 s for a page and 10 s for the list), and
 * answers a message with "late hello". */
async function lateModels() {
  let listed = 0;
  const server = createHttpServer((request, response) => {
    if (request.method === "POST") {
      request.resume();
      const events = [
        ["message_start", { type: "message_start", message: { id: "msg_fixture", type: "message", role: "assistant", model: "late-model-1", content: [], stop_reason: null, usage: { input_tokens: 1, output_tokens: 0 } } }],
        ["content_block_start", { type: "content_block_start", index: 0, content_block: { type: "text", text: "" } }],
        ["content_block_delta", { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: "late hello" } }],
        ["content_block_stop", { type: "content_block_stop", index: 0 }],
        ["message_delta", { type: "message_delta", delta: { stop_reason: "end_turn" }, usage: { output_tokens: 2 } }],
        ["message_stop", { type: "message_stop" }],
      ];
      response.writeHead(200, { "content-type": "text/event-stream" });
      response.end(events.map(([name, data]) => `event: ${name}\ndata: ${JSON.stringify(data)}\n\n`).join(""));
      return;
    }
    const after = /after_id=page-(\d)/.exec(request.url ?? "");
    const page = after ? Number(after[1]) + 1 : 1;
    setTimeout(() => {
      response.writeHead(200, { "content-type": "application/json" });
      response.end(
        JSON.stringify({
          data: [{ id: `late-model-${page}`, display_name: `Late model ${page}`, type: "model" }],
          has_more: page < 4,
          last_id: `page-${page}`,
        }),
      );
      if (page === 4) listed = Date.now();
    }, 2200);
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  return {
    url: `http://127.0.0.1:${(server.address() as { port: number }).port}/v1`,
    /** When the whole list was answered, or 0. */
    listed: () => listed,
    close: () => new Promise<void>((resolve) => server.close(() => resolve())),
  };
}

test("a project whose model list is slow opens at once, and a message sent meanwhile goes to the model that list offers", async () => {
  test.setTimeout(150000);
  const models = await lateModels();
  let client: Haarness | undefined;
  try {
    await launch();
    // The server's list is in about 9 s after it starts.
    const remote = await endpoint(
      "slow-models",
      '[defaults]\nprovider="slowlist"\nmodel="late-model-1"\n' +
        `[providers.slowlist]\nprotocol="anthropic-messages"\nname="Slow list"\nbase_url="${models.url}"\napi_key_env="SLOWLIST_KEY"\n`,
      { SLOWLIST_KEY: "fixture-key" },
    );
    client = new Haarness({ baseUrl: remote.url, token: remote.token });
    await addServer("Slow models", remote.url, remote.secret);
    const ready = Date.now();
    const pill = page.locator(".composer-model");
    await expect(pill).toHaveText("Loading models…");
    // Sent while the list loads: it goes out once the list is in, rather
    // than to the first model another provider offers.
    const composer = page.getByRole("textbox", { name: "Message", exact: true });
    await composer.fill("hello slow list");
    await composer.press("Enter");
    await composer.press("Enter");
    expect(models.listed()).toBe(0);
    await expect(page.locator(".transcript")).toContainText("late hello", { timeout: 30000 });
    // The project was ready while the list was still on its way.
    expect(models.listed()).toBeGreaterThan(ready);
    await expect(pill).toContainText("Late model 1");
    const threads = await client.listThreads();
    expect(threads.map((thread) => `${thread.provider}/${thread.model}`)).toEqual(["slowlist/late-model-1"]);
  } finally {
    client?.close();
    await app?.close().catch(() => {});
    await models.close();
  }
});

test("a message waiting for a slow model list is given up when another chat is shown meanwhile, and nothing is sent from that chat", async () => {
  test.setTimeout(150000);
  const models = await lateModels();
  let client: Haarness | undefined;
  try {
    await launch();
    const remote = await endpoint(
      "slow-models-switch",
      '[defaults]\nprovider="slowlist"\nmodel="late-model-1"\n' +
        `[providers.slowlist]\nprotocol="anthropic-messages"\nname="Slow list"\nbase_url="${models.url}"\napi_key_env="SLOWLIST_KEY"\n`,
      { SLOWLIST_KEY: "fixture-key" },
    );
    client = new Haarness({ baseUrl: remote.url, token: remote.token });
    const existing = await chat(client, "Existing chat");
    await addServer("Slow models", remote.url, remote.secret);
    const pill = page.locator(".composer-model");
    await expect(pill).toHaveText("Loading models…");
    const composer = page.getByRole("textbox", { name: "Message", exact: true });
    await composer.fill("sent from the new chat");
    await composer.press("Enter");
    // Another chat, with a draft of its own, while the list still loads.
    await page.locator(".project-list").getByRole("button", { name: existing.title!, exact: true }).click();
    await expect(page.locator(".transcript")).toContainText("Echo: Existing chat start");
    await composer.fill("not sent");
    expect(models.listed()).toBe(0);
    await expect.poll(() => models.listed(), { timeout: 30000 }).toBeGreaterThan(0);
    await expect(pill).not.toHaveText("Loading models…");
    await page.waitForTimeout(1500);
    await expect(page.locator(".transcript")).not.toContainText("not sent");
    await expect(composer).toHaveValue("not sent");
    const items = await client.listItems(existing.id);
    expect(JSON.stringify(items)).not.toContain("not sent");
    expect((await client.listThreads()).map((thread) => thread.id)).toEqual([existing.id]);
    // The message Enter was pressed on stays in its draft, unsent.
    await page.getByRole("button", { name: "New chat", exact: true }).click();
    await expect(composer).toHaveValue("sent from the new chat");
  } finally {
    client?.close();
    await app?.close().catch(() => {});
    await models.close();
  }
});
