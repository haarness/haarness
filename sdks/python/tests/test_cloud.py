import asyncio
import json
from pathlib import Path
import httpx
import pytest
from haarness_sdk import HaarnessCloud, AsyncHaarnessCloud
from haarness_sdk.cloud_transport import NoRedirectConnect

OPERATIONS = json.loads(
    (Path(__file__).parents[2] / "cloud-operations.json").read_text()
)


def exercise_args(op):
    args = ["opaque id" for _ in op["pathParams"]]
    if op.get("body") or op.get("binaryBody"):
        args.append(
            b"\x00\xff"
            if op.get("binaryBody")
            else "literal text"
            if op.get("contentType") == "text/plain"
            else {"key": "space value"}
        )
    if op.get("query"):
        args.append({"limit": 2})
    return args


def handler(op, calls):
    def call(request):
        calls.append(op["name"])
        assert request.method == op["method"]
        assert request.url.raw_path.split(b"?")[0].decode() == __import__("re").sub(
            r"\{[^}]+\}", "opaque%20id", op["path"]
        )
        assert request.headers["Authorization"] == "Bearer account-fixture"
        assert request.headers["Idempotency-Key"] == "fixture-key"
        if op.get("binaryBody"):
            assert request.content == b"\x00\xff"
        elif op.get("body"):
            assert request.headers["Content-Type"] == op["contentType"]
            if op["contentType"] == "application/x-www-form-urlencoded":
                assert request.content == b"key=space+value"
            elif op["contentType"] == "text/plain":
                assert request.content == b"literal text"
            else:
                assert json.loads(request.content) == {"key": "space value"}
        if op.get("redirectResponse"):
            return httpx.Response(302, headers={"location": "/authorized"})
        if op.get("binaryResponse"):
            return httpx.Response(200, content=b"\x00\xff")
        if op.get("textResponse"):
            return httpx.Response(200, text="<html>fixture</html>")
        return httpx.Response(200, json={"ok": True})

    return call


@pytest.mark.parametrize("asynchronous", [False, True])
def test_every_cloud_http_operation(asynchronous):
    calls = []

    async def run():
        async with AsyncHaarnessCloud(
            "http://localhost:12345", token="account-fixture"
        ) as client:
            await client._http.aclose()
            for op in OPERATIONS:
                if op.get("stream"):
                    continue
                client._http = httpx.AsyncClient(
                    transport=httpx.MockTransport(handler(op, calls))
                )
                try:
                    await getattr(client, op["name"])(
                        *exercise_args(op),
                        options={"headers": {"Idempotency-Key": "fixture-key"}},
                    )
                finally:
                    await client._http.aclose()

    if asynchronous:
        asyncio.run(run())
    else:
        with HaarnessCloud("http://localhost:12345", token="account-fixture") as client:
            client._http.close()
            for op in OPERATIONS:
                if op.get("stream"):
                    continue
                client._http = httpx.Client(
                    transport=httpx.MockTransport(handler(op, calls))
                )
                try:
                    getattr(client, op["name"])(
                        *exercise_args(op),
                        options={"headers": {"Idempotency-Key": "fixture-key"}},
                    )
                finally:
                    client._http.close()
    assert len(calls) == len([op for op in OPERATIONS if not op.get("stream")])


def test_websocket_redirects_cannot_forward_credentials():
    error = Exception("redirect")
    assert NoRedirectConnect("ws://localhost").process_redirect(error) is error


def test_cloud_does_not_use_runtime_token(monkeypatch):
    monkeypatch.setenv("HAARNESS_SERVER_TOKEN", "runtime-secret")
    monkeypatch.delenv("HAARNESS_CLOUD_TOKEN", raising=False)
    with HaarnessCloud("http://localhost") as cloud:
        assert cloud._headers() == {}


@pytest.mark.parametrize("asynchronous", [False, True])
def test_cloud_websocket_reconnect_refreshes_credentials(asynchronous):
    import threading
    from websockets.sync.server import serve

    seen = []

    def handler(socket):
        seen.append(socket.request.headers["Authorization"])
        socket.send(
            json.dumps(
                {
                    "type": "ready",
                    "scope": "account",
                    "resync": True,
                    "entities": ["schedule"],
                }
            )
        )
        if len(seen) == 1:
            socket.close()
        else:
            try:
                socket.recv(timeout=5)
            except Exception:
                pass

    with serve(handler, "127.0.0.1", 0) as server:
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        url = f"http://127.0.0.1:{server.socket.getsockname()[1]}"
        resolver = lambda: f"renewed-{len(seen) + 1}"
        events = []
        try:
            if asynchronous:

                async def run():
                    async with AsyncHaarnessCloud(url, resolve_token=resolver) as cloud:
                        stream = cloud.events(reconnect=True, retry_seconds=0.001)
                        try:
                            async for event in stream:
                                events.append(event["type"])
                                if events.count("ready") == 2:
                                    break
                        finally:
                            await stream.aclose()

                asyncio.run(run())
            else:
                with HaarnessCloud(url, resolve_token=resolver) as cloud:
                    stream = cloud.events(reconnect=True, retry_seconds=0.001)
                    try:
                        for event in stream:
                            events.append(event["type"])
                            if events.count("ready") == 2:
                                break
                    finally:
                        stream.close()
            assert seen == ["Bearer renewed-1", "Bearer renewed-2"]
            assert "resync_required" in events
        finally:
            server.shutdown()
            thread.join(timeout=5)


@pytest.mark.parametrize("asynchronous", [False, True])
@pytest.mark.parametrize("failure", ["expired", "restarted", "denied"])
def test_runtime_stream_renews_once_and_preserves_cursor(asynchronous, failure):
    from haarness_sdk import ApiError

    tickets, seen = [], []

    def attach(_):
        tickets.append(len(tickets) + 1)
        return {
            "gatewayUrl": "http://localhost/runtime",
            "ticket": f"ticket-{len(tickets)}",
            "expiresAt": "2099-01-01T00:00:00Z",
        }

    def respond(request):
        seen.append(
            (request.headers["Authorization"], request.headers["Last-Event-ID"])
        )
        # A dropped stream followed by a runtime restart must preserve the cursor
        # and emit resync, not confuse a new server's event IDs with old history.
        if failure == "restarted" and len(seen) == 1:
            body = {
                "eventId": 8,
                "serverInstanceId": "old",
                "type": "files.updated",
                "payload": {},
            }
        elif len(seen) == (2 if failure == "restarted" else 1) or failure == "denied":
            return httpx.Response(
                400 if failure == "restarted" else 401,
                json={"code": "remote_project", "message": "workspace restarted"},
            )
        else:
            body = {
                "eventId": 1 if failure == "restarted" else 8,
                "serverInstanceId": "new",
                "type": "files.updated",
                "payload": {},
            }
        return httpx.Response(
            200,
            text="data: " + json.dumps(body) + "\n\n",
            headers={"content-type": "text/event-stream"},
        )

    async def run():
        async with AsyncHaarnessCloud(
            "http://localhost", token="account-secret"
        ) as cloud:

            async def async_attach(id):
                return attach(id)

            cloud.attach_agent_session = async_attach
            session = cloud.session("session")
            runtime = await session.connect()
            await runtime._http.aclose()
            runtime._http = httpx.AsyncClient(transport=httpx.MockTransport(respond))
            stream = runtime.events(
                after=7, reconnect=failure == "restarted", retry_seconds=0.001
            )
            try:
                events = [event async for event in stream]
                assert [e["type"] for e in events] == (
                    ["files.updated", "resync_required"]
                    if failure == "restarted"
                    else ["files.updated"]
                )
            finally:
                await stream.aclose()
                await session.close()

    def run_sync():
        with HaarnessCloud("http://localhost", token="account-secret") as cloud:
            cloud.attach_agent_session = attach
            session = cloud.session("session")
            runtime = session.connect()
            runtime._http.close()
            runtime._http = httpx.Client(transport=httpx.MockTransport(respond))
            stream = runtime.events(
                after=7, reconnect=failure == "restarted", retry_seconds=0.001
            )
            try:
                events = list(stream)
                assert [e["type"] for e in events] == (
                    ["files.updated", "resync_required"]
                    if failure == "restarted"
                    else ["files.updated"]
                )
            finally:
                stream.close()
                session.close()

    if failure == "denied":
        with pytest.raises(ApiError):
            asyncio.run(run()) if asynchronous else run_sync()
    else:
        asyncio.run(run()) if asynchronous else run_sync()
    assert tickets == [1, 2]
    assert seen == (
        [("Bearer ticket-1", "7"), ("Bearer ticket-1", "8"), ("Bearer ticket-2", "8")]
        if failure == "restarted"
        else [("Bearer ticket-1", "7"), ("Bearer ticket-2", "7")]
    )


@pytest.mark.parametrize("asynchronous", [False, True])
def test_concurrent_chat_connections_share_one_runtime_client(asynchronous):
    import time
    from concurrent.futures import ThreadPoolExecutor

    calls = []
    ticket = {
        "gatewayUrl": "http://localhost/runtime",
        "ticket": "runtime",
        "expiresAt": "2099-01-01T00:00:00Z",
    }

    async def run():
        async with AsyncHaarnessCloud("http://localhost", token="account") as cloud:

            async def attach(id):
                calls.append(id)
                await asyncio.sleep(0.02)
                return ticket

            cloud.attach_agent_session = attach
            session = cloud.session("session")
            try:
                clients = await asyncio.gather(*(session.connect() for _ in range(8)))
                assert all(client is clients[0] for client in clients)
            finally:
                await session.close()

    if asynchronous:
        asyncio.run(run())
    else:
        with HaarnessCloud("http://localhost", token="account") as cloud:

            def attach(id):
                calls.append(id)
                time.sleep(0.02)
                return ticket

            cloud.attach_agent_session = attach
            session = cloud.session("session")
            try:
                with ThreadPoolExecutor(8) as pool:
                    clients = list(pool.map(lambda _: session.connect(), range(8)))
                assert all(client is clients[0] for client in clients)
            finally:
                session.close()
    assert calls == ["session"]
