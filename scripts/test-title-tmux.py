#!/usr/bin/env python3
"""Prove generated titles reach a real TUI while its first turn awaits approval."""
import json
import os
from pathlib import Path
import shlex
import subprocess
import tempfile
import time
import urllib.request


def run(*args, **kwargs):
    return subprocess.check_output(args, text=True, **kwargs)


def until(check, description):
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(0.1)
    raise AssertionError(description)


binary = str(Path(os.environ.get("HAARNESS_BIN", "target/release/haarness")).resolve())
session = f"haarness-title-{os.getpid()}"
with tempfile.TemporaryDirectory(prefix="haarness-title-") as directory:
    root = Path(directory)
    project = root / "project"
    config = project / ".haarness"
    config.mkdir(parents=True)
    (config / "config.toml").write_text(
        '[defaults]\nprovider="echo"\nmodel="echo-1"\n[permissions]\nbash="ask"\n'
    )
    env = dict(os.environ, HAARNESS_DATA_DIR=str(root / "data"), HAARNESS_TEST_PROVIDER="1")
    command = shlex.join([
        "env", f"HAARNESS_DATA_DIR={env['HAARNESS_DATA_DIR']}", "HAARNESS_TEST_PROVIDER=1",
        "HAARNESS_DEFAULT_PROVIDER=echo", "HAARNESS_DEFAULT_MODEL=echo-1",
        binary, "--workspace", str(project), "--trust-project",
    ])
    try:
        connection = json.loads(run(
            binary, "--workspace", str(project), "--trust-project", "desktop",
            input='{"action":"connect"}', env=env,
        ))

        def api(path):
            request = urllib.request.Request(
                connection["url"].rstrip("/") + "/v1" + path,
                headers={"Authorization": "Bearer " + connection["token"]},
            )
            with urllib.request.urlopen(request, timeout=5) as response:
                return json.load(response)

        run("tmux", "new-session", "-d", "-x", "180", "-y", "45", "-s", session, command)

        def pane():
            return run("tmux", "capture-pane", "-p", "-t", session)

        until(lambda: "h a a r n e s s" in pane(), "TUI did not open")
        run("tmux", "send-keys", "-l", "-t", session, ' !tool bash {"command":"printf title-tmux-proof"}')
        run("tmux", "send-keys", "-t", session, "Enter")
        permission = until(lambda: next(iter(api("/permissions")), None), "First task did not await approval")
        thread = until(
            lambda: next((thread for thread in api("/threads?archived=true")
                          if thread["id"] == permission["threadId"] and thread["title"] != "New thread"), None),
            "Title was not generated while the task was pending",
        )
        # No assistant response exists yet; Echo can only appear in the newly
        # generated title, which must reach the visible header without a reload.
        def visible_title():
            value = pane()
            return value if "Echo:" in value else None

        captured = until(visible_title, "Generated title did not appear in the TUI")
        assert any(turn["threadId"] == thread["id"] for turn in api("/turns/active"))
        assert not any(item["data"]["type"] == "assistant_message" for item in api(f"/threads/{thread['id']}/items"))
        evidence = Path(os.environ.get("HAARNESS_TITLE_TMUX_EVIDENCE", "/tmp/haarness-title-tmux.txt"))
        evidence.write_text(captured)
        print(f"PASS: generated title is visible in the running TUI before approval; capture: {evidence}")
    finally:
        subprocess.run(["tmux", "kill-session", "-t", session], capture_output=True)
        subprocess.run([binary, "--workspace", str(project), "serve", "--stop"],
                       env=env, capture_output=True, timeout=30)
