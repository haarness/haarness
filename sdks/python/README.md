# Haarness Python SDK

Unpublished Python 3.10+ synchronous and asynchronous clients for all 103
Haarness server operations. Includes typed requests/responses, SSE, PTYs,
attachments and owned local-server helpers.

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
stream: rehydrate REST snapshots and reconnect with a fresh cursor. Cancel an
async task to stop waiting. Client disconnect/cancellation never interrupts
server work; use interrupt_turn/interrupt_thread_tree for that action.

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

Local server helpers own and clean up only their child, fail on an existing
workspace lease, and leave project trust/automatic approval off by default.

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
