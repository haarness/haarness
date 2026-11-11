#!/usr/bin/env python3
"""Exercise overlapping continuations through a real TUI, daemon and task runners.

The TUI runs in a tmux server of the run's own, and everything the run writes stays in its temporary
folder (HAARNESS_E2E_KEEP=1 keeps it; HAARNESS_TASK_RESUME_EVIDENCE names a file for the TUI's capture).
"""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
import urllib.request
import uuid

import e2e_scratch
from e2e_env import clean_env
from e2e_tmux import pane_command


def run(*args, **kwargs):
    return subprocess.check_output(args, text=True, **kwargs)


def until(check, description, timeout=30):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(0.05)
    raise AssertionError(description)


binary = str(Path(os.environ.get("HAARNESS_BIN", "target/debug/haarness")).resolve())
session = "task-resume"
root = Path(tempfile.mkdtemp(prefix="haarness-task-resume-")).resolve()
project = root / "project"
(project / ".haarness").mkdir(parents=True)
(project / ".haarness/config.toml").write_text(
    '[defaults]\nprovider="echo"\nmodel="echo-1"\n[permissions]\nbash="allow"\n'
)
env = clean_env()
env.update(HAARNESS_DATA_DIR=str(root / "state"), HAARNESS_TEST_PROVIDER="1")
# Its socket in the run's folder, so the stop reaches the server the start made whatever TMUX_TMPDIR says.
tmux = ["tmux", "-S", str(root / "tmux"), "-f", "/dev/null"]
try:
    connection = json.loads(run(binary, "--workspace", str(project), "--trust-project", "desktop",
                                input='{"action":"connect"}', env=env))

    def api(path, body=None):
        request = urllib.request.Request(connection["url"].rstrip("/") + "/v1" + path,
            data=json.dumps(body).encode() if body is not None else None,
            headers={"Authorization": "Bearer " + connection["token"], "Content-Type": "application/json"})
        with urllib.request.urlopen(request, timeout=10) as response:
            return json.load(response)

    def pane():
        return run(*tmux, "capture-pane", "-p", "-t", session, env=env)

    def send(text):
        run(*tmux, "load-buffer", "-", input=" " + text, env=env)
        run(*tmux, "paste-buffer", "-p", "-t", session, env=env)
        run(*tmux, "send-keys", "-t", session, "Enter", env=env)

    command = pane_command({**env, "HAARNESS_DEFAULT_PROVIDER": "echo", "HAARNESS_DEFAULT_MODEL": "echo-1"},
                           [binary, "--workspace", str(project), "--trust-project"])
    run(*tmux, "new-session", "-d", "-x", "190", "-y", "48", "-s", session, command, env=env)
    until(lambda: "h a a r n e s s" in pane(), "TUI did not open")
    send('!tool task {"agent":"general","prompt":"initial resume fixture"}')
    child = until(lambda: next((thread for thread in api("/threads?archived=true") if thread.get("parentId")), None),
                  "initial child was not created")
    parent = child["parentId"]
    until(lambda: not api("/turns/active"), "initial child did not finish")
    blocker = api("/threads", {"provider":"echo", "model":"echo-1", "title":"Capacity fixture"})
    calls = [{"name":"task", "arguments":{"agent":"general", "background":True,
              "prompt":'!tool bash {"command":"sleep 2"}'}} for _ in range(4)]
    api(f"/threads/{blocker['id']}/turns", {"requestId":str(uuid.uuid4()), "text":"!multi " + json.dumps(calls)})
    until(lambda: len(api(f"/threads/{blocker['id']}/background")) == 4, "agent slots were not occupied")
    prompts = [f'!tool bash {{"command":"sleep 0.3; printf resume-proof-{index}"}}' for index in range(3)]
    calls = [{"name":"task", "arguments":{"agent":"general", "task_id":child["id"], "prompt":prompt}}
             for prompt in prompts]
    send("!multi " + json.dumps(calls))
    until(lambda: any(turn["threadId"] == parent for turn in api("/turns/active")), "parent did not resume")
    until(lambda: not api("/turns/active") and not api(f"/threads/{parent}/background"), "continuations did not settle")
    items = api(f"/threads/{parent}/items")
    errors = [item["data"]["output"] for item in items if item["data"]["type"] == "tool_result"
              and item["data"].get("name") == "task" and item["data"].get("is_error")]
    assert not errors, errors
    child_items = api(f"/threads/{child['id']}/items")
    for prompt in prompts:
        assert sum(item["data"].get("text") == prompt for item in child_items) == 1, prompt
    children = [thread for thread in api("/threads?archived=true") if thread.get("parentId") == parent]
    assert len(children) == 1, "continuations created duplicate children"
    capture = until(lambda: pane() if "resume-proof" in pane() else None, "completion did not reach TUI")
    assert "child thread already has an active turn" not in capture
    evidence = Path(os.environ.get("HAARNESS_TASK_RESUME_EVIDENCE", root / "tui.txt"))
    evidence.write_text(capture)
    shown = f"; TUI capture: {evidence}" if "HAARNESS_TASK_RESUME_EVIDENCE" in os.environ else ""
    print(f"PASS: three overlapping task continuations reached one child exactly once; no conflict{shown}")
except BaseException:
    try:
        (root / "tui-failure.txt").write_text(pane())
    except Exception:
        pass
    raise
finally:
    subprocess.run(tmux + ["kill-server"], capture_output=True, env=env)
    e2e_scratch.stop_server(binary, project, env)
    e2e_scratch.settle(root)
