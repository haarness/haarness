#!/usr/bin/env python3
"""Whitespace-only text never reaches Claude, so one blank reply cannot break a chat.

A mock Anthropic Messages endpoint answers like the real API: a request with
an empty or whitespace-only text block gets HTTP 400. The first turn's reply
streams line breaks before a tool call, which Haarness saves as a reply of
its own; before the fix, the round after the tool call sent it back and the
turn failed, and so did every later turn in that chat. The script
also starts a blank turn over the API and sends an image with only spaces as
its text. Runs on the built-in Anthropic provider against the mock: no model
is paid for.
"""
import http.server
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request
import uuid

import e2e_scratch
from e2e_env import ISOLATED

REPO = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get("HAARNESS_BIN", REPO / "target/debug/haarness")).resolve()
MODEL = "claude-sonnet-4-6"
REQUESTS = []
REFUSED = []
PNG = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGNgYGD4DwABBAEAwS2OUAAAAABJRU5ErkJggg=="


def blank_texts(value):
    """Every text block in a request whose text is empty or only whitespace."""
    found = []
    if isinstance(value, dict):
        if value.get("type") == "text" and not str(value.get("text", "")).strip():
            found.append(value.get("text"))
        for item in value.values():
            found += blank_texts(item)
    elif isinstance(value, list):
        for item in value:
            found += blank_texts(item)
    return found


class Anthropic(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(json.dumps({"data": [{"id": MODEL, "type": "model"}]}).encode())

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        messages = body.get("messages", [])
        blank = blank_texts(messages)
        if blank:
            REFUSED.append(blank)
            detail = ("messages: text content blocks must contain non-whitespace text"
                      if any(blank) else "messages: text content blocks must be non-empty")
            payload = json.dumps({"type": "error", "error": {"type": "invalid_request_error", "message": detail}}).encode()
            self.send_response(400)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)
            return
        REQUESTS.append(body)
        serialized = json.dumps(messages)
        last = messages[-1] if messages else {}
        tool_result = isinstance(last.get("content"), list) and any(
            block.get("type") == "tool_result" for block in last["content"])
        events = [{"type": "message_start", "message": {"id": "msg-" + uuid.uuid4().hex[:8],
                                                        "usage": {"input_tokens": 12, "output_tokens": 0}}}]
        if serialized.rfind("BLANK_PREFACE") > serialized.rfind("SECOND_TURN") and not tool_result:
            # What Claude sometimes streams: line breaks, then the tool call.
            events += [
                {"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}},
                {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "\n\n"}},
                {"type": "content_block_stop", "index": 0},
                {"type": "content_block_start", "index": 1,
                 "content_block": {"type": "tool_use", "id": "toolu_blank", "name": "read", "input": {}}},
                {"type": "content_block_delta", "index": 1,
                 "delta": {"type": "input_json_delta", "partial_json": json.dumps({"path": "notes.txt"})}},
                {"type": "content_block_stop", "index": 1},
            ]
            stop = "tool_use"
        else:
            text = "FIRST_DONE" if tool_result else "REPLY_OK"
            events += [
                {"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}},
                {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": text}},
                {"type": "content_block_stop", "index": 0},
            ]
            stop = "end_turn"
        events += [{"type": "message_delta", "delta": {"stop_reason": stop}, "usage": {"output_tokens": 4}},
                   {"type": "message_stop"}]
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()
        for event in events:
            self.wfile.write(("event: " + event["type"] + "\ndata: " + json.dumps(event) + "\n\n").encode())
        self.wfile.flush()


def run(args, **kwargs):
    return subprocess.run(args, check=True, capture_output=True, text=True, **kwargs).stdout


def until(check, label, timeout=30):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(0.05)
    raise AssertionError(label)


service = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Anthropic)
threading.Thread(target=service.serve_forever, daemon=True).start()
root = Path(tempfile.mkdtemp(prefix="haarness-blank-text-")).resolve()
home, workspace = root / "home", root / "workspace"
(home / ".local/bin").mkdir(parents=True)
(workspace / ".haarness").mkdir(parents=True)
(workspace / "notes.txt").write_text("fixture notes\n")
(workspace / ".haarness/config.toml").write_text(
    f'[defaults]\nprovider="anthropic"\nmodel="{MODEL}"\n[permissions]\nread="allow"\n[lsp]\nenabled=false\n')
env = {**ISOLATED, "PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "HOME": str(home), "HAARNESS_HOME": str(home),
       "HAARNESS_DATA_DIR": str(root / "data"), "USER": os.environ.get("USER", "fixture"), "LANG": "en_US.UTF-8",
       "ANTHROPIC_API_KEY": "fixture-key", "ANTHROPIC_BASE_URL": f"http://127.0.0.1:{service.server_port}/v1"}
args = [str(BINARY), "--workspace", str(workspace), "--trust-project"]
connection = None
try:
    connection = json.loads(run(args + ["desktop"], input='{"action":"connect"}', env=env, timeout=90))

    def api(path, body=None):
        request = urllib.request.Request(
            connection["url"] + "/v1" + path, data=None if body is None else json.dumps(body).encode(),
            headers={"Authorization": "Bearer " + connection["token"], "Content-Type": "application/json"})
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                raw = response.read()
                return json.loads(raw) if raw else None
        except urllib.error.HTTPError as error:
            return {"http": error.code, "error": error.read().decode()}

    def turn(thread, text, attachments=()):
        started = api(f"/threads/{thread}/turns", {"requestId": str(uuid.uuid4()), "text": text,
                                                   "attachments": list(attachments)})
        assert "id" in started, started
        return until(lambda: (lambda current: current if current["status"] not in ("queued", "running") else None)(
            api(f"/turns/{started['id']}")), f"turn {text!r} did not finish")

    def replies(thread):
        return [item["data"]["text"] for item in api(f"/threads/{thread}/items")
                if item["data"]["type"] == "assistant_message"]

    thread = api("/threads", {"provider": "anthropic", "model": MODEL})["id"]
    first = turn(thread, "BLANK_PREFACE read the notes")
    assert first["status"] == "completed", (first, REFUSED)
    assert "\n\n" in replies(thread), "the line breaks before the tool call are saved as they streamed"
    second = turn(thread, "SECOND_TURN and now?")
    assert second["status"] == "completed", (second, REFUSED)
    assert replies(thread)[-1] == "REPLY_OK", replies(thread)
    print("PASS a reply of line breaks before a tool call does not break the next turn", flush=True)

    before = len(api(f"/threads/{thread}/items"))
    refused = api(f"/threads/{thread}/turns", {"requestId": str(uuid.uuid4()), "text": " \n\t "})
    assert refused.get("http") == 400 and "text or an attachment" in refused["error"], refused
    assert len(api(f"/threads/{thread}/items")) == before, "a refused blank turn leaves nothing in the chat"
    third = turn(thread, "THIRD_TURN still fine")
    assert third["status"] == "completed", (third, REFUSED)
    print("PASS a blank turn is refused before it reaches the chat", flush=True)

    image = turn(thread, "   ", [PNG])
    assert image["status"] == "completed", (image, REFUSED)
    sent = REQUESTS[-1]["messages"][-1]["content"]
    assert [block["type"] for block in sent] == ["image"], sent
    print("PASS an image sent with only spaces goes without a text block", flush=True)

    assert not REFUSED, REFUSED
    print("PASS no request carried a blank text block", flush=True)
finally:
    if connection:
        subprocess.run(args + ["serve", "--stop"], env=env, capture_output=True, timeout=30)
    service.shutdown()
    e2e_scratch.settle(root)
