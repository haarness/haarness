# Haarness developer SDKs

Unpublished TypeScript and Python clients for the complete Haarness server and Cloud APIs.
The TypeScript package is private and has a publish guard. Python wheels carry
`Private :: Do Not Upload`, which public PyPI rejects. No publishing workflow is
included. Both packages can be built, packed and installed locally.

- **TypeScript:** Node.js 22+, promises, generated request/response types, fetch,
  AbortSignal cancellation, SSE async iterators, authenticated PTY WebSockets.
- **Python:** Python 3.10+, synchronous `Haarness` and asynchronous
  `AsyncHaarness`, generated TypedDicts/unions, HTTPX, SSE iterators, PTYs.
- **All 123 registered native operations:** chats/turns, commands, goals, loops,
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

## Cloud accounts, sessions and chats

`HaarnessCloud` (TypeScript/Python) and `AsyncHaarnessCloud` (Python) expose all
176 documented Cloud operations, including account events, host relay sockets, and scoped runtime adapters.
This includes identity/OAuth, plans and wallets, billing, API keys and model usage,
marketplace, devices, reusable agents, session lifecycle and schedules.
See [Cloud methods](CLOUD-OPERATIONS.md) and [Cloud schema](cloud-openapi.json).
Access remains governed by the server's role and scope checks; administrative
methods in the inventory do not grant administrative privileges.

Cloud marketplace resources are public or private to a Haarness account. Members
can read private resources; builder, admin and owner roles can submit them. The
Cloud SDK supports ZIP upload, Git/npm/MCP submissions, validation status, catalog
search, versions, install manifests and authenticated binary artifact downloads.

Runtime clients share the CLI/Desktop installer through `marketplace()` and
`capabilities()` in TypeScript, sync Python and async Python:

```ts
await runtime.marketplace({ action: "install", id: resourceId, scope: "local" });
await runtime.capabilities({ action: "disable", kind: "skill", name: "review", scope: "local" });
await runtime.capabilities({ action: "enable", kind: "skill", name: "review", scope: "local" });
```

Changes persist and apply when the runtime is idle. All chats in a Cloud session
share these capabilities. Disabling preserves files. Local/global installation
scope is separate from catalog privacy. `manageResource` / `manage_resource`
returns resource-specific JSON; TypeScript `managementView()` retains the typed
server-owned form adapter for management UI resources.

A **Cloud session** owns a machine, configuration and persistent filesystem.
A **chat** is a root runtime thread on that machine with its own history.
A **turn** is one response in a chat. Chats share files and compute, and may
therefore modify the same files. A reusable Cloud agent is an optional template.
Creating a chat allocates no machine; connecting to a stopped session does not
start it. Start compute explicitly, then wait for `workspace.observedState` to
be `running` before connecting.

```ts
import { HaarnessCloud } from "@haarness/sdk";
const cloud = new HaarnessCloud({
  baseUrl: process.env.HAARNESS_CLOUD_API_URL!,
  token: process.env.HAARNESS_API_KEY,
});
const session = cloud.session(sessionId); // an existing running session
try {
  const chat = await session.chats.create({ title: "Investigate" });
  const reply = await chat.ask("Read README.md and summarize this project");
  console.log(session.id, chat.id, reply.turn.id, reply.text);
  const runtime = await session.connect();
  console.log(await runtime.listFiles({ path: "" }));
} finally {
  await session.close(); // detach only; does not stop compute
  cloud.close();
}
```

```python
from haarness_sdk import HaarnessCloud
with HaarnessCloud(api_url, token=account_access_token) as cloud:
    session = cloud.session(session_id)  # already running
    try:
        chat = session.chats.create({"title": "Investigate"})
        reply = chat.ask("Read README.md and summarize this project")
        print(session.id, chat.id, reply["turn"]["id"], reply["text"])
        print(session.connect().list_files({"path": ""}))
    finally:
        session.close()
```

Python async uses `AsyncHaarnessCloud`, `async with`, and awaits the same
operations. `session.chats.list()` returns root chats, with native pagination
options; `get(chatId)` selects one explicitly. Handles provide metadata,
rename, archive/restore, messages, ask, send, wait and abort. Wait/abort reject
a turn belonging to a different chat. The connected runtime exposes the entire
native API, including files, Git, PTYs, permissions and SSE.

Create a freestyle session with `createSession`/`create_session` and
`{title, configuration, start}`. To use a template, list/get Cloud agents and
include its `cloudAgentId`. Account credentials authenticate only the account
API; runtime requests use short-lived attach tickets. Tickets renew before
expiry and once after an authentication failure or a stale generation read.
An event stream encountering a new runtime generation returns `resync_required`;
reload snapshots before subscribing again.

Supply a scoped API key through `HAARNESS_API_KEY`, or an OAuth account access
token explicitly or through `HAARNESS_CLOUD_TOKEN`.
`HAARNESS_SERVER_TOKEN` authenticates the local runtime. Cloud API keys can authorize both Cloud operations and inference, depending on their capabilities. OAuth code/PKCE,
refresh and device endpoints are generated methods; SDK construction does not
log in, store secrets, or refresh an OAuth grant automatically. For long-lived
clients, supply TypeScript `resolveAuth: async () => ({type: "bearer", token})`
or Python `resolve_token: () -> str`, backed by your own serialized token store.
The resolver runs on requests and WebSocket reconnections.

For multiple accounts, pass the selected `accountId` in each HTTP method's
query/body where supported; ID-addressed resources enforce their own ownership.
The Cloud constructor's `accountId` (Python `account_id`) selects the default
WebSocket subscription account; it does not rewrite HTTP request bodies.

`cloud.events({reconnect: true})` in TypeScript, or
`cloud.events(reconnect=True)` in Python, observes account invalidations over
WebSocket. On `ready` or `resync_required`, reload snapshots. On `entity_changed`,
refresh the affected entity. These events are hints, not a replayable event log.
The socket is authenticated, bounded, heartbeat-monitored and reconnectable;
close its iterator/client to detach. Runtime `session.connect().events()` is
SSE carrying chat/turn/file events. Ordinary updates do not require a page reload.

The native app also exposes typed `cloudAgents`, `cloudSessions`, `cloudChats`
and `cloudSchedules` methods (snake_case in Python) through its signed-in host
adapter. These use the same intent and permission boundary as the model tools.
The host's `workspace(id)` helper scopes native methods to an opened workspace.

## Cloud routers and provider keys

Every account has a default router. Additional routers share the inference service
and have their own model allowlist and provider credentials. `system` uses Cloud
credits; `byok` uses your upstream provider account and consumes no Cloud credits
for model requests. Cloud session compute is billed separately. A BYOK router
never falls back to system credentials when a key is missing or fails.

```ts
const { items } = await cloud.listRouters({ accountId });
let router = await cloud.createRouter({
  accountId, name: "My OpenAI", credentialMode: "byok",
  modelAllowlist: ["openai/gpt-4.1-nano"],
});
router = await cloud.setRouterCredential(router.id, "openai", {
  revision: router.revision, apiKey: providerApiKey,
});
// Use router.routerUrl as an OpenAI-compatible client's base URL,
// with your Haarness API key as that client's api_key.
```

```python
routers = cloud.list_routers({"accountId": account_id})["items"]
router = cloud.create_router({
    "accountId": account_id, "name": "My OpenAI", "credentialMode": "byok",
    "modelAllowlist": ["openai/gpt-4.1-nano"],
})
router = cloud.set_router_credential(router["id"], "openai", {
    "revision": router["revision"], "apiKey": provider_api_key,
})
```

Keep the returned revision for the next update; stale writes return 409. Provider
keys are write-only. Explicit `apiKey: null` (Python `None`) removes a key;
omitting `apiKey` is rejected. `getRouter`, `updateRouter`, and `deleteRouter`
(snake_case in Python) complete the management surface. The default cannot be
deleted. Management requires the account's admin role and `routers:write` for API
keys; discovery requires `models:read`, inference requires `models:invoke`.

The returned `routerUrl` includes `/routers/{id}/v1`; append `/models` or
`/chat/completions` once. The standard `/v1` endpoint uses the default router.
An explicit `x-haarness-router-id` header also selects a router; conflicting URL
and header IDs fail. Usage records include `routerId` and `credentialMode`, and
BYOK catalog prices represent zero Cloud cost. Account WebSocket events use
`entity: "router"`. Native users can select with `haarness account --router UUID`,
TUI `/router`, or Desktop's Cloud router selector.

## Cloud schedules

Cloud schedules run in the Cloud worker even when your laptop and SDK are
closed. They target explicit `sessionId` and `chatId`. The native `schedule`
tool/CLI remains the device-local scheduler; `cloud_schedule` and the Cloud SDK
methods are a separate surface.

```ts
const scheduled = await cloud.createCloudSchedule({
  sessionId, chatId, name: "Daily review", prompt: "Review the latest changes",
  everySeconds: 86400,
}, { headers: { "Idempotency-Key": crypto.randomUUID() } });
const paused = await cloud.updateCloudSchedule(scheduled.id, {
  version: scheduled.version, enabled: false,
});
const run = await cloud.runCloudSchedule(scheduled.id, {
  headers: { "Idempotency-Key": crypto.randomUUID() },
});
console.log(await cloud.cloudScheduleRuns(scheduled.id));
await cloud.deleteCloudSchedule(scheduled.id);
```

Python methods are `create_cloud_schedule`, `list_cloud_schedules`,
`get_cloud_schedule`, `update_cloud_schedule`, `run_cloud_schedule`,
`cloud_schedule_runs`, and `delete_cloud_schedule`. Transport headers use
`options={"headers": {"Idempotency-Key": ...}}`. Pause/resume is an update with
`enabled`; supply the latest version from get. Use an RFC3339 `at` with a UTC
offset for a one-shot run or an initial repeating date. Repeat intervals are
60–31,536,000 seconds. Updating the cadence/target requires a new schedule;
name, prompt, enabled and next date can be updated in place.

The worker checks due work every five seconds. It coalesces missed repeats,
prevents overlapping runs per schedule and reuses a stable runtime request ID
on retries. It can wake an existing stopped session under normal plan and
concurrency limits, with a ten-minute startup deadline. Normal permission
prompts become `needs_attention`; schedules never auto-approve them. Archived
or missing target chats pause future execution when detected. Archiving does
not interrupt an already accepted turn. Pausing/deleting a schedule prevents
future ticks; deleting cancels pending work but does not abort an accepted turn.
Use chat abort explicitly for that. Session deletion cancels schedules and
removes its machine, disk and all chats. Terminal schedule history is retained
for 30 days. Stopping retains chats/files and disk charges; archiving only hides
an item. Closing an SDK handle never stops billable compute.

For upload, use the connected runtime's `writeFile`/`write_file` with `path`,
`content`, `encoding` (`utf8` or `base64`) and the current `revision`. New files
use the empty-content SHA-256 revision. Stale writes return 409. Successful file
mutations emit `files.updated`; completed turns also invalidate the explorer.
Model attachments and filesystem uploads are distinct: put ordinary documents
on the session filesystem when the agent should read them with native tools.

## Generation and verification

```sh
# Build the real server first. Export schemas from a private owned instance.
cargo build -p haarness-cli
python3 sdks/tests/server.py
python3 sdks/generate.py

# Full reproducible gate: installs locked SDK development dependencies locally.
./scripts/test-sdks.sh
```

`schema.json` is the live server's own schema snapshot: a running server's
`/openapi.json` also fills in every route and shape from `openapi.json` and
lists those under `x-haarness-reference`, which `server.py` leaves out.
`inventory.py` extracts every registered method/path and handler signature
from Rust.
`generate.py` combines those with explicit contracts for server-local wire
shapes missing from the protocol schemas. It emits the complete OpenAPI spec,
operation inventory, TypeScript types/methods, and Python types/sync/async
methods. `--check` detects source/generated drift without modifying files.

The shared E2E scenario runs through each actual SDK and requires successful
coverage of all 119 non-account native operations in each client; the four account adapters are covered by the live Cloud-tool scenario. Every JSON response is checked
against its declared schema. Assertions check resulting files, history,
idempotency, approvals, questions, workflow/background lifecycles, MCP
stdio tools/resources/prompts, PTY IO/replay, binary profile round-trips,
archives, commands and shutdown. Further tests cover owned server startup and
cleanup, live event streaming, cancellation, timeout, SSE parsing/reconnect,
URL encoding, errors, redirects and the unpublished package artifacts.
The provider is Haarness's deterministic echo adapter and MCP uses a stdio
fixture; these tests do not contact paid model services.

Cloud generation imports the versioned sibling `haarness-cloud/docs/openapi.json`
and includes every operation without a route allowlist. Copy the updated document
to `sdks/cloud-openapi.json` before regeneration; the check catches sibling drift.
Contract tests cover all 125 HTTP operations in TypeScript and Python sync/async,
including form encoding, manual redirects and binary responses. Live fixture
journeys in `sdks/tests/cloud_e2e.py` (sync/async) and `cloud_ts_e2e.mjs` exercise
real account API, gateway, runtimes, two chats, uploads, all schedule operations,
worker execution while disconnected, stop/start persistence and deletion. They
require an isolated Cloud fixture stack and use fixture-only Stripe funding.
The paid-provider/tmux scenario is opt-in: `scripts/test-cloud-tools-live.py`.

### API key capabilities and lifecycle

Use the same `hck_` API key as the Cloud SDK `token` and an OpenAI-compatible model client's
`api_key`, with the returned `routerUrl` as its base URL. Cloud SDKs also read `HAARNESS_API_KEY`
(before the legacy `HAARNESS_CLOUD_TOKEN`); explicit `token` takes precedence. Keys cannot
manage other keys: creation, update, rotation and revocation require an owner/admin sign-in.
The generated API includes key list/create/update/rotate/revoke and self-context methods.
Grant `models:read` for discovery, `models:invoke` for inference, and appropriate `workspaces`,
`schedules`, `marketplace` or `billing:read` permissions for Cloud calls. An attach capability
authorizes full runtime chats, files, terminal and settings in sessions in the key's account.
Expired or revoked keys fail; SDKs never implicitly replace them with OAuth credentials.


### Releases and inference

`HaarnessCloud` now includes current/product manifests, release history, artifact and
platform downloads, and the installer scripts. Download methods return bytes; verify
SHA-256 against the manifest before running them. Operator-only upload, publish,
promote and withdraw methods require `x-haarness-internal-token` in request options.
Uploads require `x-content-sha256` and `content-length`; ordinary API keys cannot publish.

Use `HaarnessRouter({baseUrl, token})` in TypeScript, or
`HaarnessRouter(base_url, token=...)` / `AsyncHaarnessRouter` in Python. Pass the
Cloud router URL ending in `/v1`, including `/routers/{id}/v1` for a named router.
`models`, `chatCompletions` / `chat_completions`, and `streamChatCompletions` /
`stream_chat_completions` cover model discovery and JSON/SSE inference. Provider
options and tool messages pass through unchanged. `cloud.router(id, apiKey)`
(Python: `cloud.router(id, api_key=...)`) discovers the named URL without forwarding
account authentication as inference credentials. Streams fail on malformed/error
frames or a missing `[DONE]`, never replay inference, and close on cancellation.
Python callers stopping consumption early should close the generator (`close` /
`aclose`, or `contextlib.closing` / `aclosing`).

Native browser hosts use `connectBrowserHost`, `connectDraftBrowserHost`,
`browserTunnel`, `draftBrowserTunnel`, and `adoptBrowserDraft` (snake_case in Python).
Preserve the returned lease when tunnelling/adopting; these are host protocol APIs,
not a replacement for browser tool calls. `readSharedSession` returns published HTML.
Cloud `connectRemoteHost` implements the host-side relay WebSocket; authenticate with
native account credentials or a key allowed to connect hosts. Public control clients
normally use `remoteHost(id).workspace(projectId)` or `remoteSession(id)` instead.
