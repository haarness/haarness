#!/usr/bin/env python3
"""A turn the provider's safety check stopped is left out of every later request.

A mock Anthropic Messages endpoint stops any reply to a request whose last
message names TRIP_WIRE with `stop_reason: refusal`, after a few words, and
answers everything else. The chat keeps the stopped turn as it was (the
person's message, the words that streamed, the error), while the next request
goes without it, so a reworded message is not stopped again for the same
history. A turn stopped after a tool call leaves the call and its result out
too, a restart keeps the decision, and the stop's error says so in plain
words. Runs on the built-in Anthropic provider against the mock: no model is
paid for.
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
# The chat's own requests, in order (a title request offers no tools).
CHAT = []


def last_user_text(messages):
    """The text of the newest user message that is not only tool results."""
    for message in reversed(messages):
        if message.get("role") != "user":
            continue
        content = message.get("content")
        if isinstance(content, str):
            return content
        texts = [block.get("text", "") for block in content if block.get("type") == "text"]
        if texts:
            return "\n".join(texts)
    return ""


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
        if body.get("tools"):
            CHAT.append(body)
        last = messages[-1] if messages else {}
        tool_result = isinstance(last.get("content"), list) and any(
            block.get("type") == "tool_result" for block in last["content"])
        asked = last_user_text(messages)
        events = [{"type": "message_start", "message": {"id": "msg-" + uuid.uuid4().hex[:8],
                                                        "usage": {"input_tokens": 12, "output_tokens": 0}}}]

        def text(words):
            return [{"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}},
                    {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": words}},
                    {"type": "content_block_stop", "index": 0}]

        if "TOOL_TRIP" in asked and not tool_result:
            events += [{"type": "content_block_start", "index": 0,
                        "content_block": {"type": "tool_use", "id": "toolu_trip", "name": "read", "input": {}}},
                       {"type": "content_block_delta", "index": 0,
                        "delta": {"type": "input_json_delta", "partial_json": json.dumps({"path": "notes.txt"})}},
                       {"type": "content_block_stop", "index": 0}]
            stop = "tool_use"
        elif "TRIP_WIRE" in asked or "TOOL_TRIP" in asked:
            events += text("PARTIAL_WORDS")
            stop = "refusal"
        else:
            events += text("REPLY_OK")
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
root = Path(tempfile.mkdtemp(prefix="haarness-safety-stop-")).resolve()
home, workspace = root / "home", root / "workspace"
(home / ".local/bin").mkdir(parents=True)
(workspace / ".haarness").mkdir(parents=True)
(workspace / "notes.txt").write_text("NOTES_BODY\n")
(workspace / ".haarness/config.toml").write_text(
    f'[defaults]\nprovider="anthropic"\nmodel="{MODEL}"\n[permissions]\nread="allow"\n[lsp]\nenabled=false\n')
env = {**ISOLATED, "PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "HOME": str(home), "HAARNESS_HOME": str(home),
       "HAARNESS_DATA_DIR": str(root / "data"), "USER": os.environ.get("USER", "fixture"), "LANG": "en_US.UTF-8",
       "ANTHROPIC_API_KEY": "fixture-key", "ANTHROPIC_BASE_URL": f"http://127.0.0.1:{service.server_port}/v1"}
args = [str(BINARY), "--workspace", str(workspace), "--trust-project"]
connection = {}


def connect():
    connection.clear()
    connection.update(json.loads(run(args + ["desktop"], input='{"action":"connect"}', env=env, timeout=90)))


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


def turn(thread, text):
    started = api(f"/threads/{thread}/turns", {"requestId": str(uuid.uuid4()), "text": text})
    assert "id" in started, started
    return until(lambda: (lambda current: current if current["status"] not in ("queued", "running") else None)(
        api(f"/turns/{started['id']}")), f"turn {text!r} did not finish")


def sent(request):
    return json.dumps(request["messages"])


def errors(thread):
    return [item["data"] for item in api(f"/threads/{thread}/items") if item["data"]["type"] == "error"]


try:
    connect()
    thread = api("/threads", {"provider": "anthropic", "model": MODEL})["id"]
    assert turn(thread, "FIRST_TOPIC hello")["status"] == "completed"
    stopped = turn(thread, "TRIP_WIRE please")
    assert stopped["status"] == "failed", stopped
    [error] = errors(thread)
    assert error["turn_left_out"] is True, error
    assert error["message"] == ("Stopped by the provider's safety check (refusal). The model won't see this "
                                "request again, so you can edit it or pick another model."), error
    items = api(f"/threads/{thread}/items")
    texts = json.dumps([item["data"] for item in items])
    assert "TRIP_WIRE please" in texts and "PARTIAL_WORDS" in texts, "the chat keeps the stopped turn"
    print("PASS a safety stop keeps the turn in the chat and says it is left out", flush=True)

    before = len(CHAT)
    assert turn(thread, "REWORDED ask")["status"] == "completed"
    request = CHAT[before]
    assert "FIRST_TOPIC hello" in sent(request) and "REPLY_OK" in sent(request), sent(request)
    assert "TRIP_WIRE" not in sent(request) and "PARTIAL_WORDS" not in sent(request), sent(request)
    print("PASS the next request goes without the stopped turn, and keeps the turns before it", flush=True)

    # Stopped after a tool call: the call and its result go too.
    tooled = turn(thread, "TOOL_TRIP read the notes")
    assert tooled["status"] == "failed", tooled
    assert any(item["data"]["type"] == "tool_result" and "NOTES_BODY" in item["data"]["output"]
               for item in api(f"/threads/{thread}/items")), "the read ran before the stop"
    before = len(CHAT)
    assert turn(thread, "AFTER_TOOLS ask")["status"] == "completed"
    request = sent(CHAT[before])
    for gone in ("TOOL_TRIP", "toolu_trip", "NOTES_BODY", "PARTIAL_WORDS", "TRIP_WIRE"):
        assert gone not in request, (gone, request)
    assert "REWORDED ask" in request, request
    print("PASS a turn stopped after a tool call leaves the call and its result out too", flush=True)

    # Stored with the stop: a restart keeps leaving the turns out.
    run(args + ["serve", "--stop"], env=env, timeout=60)
    connect()
    before = len(CHAT)
    assert turn(thread, "AFTER_RESTART ask")["status"] == "completed"
    request = sent(CHAT[before])
    assert "AFTER_TOOLS ask" in request, request
    for gone in ("TOOL_TRIP", "NOTES_BODY", "PARTIAL_WORDS", "TRIP_WIRE"):
        assert gone not in request, (gone, request)
    assert [error["turn_left_out"] for error in errors(thread)] == [True, True], errors(thread)
    print("PASS after a restart the stopped turns stay out of the requests", flush=True)
finally:
    if connection:
        e2e_scratch.stop_server(BINARY, workspace, env)
    service.shutdown()
    e2e_scratch.settle(root)
