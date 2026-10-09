import asyncio
import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import pytest
from haarness_sdk import Haarness, AsyncHaarness, ApiError, attachments


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
