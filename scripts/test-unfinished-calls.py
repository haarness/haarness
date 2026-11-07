#!/usr/bin/env python3
"""A tool call left without its result no longer breaks a chat for good.

A mock Anthropic Messages endpoint refuses a request the way Claude does when a
`tool_use` has no `tool_result` in the message right after it (HTTP 400). A
real, isolated workspace daemon runs against it:

1. fork while a call waits: a chat in Review asks to write a file and waits
   for its approval; the chat is forked whole. The fork leaves the waiting call
   out, and a message sent there is answered. Before the fix the fork copied
   the call alone and every turn in it failed.
2. crash mid-call: a `sleep` the chat may run is running when the daemon is
   killed. The next daemon gives that call a result saying it did not finish,
   before the restart notice, and the chat's next message is answered.
3. chats stored before the fix: a call cut off by a crash that an older
   version recovered (no result), and a call an older fork copied without its
   result. Each chat's next message is answered: the request gives the call
   a result saying it did not finish.

Usage: python3 scripts/test-unfinished-calls.py   (HAARNESS_BIN picks the binary;
HAARNESS_E2E_KEEP=1 keeps the fixture folder). No model is paid for.
"""
import http.server
import json
import os
from pathlib import Path
import signal
import sqlite3
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


def unanswered(messages):
    """The `tool_use` ids no `tool_result` answers in the next message."""
    missing = []
    for index, message in enumerate(messages):
        content = message.get("content")
        if message.get("role") != "assistant" or not isinstance(content, list):
            continue
        uses = [block["id"] for block in content if block.get("type") == "tool_use"]
        following = messages[index + 1].get("content") if index + 1 < len(messages) else []
        answered = {block.get("tool_use_id") for block in following or []
                    if isinstance(block, dict) and block.get("type") == "tool_result"}
        missing += [use for use in uses if use not in answered]
    return missing


def last_prompt(messages):
    """The newest person's text, or None when the request answers a tool."""
    content = messages[-1].get("content") if messages else None
    if isinstance(content, str):
        return content
    if any(block.get("type") == "tool_result" for block in content or []):
        return None
    return " ".join(block.get("text", "") for block in content or [] if block.get("type") == "text")


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
        missing = unanswered(messages)
        if missing:
            REFUSED.append(missing)
            detail = ("messages: `tool_use` ids were found without `tool_result` blocks immediately after: "
                      + ", ".join(missing))
            payload = json.dumps({"type": "error", "error": {"type": "invalid_request_error", "message": detail}}).encode()
            self.send_response(400)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)
            return
        REQUESTS.append(body)
        prompt = last_prompt(messages)
        events = [{"type": "message_start", "message": {"id": "msg-" + uuid.uuid4().hex[:8],
                                                        "usage": {"input_tokens": 12, "output_tokens": 0}}}]
        call = None
        if prompt and "WAIT_FOR_APPROVAL" in prompt:
            call = ("write", {"path": "approved.txt", "content": "written after approval\n"})
        elif prompt and "RUN_AND_CRASH" in prompt:
            call = ("bash", {"command": "sleep 30"})
        if call:
            events += [
                {"type": "content_block_start", "index": 0,
                 "content_block": {"type": "tool_use", "id": "toolu_" + uuid.uuid4().hex[:12], "name": call[0], "input": {}}},
                {"type": "content_block_delta", "index": 0,
                 "delta": {"type": "input_json_delta", "partial_json": json.dumps(call[1])}},
                {"type": "content_block_stop", "index": 0},
            ]
            stop = "tool_use"
        else:
            text = "TOOL_DONE" if prompt is None else "REPLY_OK"
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
root = Path(tempfile.mkdtemp(prefix="haarness-unfinished-calls-")).resolve()
home, workspace = root / "home", root / "workspace"
(home / ".local/bin").mkdir(parents=True)
(workspace / ".haarness").mkdir(parents=True)
(workspace / ".haarness/config.toml").write_text(
    f'[defaults]\nprovider="anthropic"\nmodel="{MODEL}"\napproval_mode="review"\n'
    '[permissions]\nread="allow"\nedit="ask"\nbash = { "*" = "ask", "sleep 30*" = "allow" }\n[lsp]\nenabled=false\n')
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


def daemon_pids():
    """This fixture's daemon only: its workspace is a fresh temporary folder."""
    listing = run(["ps", "-axo", "pid=,command="])
    return [int(line.split(None, 1)[0]) for line in listing.splitlines()
            if str(workspace) in line and " serve" in line]


def kill_daemon():
    pids = daemon_pids()
    assert pids, "the fixture daemon is not running"
    for pid in pids:
        os.kill(pid, signal.SIGKILL)
    until(lambda: not daemon_pids(), "the killed daemon is still listed")


def stop_daemon():
    subprocess.run(args + ["serve", "--stop"], env=env, capture_output=True, timeout=30)
    until(lambda: not daemon_pids(), "the stopped daemon is still listed")


def database():
    found = sorted((root / "data").rglob("haarness.db"))
    assert len(found) == 1, found
    return sqlite3.connect(found[0])


def items(thread):
    return api(f"/threads/{thread}/items")


def start(thread, text):
    started = api(f"/threads/{thread}/turns", {"requestId": str(uuid.uuid4()), "text": text})
    assert "id" in started, started
    return started["id"]


def finished(turn):
    return until(lambda: (lambda current: current if current["status"] not in ("queued", "running") else None)(
        api(f"/turns/{turn}")), f"turn {turn} did not finish", timeout=60)


def replies(thread):
    return [item["data"]["text"] for item in items(thread) if item["data"]["type"] == "assistant_message"]


def calls_without_results(thread):
    """What `unanswered` finds in a chat's stored items, by call id."""
    open_calls = {}
    for item in items(thread):
        data = item["data"]
        if data["type"] == "tool_call":
            open_calls[data["call_id"]] = data["name"]
        elif data["type"] == "tool_result":
            open_calls.pop(data["call_id"], None)
    return open_calls


def chat():
    return api("/threads", {"provider": "anthropic", "model": MODEL})["id"]


try:
    connect()

    # 1. A fork while a call waits for its approval.
    source = chat()
    waiting = start(source, "WAIT_FOR_APPROVAL write the file")
    pending = until(lambda: [p for p in api("/permissions") if p["threadId"] == source], "the write did not ask")[0]
    assert calls_without_results(source), "the source's call waits for its result"
    fork = api(f"/threads/{source}/fork", {})
    assert "id" in fork, fork
    assert not calls_without_results(fork["id"]), items(fork["id"])
    assert finished(start(fork["id"], "hello from the fork"))["status"] == "completed", REFUSED
    assert replies(fork["id"])[-1] == "REPLY_OK", replies(fork["id"])
    api("/permissions/" + pending["permissionId"], {"decision": "once"})
    assert finished(waiting)["status"] == "completed", REFUSED
    assert (workspace / "approved.txt").exists()
    assert replies(source)[-1] == "TOOL_DONE", replies(source)
    print("PASS a fork taken while a call waits leaves it out and answers there", flush=True)

    # 2. A crash while a call runs.
    crashed = chat()
    running = start(crashed, "RUN_AND_CRASH wait a while")
    until(lambda: calls_without_results(crashed), "the sleep did not start")
    time.sleep(0.5)
    kill_daemon()
    connect()
    assert api(f"/turns/{running}")["status"] == "interrupted"
    stored = [item["data"] for item in items(crashed)]
    results = [data for data in stored if data["type"] == "tool_result"]
    assert len(results) == 1 and results[0]["is_error"], results
    assert "stopped before this call finished" in results[0]["output"], results
    assert stored.index(results[0]) < max(i for i, data in enumerate(stored) if data["type"] == "error"), stored
    assert finished(start(crashed, "what happened?"))["status"] == "completed", REFUSED
    assert replies(crashed)[-1] == "REPLY_OK", replies(crashed)
    print("PASS a call a crash cut off gets a result and the chat goes on", flush=True)

    # 3. Chats stored before the fix.
    recovered_before = chat()
    cut = start(recovered_before, "WAIT_FOR_APPROVAL write it again")
    until(lambda: calls_without_results(recovered_before), "the second write did not ask")
    old_fork = api(f"/threads/{source}/fork", {})["id"]
    kill_daemon()
    with database() as db:
        # What an older version's recovery left: the turn interrupted, a
        # notice, and no result for its call.
        db.execute("UPDATE turns SET status='interrupted', error='server restarted during active turn' WHERE id=?",
                   (cut,))
        sequence = db.execute("SELECT MAX(sequence) FROM items WHERE thread_id=?", (recovered_before,)).fetchone()[0]
        db.execute("INSERT INTO items(id,thread_id,turn_id,sequence,data_json,created_at) VALUES(?,?,?,?,?,?)",
                   (str(uuid.uuid4()), recovered_before, cut, sequence + 1,
                    json.dumps({"type": "error", "message": "Haarness restarted while this turn was running.",
                                "retryable": True}), "2026-11-07T00:00:00Z"))
        # What an older fork copied: the call, without its result, and no turn.
        sequence = db.execute("SELECT MAX(sequence) FROM items WHERE thread_id=?", (old_fork,)).fetchone()[0]
        db.execute("INSERT INTO items(id,thread_id,turn_id,sequence,data_json,created_at) VALUES(?,?,NULL,?,?,?)",
                   (str(uuid.uuid4()), old_fork, sequence + 1,
                    json.dumps({"type": "tool_call", "call_id": "toolu_copied", "name": "bash",
                                "arguments": {"command": "sleep 120"}}), "2026-11-07T00:00:00Z"))
    connect()
    assert calls_without_results(recovered_before), "the older recovery left the call without a result"
    assert calls_without_results(old_fork) == {"toolu_copied": "bash"}, items(old_fork)
    for thread in (recovered_before, old_fork):
        before = len(REQUESTS)
        assert finished(start(thread, "and now?"))["status"] == "completed", REFUSED
        assert replies(thread)[-1] == "REPLY_OK", replies(thread)
        sent = json.dumps(REQUESTS[before]["messages"])
        assert "did not finish" in sent, sent
    print("PASS chats stored with a call and no result are answered again", flush=True)

    assert not REFUSED, REFUSED
    print("PASS no request left a tool call without its result", flush=True)
finally:
    if connection:
        stop_daemon()
    service.shutdown()
    e2e_scratch.settle(root)
