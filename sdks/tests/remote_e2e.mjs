import assert from "node:assert/strict";
import { HaarnessCloud } from "../typescript/dist/index.js";
const c = new HaarnessCloud({
  baseUrl: process.env.HAARNESS_CLOUD_API_URL,
  token: process.env.HAARNESS_API_KEY,
});
const host = c.remoteHost(process.env.HAARNESS_REMOTE_HOST_ID),
  rt = host.workspace(process.env.HAARNESS_REMOTE_PROJECT_ID);
const id = process.env.HAARNESS_REMOTE_THREAD_ID;
const controller = new AbortController();
try {
  assert(
    (await host.hostProjects()).workspaces.some(
      (p) => p.id === process.env.HAARNESS_REMOTE_PROJECT_ID,
    ),
  );
  const cursor = (await rt.info()).eventCursor;
  const stream = (async () => {
    for await (const event of rt.events({
      signal: controller.signal,
      after: cursor,
    }))
      if (event.threadId === id && event.type === "turn.finished") return event;
  })();
  assert.equal(
    (await rt.chat(id, "TYPESCRIPT_REMOTE_OK")).turn.status,
    "completed",
  );
  await Promise.race([
    stream,
    new Promise((_, reject) =>
      setTimeout(() => reject(Error("No remote SSE event")), 10000).unref(),
    ),
  ]);
  controller.abort();
  assert(
    (await rt.listFiles({ hidden: true })).entries.some(
      (f) => f.path === ".hidden-proof",
    ),
  );
  const pty = await rt.createPty({
    title: "TS remote proof",
    cols: 80,
    rows: 24,
  });
  const socket = await rt.connectPty(pty.id);
  let text = "";
  try {
    socket.send("printf 'TS_REMOTE_PTY_OK\\n'\n");
    for await (const chunk of socket) {
      if (chunk instanceof Uint8Array) text += new TextDecoder().decode(chunk);
      if (text.includes("\r\nTS_REMOTE_PTY_OK\r\n")) break;
    }
  } finally {
    socket.close();
    await rt.deletePty(pty.id);
  }
  console.log("PASS: real TypeScript remote SSE, chat, files and terminal");
} finally {
  controller.abort();
  rt.close();
  host.close();
  c.close();
}
