import test from "node:test";
import assert from "node:assert/strict";
import { Haarness, ApiError, TransportError, attachments } from "../dist/index.js";
import { mkdtemp, open, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

test("media accepts larger files and enforces the twenty MiB aggregate boundary", async () => {
  const directory = await mkdtemp(join(tmpdir(), "haarness-sdk-media-"));
  const path = join(directory, "fixture.png");
  const file = await open(path, "w");
  try {
    await file.truncate(11 * 1024 * 1024);
    assert.equal((await attachments([path])).length, 1);
    await assert.rejects(attachments([path, path]), /20 MiB/);
    await file.truncate(20 * 1024 * 1024);
    assert.equal((await attachments([path])).length, 1);
    await file.truncate(20 * 1024 * 1024 + 1);
    await assert.rejects(attachments([path]), /20 MiB/);
  } finally { await file.close(); await rm(directory, { recursive: true, force: true }); }
});

test("path/query encoding, null resets, binary transport and typed errors", async () => {
  let calls = [];
  const client = new Haarness({
    baseUrl: "http://localhost:8080",
    token: "fixture-token",
    fetch: async (url, init) => {
      calls.push([url, init]);
      if (url.includes("/download"))
        return new Response(new Uint8Array([0, 255, 42]));
      if (url.endsWith("/missing"))
        return new Response(
          JSON.stringify({
            code: "not_found",
            message: "Missing",
            retryable: false,
          }),
          { status: 404 },
        );
      return new Response(JSON.stringify({ ok: true }));
    },
  });
  await client.loadSkill("spaces /?#雪");
  assert.match(calls[0][0], /spaces%20%2F%3F%23%E9%9B%AA/);
  assert.equal(calls[0][1].headers.Authorization, "Bearer fixture-token");
  await client.listThreads({ archived: false });
  assert.match(calls.at(-1)[0], /archived=false/);
  await client.updateTool("read", { enabled: null });
  assert.equal(calls.at(-1)[1].body, '{"enabled":null}');
  await client.callMcpTool("fixture", "echo", null);
  assert.equal(calls.at(-1)[1].body, "null");
  await client.uploadProfile(new Uint8Array([1, 2]));
  assert.equal(calls.at(-1)[1].headers["Content-Type"], "application/zip");
  assert.deepEqual(
    await client.downloadProfile("fixture"),
    new Uint8Array([0, 255, 42]),
  );
  await assert.rejects(
    client.getThread("missing"),
    (e) => e instanceof ApiError && e.status === 404 && e.code === "not_found",
  );
  assert.equal(calls.filter((x) => x[0].endsWith("/missing")).length, 1);
  assert.throws(() => client.getThread(".."), TypeError);
  client.close();
});

test("SSE handles UTF-8/CRLF chunk boundaries, multiline data, replay deduplication, reconnect cursor and resync", async () => {
  let calls = 0,
    received = [];
  const client = new Haarness({
    baseUrl: "http://localhost:8080",
    fetch: async (url, init) => {
      received.push(init.headers["Last-Event-ID"]);
      calls++;
      const event = (id) => ({
        schemaVersion: "1",
        type: "item.created",
        serverInstanceId: "instance",
        eventId: id,
        payload: "雪",
      });
      const text =
        calls === 1
          ? ": keepalive\r\n\r\ndata: " + JSON.stringify(event(2)) + "\r\n\r\n"
          : "data: " +
            JSON.stringify(event(2)) +
            "\n\ndata: " +
            JSON.stringify(event(3)) +
            '\n\ndata: {"type":\r\ndata: "resync_required"}\r\n\r\n';
      const bytes = new TextEncoder().encode(text);
      let offset = 0;
      return new Response(
        new ReadableStream({
          pull(controller) {
            if (offset === bytes.length) {
              controller.close();
              return;
            }
            controller.enqueue(bytes.slice(offset, ++offset));
          },
        }),
      );
    },
  });
  const events = [];
  for await (const event of client.events({
    after: 1,
    reconnect: true,
    retryMs: 1,
  }))
    events.push(event);
  assert.deepEqual(
    events.map((e) => e.eventId ?? e.type),
    [2, 3, "resync_required"],
  );
  assert.deepEqual(received, ["1", "2"]);
  client.close();
});

test("request cancellation, timeout, invalid response, close, and no mutation retries", async () => {
  let calls = 0;
  const client = new Haarness({
    baseUrl: "http://localhost:8080",
    timeoutMs: 15,
    fetch: async (url, init) => {
      calls++;
      return await new Promise((resolve, reject) => {
        if (init.signal.aborted) reject(init.signal.reason);
        else
          init.signal.addEventListener(
            "abort",
            () => reject(init.signal.reason),
            { once: true },
          );
      });
    },
  });
  await assert.rejects(
    client.createThread({}),
    (e) => e.name === "TimeoutError",
  );
  assert.equal(calls, 1);
  const abort = new AbortController();
  abort.abort();
  await assert.rejects(
    client.info({ signal: abort.signal }),
    (e) => e.name === "AbortError",
  );
  client.close();
  await assert.rejects(client.info(), (e) => e.name === "AbortError");
  const malformed = new Haarness({
    baseUrl: "http://localhost",
    fetch: async () => new Response("not json"),
  });
  await assert.rejects(malformed.info(), TransportError);
  malformed.close();
  for (const url of [
    "file:///tmp",
    "http://user:secret@localhost",
    "http://localhost/#fragment",
    "http://localhost/?token=secret",
  ])
    assert.throws(() => new Haarness({ baseUrl: url }));
});

test("SSE accepts CR-only frame delimiters at EOF", async () => {
  const client = new Haarness({
    baseUrl: "http://localhost",
    fetch: async () => new Response('data: {"type":"resync_required"}\r\r'),
  });
  const events = [];
  for await (const event of client.events()) events.push(event);
  assert.deepEqual(events, [{ type: "resync_required" }]);
  client.close();
});

const sseEvent = (id) =>
  "data: " +
  JSON.stringify({ schemaVersion: "1", type: "item.created", serverInstanceId: "instance", eventId: id, payload: null }) +
  "\n\n";
/** A stream the test writes to; it never ends by itself, like a connection that died without closing. */
function openStream(init) {
  let controller;
  const body = new ReadableStream({ start: (c) => (controller = c) });
  const encoder = new TextEncoder();
  init?.signal?.addEventListener("abort", () => {
    try { controller.error(init.signal.reason); } catch {}
  });
  return {
    response: new Response(body),
    send: (text) => controller.enqueue(encoder.encode(text)),
  };
}

test("an event stream silent past idleTimeoutMs is lost, and keepalives keep it open", async () => {
  // Silent after it opened: the stream ends with a TransportError.
  let silent;
  const lost = new Haarness({
    baseUrl: "http://localhost:8080",
    fetch: async (_url, init) => {
      silent = openStream(init);
      setTimeout(() => silent.send(sseEvent(1)), 5);
      return silent.response;
    },
  });
  const seen = [];
  const started = Date.now();
  await assert.rejects(async () => {
    for await (const event of lost.events({ idleTimeoutMs: 150 })) seen.push(event.eventId);
  }, (error) => error instanceof TransportError && /sent nothing for 150 ms/.test(error.message));
  assert.deepEqual(seen, [1]);
  assert.ok(Date.now() - started < 1_000);
  lost.close();

  // Keepalive comments every 50 ms keep a 150 ms limit from firing.
  let alive;
  const kept = new Haarness({
    baseUrl: "http://localhost:8080",
    fetch: async (_url, init) => {
      alive = openStream(init);
      let beats = 0;
      const timer = setInterval(() => {
        if (++beats < 10) alive.send(": keepalive\n\n");
        else {
          clearInterval(timer);
          alive.send(sseEvent(7));
        }
      }, 50);
      return alive.response;
    },
  });
  for await (const event of kept.events({ idleTimeoutMs: 150 })) {
    assert.equal(event.eventId, 7);
    break;
  }
  kept.close();
});

test("with reconnect, a silent event stream connects again from its cursor", async () => {
  const cursors = [];
  const client = new Haarness({
    baseUrl: "http://localhost:8080",
    fetch: async (_url, init) => {
      cursors.push(init.headers["Last-Event-ID"]);
      const stream = openStream(init);
      const first = cursors.length === 1;
      setTimeout(() => stream.send(sseEvent(first ? 4 : 5)), 5);
      return stream.response;
    },
  });
  const events = [];
  for await (const event of client.events({ after: 3, reconnect: true, retryMs: 1, idleTimeoutMs: 100 })) {
    events.push(event.eventId);
    if (events.length === 2) break;
  }
  assert.deepEqual(events, [4, 5]);
  assert.deepEqual(cursors, ["3", "4"]);
  client.close();
});

test("a silence limit that fires late, behind a blocked event loop, gives the stream one more window", async () => {
  let fetches = 0;
  const client = new Haarness({
    baseUrl: "http://localhost:8080",
    fetch: async (_url, init) => {
      fetches++;
      const stream = openStream(init);
      setTimeout(() => {
        stream.send(": keepalive\n\n");
        // The loop is blocked well past the limit, as a sleeping machine's
        // would be; the event comes right after.
        setTimeout(() => {
          const until = Date.now() + 400;
          while (Date.now() < until);
          setTimeout(() => stream.send(sseEvent(9)), 10);
        }, 20);
      }, 5);
      return stream.response;
    },
  });
  for await (const event of client.events({ idleTimeoutMs: 100 })) {
    assert.equal(event.eventId, 9);
    break;
  }
  assert.equal(fetches, 1);
  client.close();
});

test("a terminal socket's ping says whether its server still answers, and closes one that does not", async () => {
  const { WebSocketServer } = await import("ws");
  for (const autoPong of [true, false]) {
    const server = new WebSocketServer({ port: 0, autoPong });
    await new Promise((resolve) => server.once("listening", resolve));
    server.on("connection", (socket) => socket.send(JSON.stringify({ type: "cursor", start: 0, end: 0, truncated: false })));
    const client = new Haarness({ baseUrl: `http://127.0.0.1:${server.address().port}` });
    const pty = await client.connectPty("fixture");
    let cursor;
    const arrived = new Promise((resolve) => (cursor = resolve));
    const reading = (async () => {
      for await (const _message of pty) cursor();
    })();
    try {
      // Pinged once its replay is sent, as Desktop pings a terminal.
      await arrived;
      const started = Date.now();
      assert.equal(await pty.ping(200), autoPong);
      if (autoPong) {
        assert.ok(Date.now() - started < 200);
        pty.close();
      }
      // A socket that missed its answer ends, so its reader connects again.
      await reading;
    } finally {
      pty.close();
      client.close();
      for (const socket of server.clients) socket.terminate();
      await new Promise((resolve) => server.close(resolve));
    }
  }
});

test("the time a consumer spends with an event is not the server's silence", async () => {
  const client = new Haarness({
    baseUrl: "http://localhost:8080",
    fetch: async (_url, init) => {
      const stream = openStream(init);
      let id = 0;
      const timer = setInterval(() => {
        stream.send(": keepalive\n\n");
        if (++id % 2 === 0) stream.send(sseEvent(id));
      }, 50);
      init.signal.addEventListener("abort", () => clearInterval(timer));
      return stream.response;
    },
  });
  const seen = [];
  for await (const event of client.events({ idleTimeoutMs: 200 })) {
    seen.push(event.eventId);
    // Longer than the limit with each event: a slow consumer.
    await new Promise((resolve) => setTimeout(resolve, 400));
    if (seen.length === 3) break;
  }
  assert.deepEqual(seen.length, 3);
  client.close();
});

test("a terminal ping counts anything the server sends, fails a socket gone mid-replay, and survives a blocked event loop", async () => {
  const { WebSocketServer } = await import("ws");
  const cursor = JSON.stringify({ type: "cursor", start: 0, end: 0, truncated: false });
  const open = async (onConnection) => {
    const server = new WebSocketServer({ port: 0, autoPong: false });
    await new Promise((resolve) => server.once("listening", resolve));
    server.on("connection", onConnection);
    const client = new Haarness({ baseUrl: `http://127.0.0.1:${server.address().port}` });
    const pty = await client.connectPty("fixture");
    const reading = (async () => {
      for await (const _message of pty);
    })();
    return {
      pty,
      async close() {
        pty.close();
        await reading;
        client.close();
        for (const socket of server.clients) socket.terminate();
        await new Promise((resolve) => server.close(resolve));
      },
    };
  };
  // Each case closes its server whatever it finds, so a failure fails
  // instead of holding the test open.
  const check = async (onConnection, run) => {
    const opened = await open(onConnection);
    try {
      return await run(opened.pty);
    } finally {
      await opened.close();
    }
  };
  // Gone in the middle of its replay: nothing comes, so the ping fails.
  const replay = JSON.stringify({ type: "replay", start: 0, end: 9, truncated: false });
  const dead = await check(
    (socket) => socket.send(replay),
    async (pty) => {
      await new Promise((resolve) => setTimeout(resolve, 50));
      return pty.ping(100);
    },
  );
  assert.equal(dead, false);

  // No pong, but output: the server is there.
  const busy = await check(
    (socket) => {
      socket.send(cursor);
      socket.on("ping", () => setTimeout(() => socket.send(new Uint8Array([65])), 30));
    },
    async (pty) => {
      await new Promise((resolve) => setTimeout(resolve, 50));
      return pty.ping(300);
    },
  );
  assert.equal(busy, true);

  // The pong came while this process was blocked past the limit.
  const blocked = await check(
    (socket) => {
      socket.send(cursor);
      socket.on("ping", (data) => socket.pong(data));
    },
    async (pty) => {
      await new Promise((resolve) => setTimeout(resolve, 50));
      const answered = pty.ping(100);
      const until = Date.now() + 400;
      while (Date.now() < until);
      return answered;
    },
  );
  assert.equal(blocked, true);
});

test("a streamed side question yields its events across chunk boundaries and stops on done", async () => {
  const calls = [];
  const frames = [
    ": connected\n\n",
    'data: {"type":"delta","text":"An "}\n\ndata: {"type":"del',
    'ta","text":"answer"}\r\n\r\n',
    'data: {"type":"done","answer":"An answer","inputTokens":7,"outputTokens":3}\n\n',
    'data: {"type":"delta","text":"never read"}\n\n',
  ];
  const client = new Haarness({
    baseUrl: "http://localhost:8080",
    token: "fixture-token",
    fetch: async (url, init) => {
      calls.push([url, init]);
      return new Response(new ReadableStream({
        start(controller) {
          for (const frame of frames) controller.enqueue(new TextEncoder().encode(frame));
          controller.close();
        },
      }), { headers: { "content-type": "text/event-stream" } });
    },
  });
  const events = [];
  for await (const event of client.btwStream("thread/1", { question: "Why?", history: [{ question: "What?", answer: "That." }] }))
    events.push(event);
  assert.deepEqual(events, [
    { type: "delta", text: "An " },
    { type: "delta", text: "answer" },
    { type: "done", answer: "An answer", inputTokens: 7, outputTokens: 3 },
  ]);
  assert.match(calls[0][0], /\/v1\/threads\/thread%2F1\/btw\/stream$/);
  assert.equal(calls[0][1].method, "POST");
  assert.deepEqual(JSON.parse(calls[0][1].body).history, [{ question: "What?", answer: "That." }]);
  assert.equal(calls.length, 1);
});

test("a streamed side question that ends early, is refused, or goes quiet fails", async () => {
  const respond = (body, status = 200) => new Haarness({
    baseUrl: "http://localhost:8080",
    fetch: async () => typeof body === "string"
      ? new Response(body, { status, headers: { "content-type": status === 200 ? "text/event-stream" : "application/json" } })
      : new Response(new ReadableStream({ start() {} }), { headers: { "content-type": "text/event-stream" } }),
  });
  const drain = async (client, options) => { for await (const _ of client.btwStream("t", { question: "q" }, options)); };
  await assert.rejects(drain(respond('data: {"type":"delta","text":"half"}\n\n')), TransportError);
  await assert.rejects(
    drain(respond(JSON.stringify({ code: "not_found", message: "thread not found", retryable: false }), 404)),
    (error) => error instanceof ApiError && error.status === 404 && error.code === "not_found",
  );
  await assert.rejects(drain(respond(null), { idleTimeoutMs: 50 }), /sent nothing for 50 ms/);
});

test("a method that later took a query still takes its options first", async () => {
  const calls = [];
  const client = new Haarness({
    baseUrl: "http://localhost:8080",
    token: "fixture-token",
    fetch: async (url, init) => {
      calls.push([url, init]);
      return new Response("[]", { headers: { "content-type": "application/json" } });
    },
  });
  const controller = new AbortController();
  await client.mcpServers({ timeoutMs: 5000, signal: controller.signal, headers: { "x-fixture": "1" } });
  await client.mcpServers({ threadId: "chat", agent: "build" });
  await client.mcpServers({ threadId: "chat" }, { headers: { "x-fixture": "2" } });
  await client.mcpServers();
  // A mixed object (only plain JavaScript can pass one) sends its options as options.
  await client.mcpServers({ threadId: "chat", timeoutMs: 5000, headers: { "x-fixture": "3" } });
  assert.equal(calls[4][0], "http://localhost:8080/v1/mcp?threadId=chat");
  assert.equal(new Headers(calls[4][1].headers).get("x-fixture"), "3");
  assert.equal(calls[0][0], "http://localhost:8080/v1/mcp");
  assert.equal(new Headers(calls[0][1].headers).get("x-fixture"), "1");
  assert.equal(calls[1][0], "http://localhost:8080/v1/mcp?threadId=chat&agent=build");
  assert.equal(calls[2][0], "http://localhost:8080/v1/mcp?threadId=chat");
  assert.equal(new Headers(calls[2][1].headers).get("x-fixture"), "2");
  assert.equal(calls[3][0], "http://localhost:8080/v1/mcp");
});
