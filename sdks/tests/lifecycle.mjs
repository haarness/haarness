import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { LocalServer } from "../typescript/dist/index.js";
const [executable, workspace, dataDir] = process.argv.slice(2);
const options = {
  executable,
  workspace,
  dataDir,
  trustProject: true,
  env: { HAARNESS_TEST_PROVIDER: "1" },
};
const server = await LocalServer.start(options);
try {
  const client = server.client,
    thread = await client.createThread({ provider: "echo", model: "echo-1" });
  assert.equal(
    (await client.chat(thread.id, "Owned SDK")).text,
    "Echo: Owned SDK",
  );
  // A second start in the same folder is a front for its server: the same
  // chats at its own address, and closing it stops only the front.
  const front = await LocalServer.start({ ...options, startupTimeoutMs: 5000 });
  const frontUrl = front.client.baseUrl;
  try {
    assert.notEqual(frontUrl, client.baseUrl);
    assert.equal(
      (await front.client.chat(thread.id, "Through the front")).text,
      "Echo: Through the front",
    );
  } finally {
    await front.close();
  }
  assert.ok(front.child.exitCode !== null || front.child.signalCode !== null);
  await assert.rejects(fetch(`${frontUrl}/healthz`));
  assert.equal((await client.info()).name, "haarness");
  const cursor = (await client.info()).eventCursor,
    abort = new AbortController();
  const observer = (async () => {
    for await (const event of client.events({
      after: cursor,
      signal: abort.signal,
    })) {
      if (
        event.type === "turn.finished" &&
        event.payload?.status === "completed"
      )
        return;
    }
  })();
  const turn = await client.startTurn(thread.id, {
    requestId: randomUUID(),
    text: "::mdslow",
  });
  const cancel = new AbortController();
  setTimeout(() => cancel.abort(), 20);
  await assert.rejects(
    client.waitTurn(turn.id, { timeout_ms: 10000 }, { signal: cancel.signal }),
    (e) => e.name === "AbortError",
  );
  const timeout = setTimeout(() => abort.abort(), 10000);
  try {
    await observer;
  } finally {
    clearTimeout(timeout);
    abort.abort();
  }
  assert.equal((await client.getTurn(turn.id)).status, "completed");
} finally {
  await server.close();
  await server.close();
}
console.log("TypeScript owned server and live cancellation passed");
