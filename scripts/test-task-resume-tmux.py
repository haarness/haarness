#!/usr/bin/env python3
"""Exercise overlapping continuations through a real TUI, daemon and task runners."""
import json
import os
from pathlib import Path
import shlex
import subprocess
import tempfile
import time
import urllib.request
import uuid


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
session = f"haarness-task-resume-{os.getpid()}"
with tempfile.TemporaryDirectory(prefix="haarness-task-resume-") as directory:
    root = Path(directory)
    project = root / "project"
    (project / ".haarness").mkdir(parents=True)
    (project / ".haarness/config.toml").write_text(
        '[defaults]\nprovider="echo"\nmodel="echo-1"\n[permissions]\nbash="allow"\n'
    )
    env = dict(os.environ, HAARNESS_DATA_DIR=str(root / "state"), HAARNESS_TEST_PROVIDER="1")
    connection = None
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
            return run("tmux", "capture-pane", "-p", "-t", session)

        def send(text):
            run("tmux", "load-buffer", "-", input=" " + text)
            run("tmux", "paste-buffer", "-p", "-t", session)
            run("tmux", "send-keys", "-t", session, "Enter")

        command = shlex.join(["env", f"HAARNESS_DATA_DIR={env['HAARNESS_DATA_DIR']}",
                             "HAARNESS_TEST_PROVIDER=1", "HAARNESS_DEFAULT_PROVIDER=echo",
                             "HAARNESS_DEFAULT_MODEL=echo-1", binary, "--workspace", str(project), "--trust-project"])
        run("tmux", "new-session", "-d", "-x", "190", "-y", "48", "-s", session, command)
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
        evidence = Path(os.environ.get("HAARNESS_TASK_RESUME_EVIDENCE", "/tmp/haarness-task-resume-tmux.txt"))
        evidence.write_text(capture)
        print(f"PASS: three overlapping task continuations reached one child exactly once; no conflict; TUI capture: {evidence}")
    except BaseException:
        try:
            Path("/tmp/haarness-task-resume-tmux-failure.txt").write_text(pane())
        except Exception:
            pass
        raise
    finally:
        subprocess.run(["tmux", "kill-session", "-t", session], capture_output=True)
        if connection:
            subprocess.run([binary, "--workspace", str(project), "serve", "--stop"],
                           env=env, capture_output=True, timeout=30)
