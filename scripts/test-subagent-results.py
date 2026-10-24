#!/usr/bin/env python3
"""What a parent agent reads when its subagent stopped without a normal reply.

Drives a real, isolated workspace daemon with the echo provider, whose
`::finish <reason> [text]` directive ends a child's response with that finish
reason. Each phase delegates with `!tool task` and reads the parent's `task`
result:

- answer: a child that writes a reply returns it unchanged.
- limit: a child whose reasoning used up the output limit says it stopped at
  its output limit before writing a reply (before: "Subagent completed
  without a text response").
- resumed: a child resumed with `task_id` that stops the same way never
  returns its previous run's reply (before: it did, marked completed).
- refusal: a child the provider's safety check stopped tells the parent not to
  retry it as is, with its partial reply (before: "Send a follow-up message
  to try again"), while the child's own chat keeps the message for a person.
"""
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
import uuid

import e2e_scratch

REPO = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get("HAARNESS_BIN", REPO / "target/debug/haarness")).resolve()

root = Path(tempfile.mkdtemp(prefix="haarness-subagent-results-")).resolve()
home, workspace = root / "home", root / "workspace"
home.mkdir()
(workspace / ".haarness").mkdir(parents=True)
(workspace / ".haarness/config.toml").write_text(
    '[defaults]\nprovider="echo"\nmodel="echo-1"\n[lsp]\nenabled=false\n')
env = {"PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "HOME": str(home), "HAARNESS_HOME": str(home),
       "HAARNESS_DATA_DIR": str(root / "data"), "USER": os.environ.get("USER", "fixture"), "LANG": "en_US.UTF-8",
       "HAARNESS_TEST_PROVIDER": "1"}
args = [str(BINARY), "--workspace", str(workspace), "--trust-project"]
connection = {}


def run(command, **kwargs):
    return subprocess.run(command, check=True, capture_output=True, text=True, **kwargs).stdout


def until(check, label, timeout=30):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(0.05)
    raise AssertionError(label)


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


def delegate(thread, arguments):
    """One parent turn delegating with `arguments`: its `task` result."""
    started = api(f"/threads/{thread}/turns",
                  {"requestId": str(uuid.uuid4()), "text": "!tool task " + json.dumps(arguments)})
    assert "id" in started, started
    until(lambda: api(f"/turns/{started['id']}")["status"] == "completed", "the parent turn did not finish")
    until(lambda: api(f"/threads/{thread}/activity")["activeTurn"] is None, "the parent did not go quiet")
    results = [item["data"] for item in api(f"/threads/{thread}/items")
               if item["turnId"] == started["id"] and item["data"]["type"] == "tool_result"
               and item["data"]["name"] == "task"]
    assert len(results) == 1, results
    return results[0]


def child_of(thread):
    links = [item["data"]["child_thread_id"] for item in api(f"/threads/{thread}/items")
             if item["data"]["type"] == "task_link"]
    assert links, "no subagent was launched"
    return links[-1]


def chat():
    return api("/threads", {"provider": "echo", "model": "echo-1"})["id"]


try:
    connection.update(json.loads(run(args + ["desktop"], input='{"action":"connect"}', env=env, timeout=90)))

    answered = delegate(chat(), {"description": "Answer", "prompt": "plain answer"})
    assert not answered["is_error"], answered
    assert "<task_result>\nEcho: plain answer\n</task_result>" in answered["output"], answered
    print("PASS a subagent that writes a reply returns it unchanged", flush=True)

    limited = delegate(chat(), {"description": "Limit", "prompt": "::finish max_tokens"})
    assert not limited["is_error"], limited
    assert re.search(r"The subagent stopped at its output limit \(\d+ output tokens\) before writing a reply\.",
                     limited["output"]), limited
    assert "without a text response" not in limited["output"], limited
    print("PASS a subagent stopped at its output limit says so", flush=True)

    thread = chat()
    first = delegate(thread, {"description": "Resume", "prompt": "first run"})
    assert "Echo: first run" in first["output"], first
    resumed = delegate(thread, {"description": "Resume", "prompt": "::finish max_tokens", "task_id": child_of(thread)})
    assert "Echo: first run" not in resumed["output"], resumed
    assert "before writing a reply" in resumed["output"], resumed
    print("PASS a resumed subagent never returns its previous run's reply", flush=True)

    thread = chat()
    refused = delegate(thread, {"description": "Refusal", "prompt": "::finish refusal partial words"})
    output = refused["output"]
    assert refused["is_error"], refused
    assert "The provider's safety check stopped this subagent (refusal)." in output, output
    assert "do not retry it as is" in output and "Partial reply before the stop:\npartial words" in output, output
    assert "Send a follow-up message" not in output, output
    child_errors = [item["data"]["message"] for item in api(f"/threads/{child_of(thread)}/items")
                    if item["data"]["type"] == "error"]
    assert any("Send a follow-up message" in message for message in child_errors), child_errors
    print("PASS a refused subagent tells its parent not to retry it; its own chat keeps the person's wording",
          flush=True)
finally:
    try:
        subprocess.run(args + ["serve", "--stop"], env=env, timeout=60, capture_output=True)
    finally:
        e2e_scratch.settle(root)
