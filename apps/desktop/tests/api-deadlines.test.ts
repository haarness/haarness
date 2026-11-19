import { test } from "node:test";
import assert from "node:assert/strict";
import { Haarness } from "@haarness/sdk";
import { withDeadline } from "../src/main/api-deadlines";

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
