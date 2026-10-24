# Haarness Python SDK

Unpublished Python 3.10+ synchronous and asynchronous clients for every native server and Cloud
HTTP operation, with typed requests/responses, SSE, PTYs, attachments and owned local-server
helpers. This README is for Python developers using the package; the repository's
`sdks/README.md` has the full guide and how the SDKs are generated and tested.

> **User guide:** [TypeScript and Python SDKs](https://haarness.ai/docs/server/sdks).

```sh
python -m pip install /path/to/haarness/sdks/python
```

Public PyPI publication is blocked by the `Private :: Do Not Upload` classifier.
Local wheel and source distributions can be built with `uv build`.

```python
from haarness_sdk import Haarness, ApiError

with Haarness("http://127.0.0.1:4096") as client:
    thread = client.create_thread({"title": "Review"})
    result = client.chat(thread["id"], "Inspect this repository")
    print(result["turn"]["status"], result["text"])
```

Pass `token=...` or set HAARNESS_SERVER_TOKEN. AsyncHaarness has the same methods:
use async with and await. The constructor never starts a server.

Methods use snake_case, but dictionary keys preserve server JSON spelling.
Arguments are path values, then body/query when applicable. Transport options
are keyword-only: `options={"timeout": 125}` (seconds). Default is 30 seconds;
chat defaults to about five minutes. The turn-wait query uses `timeout_ms`,
whereas workflow wait uses `timeoutMs`. All types are available in
haarness_sdk.types. Omitted fields and explicit None resets remain distinct.

`events(after=..., reconnect=True)` is a closeable iterator; async clients return
an async iterator. Use contextlib.closing/aclosing for early exit. Capture
info()["eventCursor"] before REST hydration. A resync_required marker ends the
stream: rehydrate REST snapshots and reconnect with a fresh cursor. With
`idle_timeout=45` (seconds; the server sends a keepalive every 15 s), a stream
that sends nothing for that long counts as lost, a connection that died without
closing: it connects again from its cursor with `reconnect=True`, and otherwise
ends with TransportError. Time spent with an event does not count. Cancel an
async task to stop waiting. PTY connections ping their server every 20 s by
themselves and close when it stops answering; `ping(timeout)` asks at once. Client disconnect/cancellation never interrupts
server work; use interrupt_turn/interrupt_thread_tree for that action.

`btw_stream(id, {"question": ...})` asks a side question and yields its answer
as it is written (`delta`, `text`, then `done` or `error`), failing with
TransportError after `idle_timeout=45` seconds of silence; `btw()` answers in one piece.
`workspace(id)` returns a client bound to one project behind a host gateway
(`host_projects()` lists them).

No mutations are retried automatically. Reuse requestId for an intentional
idempotent turn retry. ApiError exposes status/code/retryable/body; network and
decoding errors use TransportError. Async CancelledError is preserved. Handle
pending permissions/questions concurrently while chat waits; the SDK does not
automatically approve them.

`connect_pty(id, cursor=...)` yields a socket with send, recv, close and cursor;
use with/iteration or async with/async iteration. Output is bytes; controls are
JSON dictionaries. Save a consumed cursor control's end value for reconnect.
Closing detaches; delete_pty terminates the process.

`attachments(paths)` returns remote-safe PDF/image/audio/video data URLs with a
20 MiB aggregate bound. upload_profile/download_profile use ZIP bytes.

```python
from haarness_sdk import LocalServer, AsyncLocalServer
with LocalServer.start("/path/to/project", executable="/path/to/haarness") as server:
    print(server.client.info())
# async with await AsyncLocalServer.start(...) as server:
```

Local server helpers own and clean up only their child (in a folder whose server
already runs, a front for it at its own address, never a replacement), and load project configuration by default without automatically approving tools.

The repository's `sdks/OPERATIONS.md` lists every method; `sdks/README.md`
describes generation and `scripts/test-sdks.sh` runs the real-server E2E gate.
execute_command handles server commands; CLI-only authentication/install/native
scheduler administration and TUI-only commands are not HTTP endpoints.

## Optional server authentication

```python
with Haarness("http://localhost:4096", password="") as client:
    info = client.info()

with Haarness("https://haarness.example.com", username="haarness", password=password) as client:
    info = client.info()
```

The username defaults to `haarness`. An explicit empty password connects without
authentication and ignores an ambient legacy token. `AsyncHaarness` accepts the
same options. Credentials apply to requests, event streams and terminals; the
existing `token` option remains compatible.

## Cloud

`HaarnessCloud` connects to the Cloud account API using an account token
(`HAARNESS_CLOUD_TOKEN`), or a scoped API key (`HAARNESS_API_KEY`), separately from runtime tokens.
Python also exports `AsyncHaarnessCloud`. Select `session(sessionId)`, then
`session.chats` for root chat handles with independent history and shared files.
Connect the session to access the complete native API. Cloud account updates
use authenticated WebSockets; chat/turn updates use runtime SSE.

Cloud schedule CRUD, pause/resume, run-now and history are generated methods.
Schedules target a session and chat and run on the Cloud worker with the laptop
off. They differ from the device-local `schedule` tool. Closing a client only
detaches; explicitly stop or delete sessions to release compute. The repository's
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
authorizes full runtime chats, files, terminal and settings in sessions in the key's account.
Expired or revoked keys fail; SDKs never implicitly replace them with OAuth credentials.
