#!/usr/bin/env python3
"""Verify provider error redaction through a real daemon, durable API and log."""
import http.server
import json
from pathlib import Path
import sys
import threading
import time
import urllib.request
from uuid import uuid4

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "sdks" / "tests"))
from server import Fixture


class Provider(http.server.BaseHTTPRequestHandler):
    fail = True
    paths = []

    def log_message(self, *args):
        pass

    def do_GET(self):
        body = json.dumps({"data": [{"id": "fixture-model"}]}).encode()
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        self.rfile.read(int(self.headers["Content-Length"]))
        self.paths.append(self.path)
        if self.fail:
            self.close_connection = True
            return
        events = [
            {"type": "response.output_text.delta", "delta": "RECOVERED_REPLY"},
            {"type": "response.completed", "response": {"usage": {"input_tokens": 4, "output_tokens": 2}}},
        ]
        body = "".join("data: " + json.dumps(event) + "\n\n" for event in events).encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


service = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Provider)
worker = threading.Thread(target=service.serve_forever, daemon=True)
worker.start()
fixture = Fixture()
fixture.env.update(
    HAARNESS_HOME=str(fixture.home),
    OPENAI_API_KEY="fixture-key",
    OPENAI_BASE_URL=f"http://127.0.0.1:{service.server_port}/v1?key=private-query",
)
try:
    with fixture as server:
        def api(path, body=None):
            request = urllib.request.Request(
                server.url + "/v1" + path,
                data=None if body is None else json.dumps(body).encode(),
                headers={"Authorization": "Bearer " + server.token, "Content-Type": "application/json"},
            )
            with urllib.request.urlopen(request, timeout=10) as response:
                return json.load(response)

        def run(thread):
            turn = api(f"/threads/{thread}/turns", {"requestId": str(uuid4()), "text": "Reply briefly."})
            # A transport failure is retried with backoff before the turn fails.
            deadline = time.monotonic() + 90
            while time.monotonic() < deadline:
                result = api("/turns/" + turn["id"])
                if result["status"] not in ("running", "pending"):
                    return result
                time.sleep(0.05)
            raise AssertionError("Provider turn did not settle")

        thread = api("/threads", {"title": "Provider privacy fixture", "provider": "openai", "model": "fixture-model"})["id"]
        failed = run(thread)
        assert failed["status"] == "failed", failed
        items = api(f"/threads/{thread}/items")
        assert any(item["data"]["type"] == "error" for item in items), items
        persisted = json.dumps({"turn": failed, "items": items})
        assert "Could not reach the provider" in persisted, persisted
        assert "private-query" not in persisted, "URL secret reached durable API errors"
        # Read the same state again, and include the real runtime error log.
        assert "private-query" not in json.dumps(api(f"/threads/{thread}/items"))
        log = (server.root / "server.log").read_text()
        assert "turn failed" in log, "Expected runtime failure was not logged"
        assert f"127.0.0.1:{service.server_port}/v1" in log, "Diagnostic endpoint context was lost"
        assert "private-query" not in log, "URL secret reached the server log"
        Provider.fail = False
        recovered = run(thread)
        assert recovered["status"] == "completed", recovered
        assert "RECOVERED_REPLY" in json.dumps(api(f"/threads/{thread}/items"))
        assert all("private-query" in path for path in Provider.paths), "Error redaction changed request URLs"
        print("PASS: provider failure, durable API and log redaction, unchanged request URL, successful next turn")
finally:
    service.shutdown()
    service.server_close()
    worker.join(timeout=5)
