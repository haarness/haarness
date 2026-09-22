# Haarness developer SDKs

Unpublished TypeScript and Python clients for the complete Haarness server API.
The TypeScript package is private and has a publish guard. Python wheels carry
`Private :: Do Not Upload`, which public PyPI rejects. No publishing workflow is
included. Both packages can be built, packed and installed locally.

- **TypeScript:** Node.js 22+, promises, generated request/response types, fetch,
  AbortSignal cancellation, SSE async iterators, authenticated PTY WebSockets.
- **Python:** Python 3.10+, synchronous `Haarness` and asynchronous
  `AsyncHaarness`, generated TypedDicts/unions, HTTPX, SSE iterators, PTYs.
- **All 103 registered operations:** sessions/turns, commands, goals, loops,
  background work, permissions/questions, MCP, plugins, skills,
  workspaces, Git review, PTYs, workflows, profile archives, analytics and shutdown.
- Explicit owned local-server helpers, remote-safe attachments, and `chat()`.

[Every operation and method name](OPERATIONS.md) · [API schema](openapi.json)

## Local installation

```sh
cd sdks/typescript
npm ci
npm run build
npm pack
# From a consuming project:
npm install /absolute/path/to/haarness/sdks/typescript
```

```sh
python -m pip install /absolute/path/to/haarness/sdks/python
# Or install an editable copy during development:
python -m pip install -e /absolute/path/to/haarness/sdks/python
```

The package names are `@haarness/sdk` and `haarness-sdk`; the Python import is
`haarness_sdk`. Credentials are supplied explicitly or through
`HAARNESS_SERVER_TOKEN`. Clients connect to a server you specify; constructing
a client never starts or replaces a daemon.

## Connect and chat

Start the server with `haarness serve --port 4096` and obtain its bearer token
from your private runtime discovery file, or set `HAARNESS_SERVER_TOKEN` before
starting it. The URL is the server root, not `/v1`. A reverse-proxy base path is
supported. Network servers should use a private network or an HTTPS tunnel.

```ts
import { Haarness } from "@haarness/sdk";

const client = new Haarness({
  baseUrl: "http://127.0.0.1:4096",
  token: process.env.HAARNESS_SERVER_TOKEN,
});
try {
  const thread = await client.createThread({ title: "Review" });
  const result = await client.chat(thread.id, "Inspect this repository");
  console.log(result.turn.status, result.text);
} finally {
  client.close();
}
```

```python
from haarness_sdk import Haarness

with Haarness("http://127.0.0.1:4096") as client:
    thread = client.create_thread({"title": "Review"})
    result = client.chat(thread["id"], "Inspect this repository")
    print(result["turn"]["status"], result["text"])
```

```python
import asyncio
from haarness_sdk import AsyncHaarness

async def main():
    async with AsyncHaarness("http://127.0.0.1:4096") as client:
        thread = await client.create_thread({"title": "Review"})
        result = await client.chat(thread["id"], "Inspect this repository")
        print(result["text"])

asyncio.run(main())
```

All request/response keys preserve the server's JSON spelling in both languages.
Python method names use snake_case; TypeScript uses camelCase. Path arguments
come first, then a request body when applicable, then a query object. Python
transport options are keyword-only. JSON `null` and omitted fields remain
separate, including settings-reset requests.

`chat()` starts and waits for a turn, then returns `{turn, items, text}` for that
turn only. Inspect `turn.status` for completion/failure/interruption. A pending
permission or question must be answered by another observer while chat waits;
SDKs never automatically approve prompts. `autoApprove` is an explicit server
request field for authorized noninteractive work.

## Streaming, approvals and cancellation

```ts
const before = await client.info();
const abort = new AbortController();
try {
  for await (const event of client.events({
    after: before.eventCursor,
    reconnect: true,
    signal: abort.signal,
  })) {
    if (event.type === "resync_required") break;
    console.log(event);
  }
} finally {
  abort.abort();
}
```

```python
from contextlib import closing

before = client.info()
with closing(client.events(after=before["eventCursor"], reconnect=True)) as events:
    for event in events:
        if event["type"] == "resync_required":
            break
        print(event)
```

Async Python uses `async for` and `contextlib.aclosing`. Cancel its task to abort
an in-flight request or stream. TypeScript accepts `{signal, timeoutMs}` as the
last argument of every endpoint method. Python accepts
`options={"timeout": seconds}`. Default HTTP timeout is 30 seconds; `chat()`
allows about five minutes by default. For longer waits or workspace operations,
set a larger transport timeout explicitly. `wait_turn`'s **query** field is
`timeout_ms`; workflow wait uses `timeoutMs`, matching the server.

SSE is one connection unless `reconnect` is enabled. Reconnection sends the last
delivered event ID and suppresses duplicate event IDs. A `resync_required` marker
or changed server instance ends iteration: capture a new `info().eventCursor`,
reload REST snapshots, and open a new stream with that cursor. Events cover the
whole server; filter by `threadId`/`rootThreadId`/`turnId` in your consumer.
Disconnecting a stream, closing a client, or cancelling an HTTP wait **does not
cancel server-owned work**. Use `interruptTurn`/`interrupt_turn` or
`interruptThreadTree`/`interrupt_thread_tree` for that action.

Use `listPermissions`/`list_permissions` and `replyPermission`/`reply_permission`
for approvals, and `listQuestions`/`list_questions` and
`replyQuestion`/`reply_question` for user answers. No request is automatically
retried, including a retryable error. For a deliberate turn retry, reuse the
same `requestId`; generating a new one requests a new turn.

## Workflows, commands and other resources

```ts
const command = await client.executeCommand(thread.id, {
  name: "review", arguments: "src", wait: true,
});
const run = await client.startWorkflow({ name: "audit", threadId: thread.id });
const finished = await client.waitWorkflowRun(run.id, { timeoutMs: 120_000 }, {
  timeoutMs: 125_000,
});
```

```python
command = client.execute_command(thread["id"], {
    "name": "review", "arguments": "src", "wait": True,
})
run = client.start_workflow({"name": "audit", "threadId": thread["id"]})
finished = client.wait_workflow_run(run["id"], {"timeoutMs": 120_000},
                                    options={"timeout": 125})
```

Names in these examples must exist in your `commands()`/`workflows()` catalog.
Command results are tagged as `turn`, `workflow_run` or `text`. Server-executable
commands are distinct from TUI-only commands such as `/exit`. CLI-only
installation/authentication/native-scheduler administration has no server API;
the SDK does not invent endpoints or silently shell out for those commands.

All other resources have direct typed methods listed in [OPERATIONS.md](OPERATIONS.md).
Examples: `updateTool("read", {enabled: null})`, `setGoal(id, {action: "pause"})`,
and `exportSessionArchive(id, {sanitize: true})`. Corresponding Python methods use
snake_case. Destructive workspace methods preserve the server's explicit
`confirm`/discard flags and conflict checks.

## Attachments, binary profiles, PTYs

`attachments(paths)` reads local PDF/image/audio/video files and creates data
URLs for a remote server. It checks supported extensions and the 20 MiB
aggregate limit. Provider-specific decoding and media compatibility are checked
by the server. TypeScript awaits this helper; Python calls it synchronously.
Pass the resulting array as a turn/chat `attachments` field.

`uploadProfile`/`upload_profile` accepts ZIP bytes (`Uint8Array`/`bytes`).
`downloadProfile`/`download_profile` returns identical archive bytes. Uploads
store a profile; they do not restore it into a running server's configuration.

```ts
const pty = await client.createPty({ cols: 100, rows: 30 });
const socket = await client.connectPty(pty.id);
try {
  socket.send("pwd\n");
  for await (const message of socket) {
    if (message instanceof Uint8Array) process.stdout.write(message);
    else console.log(message); // replay/cursor control frame
  }
} finally { socket.close(); }
```

Python: `with client.connect_pty(id) as socket`, then `socket.send`,
`socket.recv(timeout=5)` or iteration. Async Python awaits `connect_pty`,
`send`, `recv`, `close`, and supports `async with`/`async for`.
Save the last consumed cursor control frame's `end` value, then reconnect with
that cursor (`{cursor}` in TypeScript, `cursor=...` in Python). A truncated
replay marker means older output is no longer available. `socket.cursor`
reports the latest consumed acknowledgement. Closing a socket detaches;
`deletePty`/`delete_pty` terminates the process. PTYs do not survive server restart.

## Own a local server explicitly

```ts
import { LocalServer } from "@haarness/sdk";
const server = await LocalServer.start({
  workspace: "/path/to/project", executable: "/path/to/haarness",
});
try { console.log(await server.client.info()); }
finally { await server.close(); }
```

```python
from haarness_sdk import LocalServer, AsyncLocalServer
with LocalServer.start("/path/to/project", executable="/path/to/haarness") as server:
    print(server.client.info())
# Async: async with await AsyncLocalServer.start(...) as server:
```

These helpers spawn a foreground child on an OS-assigned loopback port with a
fresh random bearer token. They verify the server identity/schema, retain
ownership, and stop only that child. They fail if a workspace server already
owns the lease; they never replace it. `dataDir`/`data_dir` selects an isolated
installation. Project configuration loads by default. Set `disableProjectConfig: true`
(TypeScript) or `disable_project_config=True` (Python) to load only global
configuration. The environment opt-out `HAARNESS_DISABLE_PROJECT_CONFIG=1`
also applies. Legacy `trustProject: false` / `trust_project=False` remain
explicit opt-outs; omitted values use the new default. The legacy server
info field `projectTrusted` reports whether project configuration is enabled.
Automatic approval remains controlled separately by `yolo`.

## Errors

`ApiError` exposes `status`, `code`, `retryable`, and `body`. Framework/plain-text
and profile-catalog errors also become `ApiError`. Network/decoding errors are
`TransportError`; TypeScript cancellation/timeouts use AbortError/TimeoutError,
and async Python preserves `CancelledError`. Authentication is never placed in
URLs, HTTP redirects are not followed, and PTY authentication uses headers.

## Generation and verification

```sh
# Build the real server first. Export schemas from a private owned instance.
cargo build -p haarness-cli
python3 sdks/tests/server.py
python3 sdks/generate.py

# Full reproducible gate: installs locked SDK development dependencies locally.
./scripts/test-sdks.sh
```

`schema.json` is the unmodified live server schema snapshot. `inventory.py`
extracts every registered method/path and handler signature from Rust.
`generate.py` combines those with explicit contracts for server-local wire
shapes missing from the protocol schemas. It emits the complete OpenAPI spec,
operation inventory, TypeScript types/methods, and Python types/sync/async
methods. `--check` detects source/generated drift without modifying files.

The shared E2E scenario runs through each actual SDK and requires successful
coverage of **all 103 registered operations**. Every JSON response is checked
against its declared schema. Assertions check resulting files, history,
idempotency, approvals, questions, workflow/background lifecycles, MCP
stdio tools/resources/prompts, PTY IO/replay, binary profile round-trips,
archives, commands and shutdown. Further tests cover owned server startup and
cleanup, live event streaming, cancellation, timeout, SSE parsing/reconnect,
URL encoding, errors, redirects and the unpublished package artifacts.
The provider is Haarness's deterministic echo adapter and MCP uses a stdio
fixture; these tests do not contact paid model services.
