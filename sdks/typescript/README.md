# Haarness TypeScript SDK

Unpublished, typed Node.js 22+ client for all 103 Haarness server operations,
SSE events, PTY WebSockets, attachments and explicitly owned local servers.

```sh
npm ci
npm run build
npm pack
```

Install this directory or its packed tarball into a consuming project. Publishing
is disabled with `private: true` and a prepublish guard.

```ts
import { Haarness, LocalServer, ApiError, attachments } from "@haarness/sdk";

const client = new Haarness({
  baseUrl: "http://127.0.0.1:4096",
  token: process.env.HAARNESS_SERVER_TOKEN,
});
try {
  const thread = await client.createThread({ title: "Review" });
  const reply = await client.chat(thread.id, "Inspect this repository");
  console.log(reply.turn.status, reply.text);
} finally { client.close(); }
```

Every method has generated input/output types. Arguments are path values, then
body (if any), query (if any), and transport options `{signal, timeoutMs}`.
Default timeout is 30 seconds; `chat()` defaults to about five minutes. Requests
are never automatically retried. Reuse a requestId for an intentional idempotent
turn retry. ApiError exposes status/code/retryable/body; network and decoding
failures use TransportError. Cancellation preserves AbortError/TimeoutError.

`events({after, reconnect, signal})` is an async iterator. Capture
`info().eventCursor` before hydrating REST snapshots. A `resync_required` event
ends iteration; rehydrate and reconnect with a fresh cursor. Closing a client
or event stream does not cancel server work. Use interruptTurn or
interruptThreadTree explicitly. Handle approvals/questions concurrently through
listPermissions/replyPermission and listQuestions/replyQuestion; no approvals
are automatic.

`connectPty(id, {cursor})` returns an async-iterable socket with send/close and
cursor. Messages are binary output or replay/cursor control objects. Save
the last consumed cursor control's `end` value for replay. Closing detaches;
deletePty terminates the process.

`attachments(paths)` asynchronously returns remote-safe media data URLs, bounded
to 20 MiB total. Profile upload/download accepts/returns Uint8Array ZIP bytes.

`await LocalServer.start({workspace, executable, dataDir})` returns an owned
server with `.client`; `await server.close()` stops only that child. Existing
workspace servers are never replaced. Project trust and automatic approval
remain off unless explicitly requested.

See [OPERATIONS.md](OPERATIONS.md) for every method. Server command execution is
available through executeCommand; TUI-only and CLI-only commands are not HTTP
endpoints. The repository's `sdks/README.md` describes generation and the full
real-server E2E gate (`scripts/test-sdks.sh`).

## Optional server authentication

```ts
const open = new Haarness({ baseUrl: "http://localhost:4096", auth: { type: "none" } });
const protectedServer = new Haarness({
  baseUrl: "https://haarness.example.com",
  username: "haarness", // default when omitted
  password: process.env.HAARNESS_SERVER_PASSWORD,
});
```

Explicit `auth: { type: "none" }` ignores an ambient legacy token. Password
credentials apply to HTTP requests, event streams and terminal WebSockets.
The existing `token` option remains supported.
