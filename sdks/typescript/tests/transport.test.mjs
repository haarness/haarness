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
