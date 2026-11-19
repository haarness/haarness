import { test } from "node:test";
import assert from "node:assert/strict";
import { Haarness } from "@haarness/sdk";
import { cutOnTheWay, withDeadline } from "../src/main/api-deadlines";

test("a compaction waits as long as the phones and the web do, other calls keep the client's deadline", () => {
  assert.deepEqual(withDeadline("compact", ["chat", {}]), ["chat", {}, { timeoutMs: 310_000 }]);
  // Options a caller gave are kept beside it.
  assert.deepEqual(withDeadline("compact", ["chat", {}, { headers: { a: "b" } }]), [
    "chat",
    {},
    { headers: { a: "b" }, timeoutMs: 310_000 },
  ]);
  const plain = ["chat", { text: "hi" }];
  assert.equal(withDeadline("startTurn", plain), plain);
  assert.equal(withDeadline("toString", plain), plain);
});

test("the longer deadline reaches the request: a summary slower than the client's own deadline still answers", async () => {
  const item = { id: "item", threadId: "chat", sequence: 3, data: { type: "compaction" } };
  // The client gives up after 20 ms; this server answers after 80 ms.
  const client = new Haarness({
    baseUrl: "http://127.0.0.1:1",
    timeoutMs: 20,
    fetch: (async (_url: string, init: RequestInit) => {
      await new Promise((resolve, reject) => {
        const timer = setTimeout(resolve, 80);
        init.signal?.addEventListener("abort", () => {
          clearTimeout(timer);
          reject(init.signal?.reason);
        });
      });
      return new Response(JSON.stringify(item), { status: 200, headers: { "Content-Type": "application/json" } });
    }) as typeof fetch,
  });
  const call = (args: unknown[]) =>
    (client.compact as (...args: unknown[]) => Promise<unknown>).apply(client, args);
  await assert.rejects(call(["chat", {}]), /timed out/i);
  assert.deepEqual(await call(withDeadline("compact", ["chat", {}])), item);
});

test("a compaction a relay or proxy cut on its way is told from one the server refused", async () => {
  const answering = (status: number, body: string, type = "application/json") =>
    new Haarness({
      baseUrl: "http://127.0.0.1:1",
      fetch: (async () => new Response(body, { status, headers: { "Content-Type": type } })) as typeof fetch,
    });
  const failure = (client: Haarness) => client.compact("chat", {}).then(() => undefined, (error: unknown) => error);
  // A computer on the release that finishes cut summaries, reached through Cloud.
  const relayed = { relayed: true, url: "https://haarness.ai/api/v1/remote-hosts/h/runtime", version: "0.1.24" };
  const json = (status: number, body: object) => answering(status, JSON.stringify(body));
  // Cloudflare's limit, as it answers it, and a gateway's.
  assert.equal(cutOnTheWay(await failure(answering(524, "<html>A timeout occurred</html>", "text/html")), relayed), true);
  assert.equal(cutOnTheWay(await failure(answering(504, "Gateway Timeout", "text/plain")), relayed), true);
  // Cloud lost the computer's connection while the request was out.
  assert.equal(
    cutOnTheWay(await failure(json(409, { code: "remote_offline", message: "This computer is offline or its connection changed" })), relayed),
    true,
  );
  // Another compaction under way: waited for whatever the release.
  const underWay = await failure(json(409, { code: "conflict", message: "conflict: this chat is already being compacted" }));
  assert.equal(cutOnTheWay(underWay, relayed), true);
  assert.equal(cutOnTheWay(underWay, { relayed: false, url: "http://127.0.0.1:4096", version: "0.1.0" }), true);
  // The computer's runtime failed the request, or the model the summary.
  assert.equal(cutOnTheWay(await failure(json(502, { code: "bad_gateway", message: "Your computer couldn't answer." })), relayed), false);
  assert.equal(cutOnTheWay(await failure(json(502, { code: "provider_error", message: "provider error: overloaded" })), relayed), false);
  assert.equal(cutOnTheWay(await failure(json(503, { code: "provider_unavailable", message: "Ollama isn't running" })), relayed), false);
  // An older computer's summary died with its request.
  assert.equal(
    cutOnTheWay(await failure(answering(524, "<html>A timeout occurred</html>", "text/html")), { ...relayed, version: "0.1.23" }),
    false,
  );
});

test("a dropped connection is a cut only on the way to another computer, never a failure to connect", async () => {
  const dropping = (code: string) =>
    new Haarness({
      baseUrl: "http://127.0.0.1:1",
      fetch: (async () => {
        throw new TypeError("fetch failed", { cause: Object.assign(new Error(code), { code }) });
      }) as typeof fetch,
    });
  const failure = (client: Haarness) => client.compact("chat", {}).then(() => undefined, (error: unknown) => error);
  const reset = await failure(dropping("ECONNRESET"));
  const refused = await failure(dropping("ECONNREFUSED"));
  // This computer's own server that dropped the request is gone, its summary too.
  assert.equal(cutOnTheWay(reset, { relayed: false, url: "http://127.0.0.1:4096", version: "0.1.24" }), false);
  assert.equal(cutOnTheWay(reset, { relayed: false, url: "http://localhost:4096", version: "0.1.24" }), false);
  // Through Cloud's relay, or to a server on another computer, the answer was lost on the way.
  assert.equal(cutOnTheWay(reset, { relayed: true, url: "http://127.0.0.1:19080/api", version: "0.1.24" }), true);
  assert.equal(cutOnTheWay(reset, { relayed: false, url: "https://build-box.example.test:4096", version: "0.1.24" }), true);
  // A request that never got there.
  assert.equal(cutOnTheWay(refused, { relayed: true, url: "https://haarness.ai/api", version: "0.1.24" }), false);
});
