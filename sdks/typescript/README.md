# Haarness TypeScript SDK

Unpublished, typed Node.js 22+ client for every Haarness server and Cloud HTTP operation, SSE
events, PTY WebSockets, attachments and explicitly owned local servers. This README is for
TypeScript developers using the package; the repository's `sdks/README.md` has the
full guide and how the SDKs are generated and tested.

> **User guide:** [TypeScript and Python SDKs](https://haarness.ai/docs/server/sdks).

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
body (if any), query (if any), and transport options `{signal, timeoutMs, headers}`.
Default timeout is 30 seconds; `chat()` defaults to about five minutes. Requests
are never automatically retried. Reuse a requestId for an intentional idempotent
turn retry. ApiError exposes status/code/retryable/body; network and decoding
failures use TransportError. Cancellation preserves AbortError/TimeoutError.

`events({after, reconnect, signal, idleTimeoutMs})` is an async iterator. Capture
`info().eventCursor` before hydrating REST snapshots. A `resync_required` event
ends iteration; rehydrate and reconnect with a fresh cursor. With
`idleTimeoutMs`, a stream that sends nothing for that long (the server sends a
keepalive every 15 s, so 45000 suits) counts as lost: it connects again from its
cursor with `reconnect`, or ends with a TransportError; a wait that ran far past
its time while the machine slept first gets one more window. Closing a client
or event stream does not cancel server work. Use interruptTurn or
interruptThreadTree explicitly. Handle approvals/questions concurrently through
listPermissions/replyPermission and listQuestions/replyQuestion; no approvals
are automatic.

`btwStream(id, {question})` asks a side question and yields its answer as it is
written (`delta`, `text`, then `done` or `error`), failing after
`idleTimeoutMs` (default 45000) of silence; `btw()` answers in one piece.
`workspace(id)` returns a client bound to one project behind a host gateway
(`hostProjects()` lists them).

`connectPty(id, {cursor})` returns an async-iterable socket with send/close,
`ping(timeoutMs)` (false, and the socket closed, when the server does not
answer in time: a connection that died without closing) and cursor. Messages are binary output or replay/cursor control objects. Save
the last consumed cursor control's `end` value for replay. Closing detaches;
deletePty terminates the process.

`attachments(paths)` asynchronously returns remote-safe media data URLs, bounded
to 20 MiB total. Profile upload/download accepts/returns Uint8Array ZIP bytes.

`await LocalServer.start({workspace, executable, dataDir})` returns an owned
server with `.client`; `await server.close()` stops only that child. Existing
workspace servers are never replaced: in a folder whose server already runs, the
child is a front for it at its own address. Project configuration loads by default; automatic tool approval remains a
separate explicit choice.

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

## Cloud

`HaarnessCloud` connects to the Cloud account API with a scoped API key
(`HAARNESS_API_KEY`, which wins) or an account token (`HAARNESS_CLOUD_TOKEN`),
separately from runtime tokens. With no `baseUrl` it uses `HAARNESS_CLOUD_API_URL`,
else `https://haarness.ai`, as the CLI does. Select `workspace(workspaceId)`, or
`newWorkspace({title, initialPrompt})`, whose handle keeps the response in `created`;
then `workspace.chats` for root chat handles with independent history and shared files.
Connect the workspace to access the complete native API; it uses the client's own
`fetch` and `timeoutMs`. Workspaces sleep (`stop`) and wake (`start`) and have no
archive. Cloud account updates use authenticated WebSockets; chat/turn updates use
runtime SSE. A failed `LocalServer.start` reports the reason the server printed.

Cloud schedule CRUD, pause/resume, run-now and history are generated methods.
Schedules target a workspace and chat and run on the Cloud worker with the laptop
off. They differ from the device-local `schedule` tool. Closing a client only
detaches; explicitly stop or delete workspaces to release compute. The repository's
`sdks/README.md` contains complete examples, authentication and lifecycle details,
and `CLOUD-OPERATIONS.md` inventories every Cloud method.

### API key capabilities and lifecycle

Use the same `hck_` API key as the Cloud SDK `token` and an OpenAI-compatible model client's
`api_key`, with the returned `routerUrl` as its base URL. Cloud SDKs also read `HAARNESS_API_KEY`
(before the legacy `HAARNESS_CLOUD_TOKEN`); explicit `token` takes precedence. Keys cannot
manage other keys: creation, update, rotation and revocation require an owner/admin sign-in.
The generated API includes key list/create/update/rotate/revoke and self-context methods.
Grant `models:read` for discovery, `models:invoke` for inference, and appropriate `workspaces`,
`schedules`, `marketplace` or `billing:read` permissions for Cloud calls. An attach capability
authorizes full runtime chats, files, terminal and settings in workspaces in the key's account.
Expired or revoked keys fail; SDKs never implicitly replace them with OAuth credentials.
