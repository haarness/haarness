import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { createServer } from "node:http";
import { WebSocketServer } from "ws";
import { HaarnessCloud, ApiError } from "../dist/index.js";

const operations = JSON.parse(
  await readFile(
    new URL("../../cloud-operations.json", import.meta.url),
    "utf8",
  ),
);
const camel = (name) => name.replace(/_([a-z])/g, (_, c) => c.toUpperCase());
test("billing account queries retain the original request-options call shape", async () => {
  const seen = [];
  const client = new HaarnessCloud({baseUrl:"http://localhost:12345", fetch:async (url, init) => {
    seen.push({url:new URL(url), headers:init.headers});
    return new Response("{}");
  }});
  for (const method of ["billingPortal", "cancelSubscription", "resumeSubscription", "patchWallet"]) {
    const body = method === "patchWallet" ? [{overageOptIn:true}] : [];
    await client[method](...body, {headers:{"x-proof":"legacy"}});
    assert.equal(seen.at(-1).headers["x-proof"], "legacy");
    assert.equal(seen.at(-1).url.search, "");
    await client[method](...body, {accountId:"team"}, {headers:{"x-proof":"scoped"}});
    assert.equal(seen.at(-1).headers["x-proof"], "scoped");
    assert.equal(seen.at(-1).url.searchParams.get("accountId"), "team");
  }
  client.close();
});
test("every documented Cloud HTTP operation preserves method, path, auth and wire encoding", async () => {
  let current;
  let calls = 0;
  const client = new HaarnessCloud({
    baseUrl: "http://localhost:12345",
    token: "account-fixture",
    fetch: async (url, init) => {
      calls++;
      assert.equal(init.method, current.method);
      assert.equal(
        new URL(url).pathname,
        current.path.replace(/\{[^}]+\}/g, "opaque%20id"),
      );
      assert.equal(init.headers.Authorization, "Bearer account-fixture");
      assert.equal(init.headers["Idempotency-Key"], "fixture-key");
      if (current.binaryBody)
        assert.deepEqual(init.body, new Uint8Array([0, 255]));
      else if (current.body) {
        assert.equal(init.headers["Content-Type"], current.contentType);
        if (current.contentType === "application/x-www-form-urlencoded")
          assert.equal(init.body, "key=space+value");
        else if (current.contentType === "text/plain")
          assert.equal(init.body, "literal text");
        else assert.equal(init.body, JSON.stringify({ key: "space value" }));
      }
      if (current.redirectResponse) {
        assert.equal(init.redirect, "manual");
        return new Response(null, {
          status: 302,
          headers: { Location: "/authorized" },
        });
      }
      if (current.binaryResponse) return new Response(new Uint8Array([0, 255]));
      if (current.textResponse) return new Response("<html>fixture</html>");
      return new Response(JSON.stringify({ ok: true }));
    },
  });
  for (current of operations.filter((op) => !op.stream)) {
    const args = current.pathParams.map(() => "opaque id");
    if (current.body || current.binaryBody)
      args.push(
        current.binaryBody
          ? new Uint8Array([0, 255])
          : current.contentType === "text/plain"
            ? "literal text"
            : { key: "space value" },
      );
    if (current.query) args.push({ limit: 2 });
    args.push({ headers: { "Idempotency-Key": "fixture-key" } });
    const result = await client[camel(current.name)](...args);
    if (current.redirectResponse)
      assert.deepEqual(result, { status: 302, location: "/authorized" });
  }
  assert.equal(calls, operations.filter((op) => !op.stream).length);
  client.close();
});

test("Cloud WebSocket reconnect renews auth, emits resync and closes promptly", async () => {
  const server = createServer();
  const sockets = new WebSocketServer({ server });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const seen = [];
  sockets.on("connection", (socket, request) => {
    seen.push(request.headers.authorization);
    socket.send(
      JSON.stringify({
        type: "ready",
        scope: "account",
        resync: true,
        entities: ["schedule"],
      }),
    );
    if (seen.length === 1) socket.close();
  });
  const client = new HaarnessCloud({
    baseUrl: `http://127.0.0.1:${server.address().port}`,
    resolveAuth: async () => ({
      type: "bearer",
      token: `renewed-${seen.length + 1}`,
    }),
  });
  try {
    let ready = 0,
      resync = 0;
    for await (const event of client.events({ reconnect: true, retryMs: 1 })) {
      if (event.type === "resync_required") resync++;
      if (event.type === "ready" && ++ready === 2) break;
    }
    assert.deepEqual(seen, ["Bearer renewed-1", "Bearer renewed-2"]);
    assert.equal(resync, 1);
  } finally {
    client.close();
    for (const socket of sockets.clients) socket.terminate();
    await new Promise((resolve) => sockets.close(resolve));
    await new Promise((resolve) => server.close(resolve));
  }
});

test("Cloud errors preserve details and never use the local server credential", async () => {
  const old = process.env.HAARNESS_SERVER_TOKEN;
  process.env.HAARNESS_SERVER_TOKEN = "local-secret";
  const client = new HaarnessCloud({
    baseUrl: "http://localhost:12345",
    token: "",
    fetch: async (url, init) => {
      assert.equal(init.headers.Authorization, undefined);
      return new Response(
        JSON.stringify({
          error: {
            code: "insufficient_credits",
            message: "Add credits",
            retryable: false,
          },
        }),
        { status: 402 },
      );
    },
  });
  try {
    await assert.rejects(
      client.getWallet(),
      (e) =>
        e instanceof ApiError &&
        e.code === "insufficient_credits" &&
        e.message.includes("Add credits"),
    );
  } finally {
    client.close();
    if (old === undefined) delete process.env.HAARNESS_SERVER_TOKEN;
    else process.env.HAARNESS_SERVER_TOKEN = old;
  }
});

test("runtime attachment has one bearer and renews on a flat generation error", async () => {
  let tickets = 0,
    restarted = false;
  const auth = [];
  const server = createServer((req, res) => {
    if (req.url === "/api/v1/sessions/session/attach") {
      assert.equal(req.headers.authorization, "Bearer account-fixture");
      tickets++;
      res.setHeader("Content-Type", "application/json");
      res.end(
        JSON.stringify({
          gatewayUrl: `http://127.0.0.1:${server.address().port}/runtime`,
          ticket: `ticket-${tickets}`,
          expiresAt: new Date(Date.now() + 3600000).toISOString(),
        }),
      );
      return;
    }
    if (req.url === "/runtime/v1/info") {
      auth.push(req.headers.authorization);
      if (restarted && req.headers.authorization === "Bearer ticket-1") {
        res.writeHead(400, { "Content-Type": "application/json" });
        res.end(
          JSON.stringify({
            code: "remote_project",
            message: "workspace restarted; re-attach",
          }),
        );
        return;
      }
      res.setHeader("Content-Type", "application/json");
      res.end(JSON.stringify({ name: "haarness" }));
      return;
    }
    res.writeHead(404);
    res.end();
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const client = new HaarnessCloud({
    baseUrl: `http://127.0.0.1:${server.address().port}`,
    token: "account-fixture",
  });
  const session = client.session("session");
  try {
    const runtime = await session.connect();
    await runtime.info();
    restarted = true;
    await runtime.info();
    assert.deepEqual(auth, [
      "Bearer ticket-1",
      "Bearer ticket-1",
      "Bearer ticket-2",
    ]);
    assert.equal(tickets, 2);
  } finally {
    await session.close();
    client.close();
    await new Promise((resolve) => server.close(resolve));
  }
});
