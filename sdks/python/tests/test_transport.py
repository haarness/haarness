import asyncio
import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import pytest
from haarness_sdk import Haarness, AsyncHaarness, ApiError, TransportError, attachments


def test_media_twenty_mib_aggregate_boundary(tmp_path):
    path = tmp_path / "fixture.png"
    with path.open("wb") as file:
        file.truncate(11 * 1024 * 1024)
        assert len(attachments([path])) == 1
        with pytest.raises(ValueError, match="20 MiB"):
            attachments([path, path])
        file.truncate(20 * 1024 * 1024)
        assert len(attachments([path])) == 1
        file.truncate(20 * 1024 * 1024 + 1)
        with pytest.raises(ValueError, match="20 MiB"):
            attachments([path])


@pytest.fixture
def endpoint():
    calls = []

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_GET(self):
            calls.append((self.path, dict(self.headers), None))
            if self.path == "/v1/events":
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.end_headers()
                event = {
                    "type": "item.created",
                    "schemaVersion": "1",
                    "serverInstanceId": "instance",
                    "eventId": 2,
                    "payload": "雪",
                }
                data = (
                    ": keepalive\r\n\r\ndata: "
                    + json.dumps(event)
                    + '\r\n\r\ndata: {"type":\ndata: "resync_required"}\n\n'
                ).encode()
                for byte in data:
                    self.wfile.write(bytes([byte]))
                    self.wfile.flush()
                return
            if self.path == "/v1/threads/missing":
                code, body = (
                    404,
                    {"code": "not_found", "message": "Missing", "retryable": False},
                )
            elif self.path == "/v1/info":
                code, body = 302, {}
            else:
                code, body = 200, {}
            self.send_response(code)
            self.send_header("Location", "http://unrelated.example/")
            self.end_headers()
            self.wfile.write(json.dumps(body).encode())

        def do_POST(self):
            data = self.rfile.read(int(self.headers.get("Content-Length", 0)))
            calls.append((self.path, dict(self.headers), data))
            if self.path.endswith("/btw/stream"):
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.end_headers()
                frames = (
                    ': connected\n\ndata: {"type":"delta","text":"An "}\r\n\r\n'
                    'data: {"type":"delta","text":"answer"}\n\n'
                    'data: {"type":"done","answer":"An answer","inputTokens":7,"outputTokens":3}\n\n'
                )
                if json.loads(data)["question"] == "half":
                    frames = 'data: {"type":"delta","text":"half"}\n\n'
                for byte in frames.encode():
                    self.wfile.write(bytes([byte]))
                    self.wfile.flush()
                return
            self.send_response(204)
            self.end_headers()

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{server.server_port}", calls
    finally:
        server.shutdown()
        server.server_close()
        thread.join()


def test_sync_transport(endpoint):
    url, calls = endpoint
    with Haarness(url, token="fixture") as client:
        client.load_skill("spaces /?#雪")
        assert "spaces%20%2F%3F%23%E9%9B%AA" in calls[-1][0]
        client.list_threads({"archived": False})
        assert calls[-1][0].endswith("archived=false")
        client.update_tool("read", {"enabled": None})
        assert json.loads(calls[-1][2]) == {"enabled": None}
        client.call_mcp_tool("fixture", "echo", None)
        assert calls[-1][2] == b"null"
        client.upload_profile(b"\x00\xff")
        assert calls[-1][2] == b"\x00\xff"
        with pytest.raises(ApiError) as result:
            client.get_thread("missing")
        assert result.value.code == "not_found" and not result.value.retryable
        with pytest.raises(ApiError) as result:
            client.info()
        assert result.value.status == 302  # no redirect or token forwarding
        events = list(client.events(after=1))
        assert events[0]["payload"] == "雪" and events[-1]["type"] == "resync_required"
        assert calls[-1][1]["Last-Event-ID"] == "1"
        with pytest.raises(ValueError):
            client.get_thread("..")
    assert sum(path == "/v1/threads/missing" for path, *_ in calls) == 1


def test_async_transport(endpoint):
    async def scenario():
        url, calls = endpoint
        async with AsyncHaarness(url, token="fixture") as client:
            await client.call_mcp_tool("fixture", "echo", None)
            assert calls[-1][2] == b"null"
            await client.upload_profile(b"\x00\xff")
            assert calls[-1][2] == b"\x00\xff"
            with pytest.raises(ApiError):
                await client.info()
            events = [event async for event in client.events(after=1)]
            assert len(events) == 2 and events[-1]["type"] == "resync_required"

    asyncio.run(scenario())


@pytest.mark.parametrize(
    "url",
    [
        "file:///tmp",
        "http://user:secret@localhost",
        "http://localhost/#fragment",
        "http://localhost/?token=secret",
    ],
)
def test_invalid_url(url):
    with pytest.raises(ValueError):
        Haarness(url)


def test_optional_password_auth(endpoint, monkeypatch):
    import base64

    url, calls = endpoint
    monkeypatch.setenv("HAARNESS_SERVER_TOKEN", "ambient-token")
    with Haarness(url, password="") as client:
        client.load_skill("demo")
    assert "Authorization" not in calls[-1][1]
    with Haarness(url, password="日本:secret") as client:
        client.load_skill("demo")
    assert (
        calls[-1][1]["Authorization"]
        == "Basic " + base64.b64encode("haarness:日本:secret".encode()).decode()
    )

    async def check():
        async with AsyncHaarness(url, username="alex", password="secret") as client:
            await client.load_skill("demo")

    asyncio.run(check())
    assert calls[-1][1]["Authorization"] == "Basic YWxleDpzZWNyZXQ="
    with pytest.raises(ValueError):
        Haarness(url, username="bad:user", password="secret")


@pytest.fixture
def silent_endpoint():
    """Sends one event per connection, then holds it open and sends nothing,
    like a connection that died without closing."""
    cursors = []
    release = threading.Event()

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_GET(self):
            cursors.append(self.headers.get("Last-Event-ID"))
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            event = {
                "type": "item.created",
                "schemaVersion": "1",
                "serverInstanceId": "instance",
                "eventId": 10 + len(cursors),
                "payload": None,
            }
            self.wfile.write(("data: " + json.dumps(event) + "\n\n").encode())
            self.wfile.flush()
            release.wait(10)

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server.daemon_threads = True
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{server.server_port}", cursors
    finally:
        release.set()
        server.shutdown()
        server.server_close()
        thread.join()


def test_a_silent_event_stream_is_lost_or_continues_from_its_cursor(silent_endpoint):
    from haarness_sdk import TransportError

    url, cursors = silent_endpoint
    with Haarness(url, token="fixture") as client:
        seen = []
        with pytest.raises(TransportError, match="sent nothing for 0.3 s"):
            for event in client.events(after=10, idle_timeout=0.3):
                seen.append(event["eventId"])
        assert seen == [11]
        stream = client.events(after=11, reconnect=True, retry_seconds=0.01, idle_timeout=0.3)
        assert [next(stream)["eventId"], next(stream)["eventId"]] == [12, 13]
        stream.close()
    assert cursors == ["10", "11", "12"]

    async def scenario():
        async with AsyncHaarness(url, token="fixture") as client:
            with pytest.raises(TransportError, match="sent nothing"):
                async for _event in client.events(after=13, idle_timeout=0.3):
                    pass

    asyncio.run(scenario())


def test_a_slow_consumer_is_not_the_servers_silence_and_the_limit_must_be_positive():
    import time

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_GET(self):
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            for index in range(1, 40):
                event = {"type": "item.created", "schemaVersion": "1",
                         "serverInstanceId": "instance", "eventId": index, "payload": None}
                try:
                    self.wfile.write(b": keepalive\n\n" + ("data: " + json.dumps(event) + "\n\n").encode())
                    self.wfile.flush()
                except OSError:
                    return
                time.sleep(0.05)

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server.daemon_threads = True
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        with Haarness(f"http://127.0.0.1:{server.server_port}", token="fixture") as client:
            seen = []
            stream = client.events(idle_timeout=0.2)
            for event in stream:
                seen.append(event["eventId"])
                time.sleep(0.4)
                if len(seen) == 3:
                    break
            stream.close()
            assert seen == [1, 2, 3]
            with pytest.raises(ValueError):
                next(client.events(idle_timeout=0))
    finally:
        server.shutdown()
        server.server_close()


def test_a_pty_ping_says_whether_the_server_answers():
    import websockets.sync.server
    from haarness_sdk import PtyConnection

    with websockets.sync.server.serve(lambda socket: socket.recv(), "127.0.0.1", 0) as server:
        threading.Thread(target=server.serve_forever, daemon=True).start()
        port = server.socket.getsockname()[1]
        pty = PtyConnection(f"ws://127.0.0.1:{port}/", {}, 5)
        assert pty.ping(2)
        pty.close()
        server.shutdown()


def test_streamed_side_questions(endpoint):
    url, calls = endpoint
    history = [{"question": "What?", "answer": "That."}]
    with Haarness(url, token="fixture") as client:
        events = list(client.btw_stream("t/1", {"question": "Why?", "history": history}))
        assert [event["type"] for event in events] == ["delta", "delta", "done"]
        assert events[-1]["answer"] == "An answer"
        assert calls[-1][0] == "/v1/threads/t%2F1/btw/stream"
        assert json.loads(calls[-1][2])["history"] == history
        with pytest.raises(TransportError, match="ended before it finished"):
            list(client.btw_stream("t", {"question": "half"}))

    async def scenario():
        async with AsyncHaarness(url, token="fixture") as client:
            events = [event async for event in client.btw_stream("t", {"question": "Why?"})]
            assert events[-1] == {"type": "done", "answer": "An answer", "inputTokens": 7, "outputTokens": 3}

    asyncio.run(scenario())
