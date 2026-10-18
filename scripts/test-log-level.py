#!/usr/bin/env python3
"""Log levels change without a restart, reach the workspace server, and export scrubbed.

Drives real, isolated workspace daemons with the echo provider:

- `haarness --log-level debug` starts its workspace server at that level
  (before, the flag stayed in the client and the server logged at info).
- `haarness logs level debug --for 5m` makes a server that is already running
  log at debug within a few seconds, `haarness logs level stop` brings it back,
  and every line names the process that wrote it (pid and role).
- `haarness logs export` packs the logs with credentials removed.
"""
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import time
import urllib.request
import uuid
import zipfile

import e2e_scratch

REPO = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get("HAARNESS_BIN", REPO / "target/debug/haarness")).resolve()

root = Path(tempfile.mkdtemp(prefix="haarness-log-level-")).resolve()
home, workspace, data = root / "home", root / "workspace", root / "data"
home.mkdir()
(workspace / ".haarness").mkdir(parents=True)
(workspace / "notes.txt").write_text("fixture notes\n")
(workspace / ".haarness/config.toml").write_text(
    '[defaults]\nprovider="echo"\nmodel="echo-1"\n[permissions]\nread="allow"\n[lsp]\nenabled=false\n')
env = {"PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "HOME": str(home), "HAARNESS_HOME": str(home),
       "HAARNESS_DATA_DIR": str(data), "USER": os.environ.get("USER", "fixture"), "LANG": "en_US.UTF-8",
       "HAARNESS_TEST_PROVIDER": "1"}
args = [str(BINARY), "--workspace", str(workspace), "--trust-project"]
connection = {}


def run(command, **kwargs):
    return subprocess.run(command, check=True, capture_output=True, text=True, env=env, **kwargs).stdout


def until(check, label, timeout=30):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(0.1)
    raise AssertionError(label)


def connect(*flags):
    connection.clear()
    connection.update(json.loads(run(args[:1] + list(flags) + args[1:] + ["desktop"],
                                     input='{"action":"connect"}', timeout=90)))


def stop():
    run(args + ["serve", "--stop"], timeout=60)
    until(lambda: not daemons(), "the fixture daemon did not stop")


def api(path, body=None):
    request = urllib.request.Request(
        connection["url"] + "/v1" + path, data=None if body is None else json.dumps(body).encode(),
        headers={"Authorization": "Bearer " + connection["token"], "Content-Type": "application/json"})
    with urllib.request.urlopen(request, timeout=30) as response:
        raw = response.read()
        return json.loads(raw) if raw else None


def daemons():
    """This fixture's daemon only: its workspace is a fresh temporary folder."""
    listing = subprocess.run(["ps", "-axo", "pid=,command="], capture_output=True, text=True).stdout
    return [(int(line.split(None, 1)[0]), line) for line in listing.splitlines()
            if str(workspace) in line and " serve" in line]


def tool_turn():
    """A turn that runs one tool: the server logs `tool starting` at debug."""
    thread = api("/threads", {"provider": "echo", "model": "echo-1"})["id"]
    started = api(f"/threads/{thread}/turns", {"requestId": str(uuid.uuid4()),
                                               "text": '!tool read {"path":"notes.txt"}'})
    until(lambda: api(f"/turns/{started['id']}")["status"] == "completed", "the tool turn did not finish")


def log_files():
    # `HAARNESS_DATA_DIR` is the root; the data folder with `logs/` is inside it.
    return sorted(data.glob("**/logs/haarness.log*"))


def log_text():
    return "".join(path.read_text(errors="replace") for path in log_files())


def lines(pid, pattern):
    return [line for line in log_text().splitlines()
            if re.search(rf"Z {pid} serve +{pattern}", line)]


try:
    # --log-level reaches the server it starts.
    connect("--log-level", "debug")
    [(pid, command)] = daemons()
    assert "--log-level debug" in command, command
    tool_turn()
    until(lambda: lines(pid, r"DEBUG .*tool starting"), "the server started with --log-level debug logs no debug")
    stop()
    print("PASS --log-level reaches the workspace server it starts", flush=True)

    # A running server follows `haarness logs level`.
    connect()
    [(pid, command)] = daemons()
    assert "--log-level" not in command, command
    tool_turn()
    time.sleep(0.5)
    assert not lines(pid, r"DEBUG .*tool starting"), "the server logs debug before the level was raised"
    raised = run(args + ["logs", "level", "debug", "--for", "5m"])
    assert raised.startswith("debug until "), raised
    until(lambda: lines(pid, r"INFO .*log level raised for a while"), "the running server did not follow the raised level", 10)
    shown = run(args + ["logs", "level"])
    assert shown.startswith("debug until ") and "left" in shown, shown
    tool_turn()
    until(lambda: lines(pid, r"DEBUG .*tool starting"), "the raised server logs no debug")
    print("PASS a running server logs at the raised level within seconds, without a restart", flush=True)

    stopped = run(args + ["logs", "level", "stop"])
    assert stopped.startswith("Stopped"), stopped
    until(lambda: lines(pid, r"INFO .*log level back to this process's own"), "the server did not go back to its level", 10)
    before = len(lines(pid, r"DEBUG .*tool starting"))
    tool_turn()
    time.sleep(0.5)
    assert len(lines(pid, r"DEBUG .*tool starting")) == before, "the server still logs debug after stop"
    assert run(args + ["logs", "level"]).startswith("each process's own level")
    print("PASS logs level stop brings the running server back to its own level", flush=True)

    # Every line names the process that wrote it.
    assert lines(pid, r"INFO "), "server lines carry the pid and role"
    stop()

    # An export removes credentials.
    secret = "sk-fixture-secret-0123456789abcdef"
    logs = log_files()[-1]
    with logs.open("a") as handle:
        handle.write(f"2026-10-18T00:00:00Z 1 serve  WARN fixture: rejected Authorization: Bearer {secret}\n")
    archive = root / "export.zip"
    printed = run(args + ["logs", "export", "--output", str(archive)])
    assert str(archive) in printed and "Check before sharing" in printed, printed
    with zipfile.ZipFile(archive) as packed:
        names = packed.namelist()
        assert "README.txt" in names and f"logs/{logs.name}" in names, names
        exported = packed.read(f"logs/{logs.name}").decode()
        assert secret not in exported and "[redacted]" in exported, exported[-400:]
        assert "tool starting" in exported, "ordinary lines are kept"
    print("PASS logs export packs the logs with credentials removed", flush=True)
finally:
    if connection and daemons():
        subprocess.run(args + ["serve", "--stop"], env=env, capture_output=True, timeout=60)
    e2e_scratch.settle(root)
