#!/usr/bin/env python3
"""A background subagent a restart stops is reported to the chat that launched it.

Drives a real, isolated workspace daemon with the echo provider. Each phase
launches a subagent in the background that keeps working for two minutes,
then stops the daemon while it runs, starts it again and reads the launching
chat:

- crash: the daemon is killed. The chat gets the task's result, an error
  naming the restart and the task_id, for the model to read with the next
  message, and a notice the person sees; no turn starts by itself.
- graceful: `serve --stop`. Same report; the task is not marked cancelled.
- busy: the chat's own turn was running too. Same report, no turn.

The person's next message then runs as usual. In the crash phase a shell
command is also running (`sleep`, which writes nothing and so outlived the
killed daemon under launchd): the next daemon for the folder stops it, and
leaves a process the killed one never started alone.

Before this change none of the chats heard anything: the subagent stayed
"interrupted" (or "cancelled") in the task list and the model never knew.
"""
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
import uuid

import e2e_scratch
from e2e_env import ISOLATED

REPO = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get("HAARNESS_BIN", REPO / "target/debug/haarness")).resolve()

root = Path(tempfile.mkdtemp(prefix="haarness-background-restart-")).resolve()
home, workspace = root / "home", root / "workspace"
home.mkdir()
(workspace / ".haarness").mkdir(parents=True)
(workspace / ".haarness/config.toml").write_text(
    '[defaults]\nprovider="echo"\nmodel="echo-1"\n[permissions]\nbash="allow"\n[lsp]\nenabled=false\n')
env = {**ISOLATED, "PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "HOME": str(home), "HAARNESS_HOME": str(home),
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


def launch(text):
    thread = api("/threads", {"provider": "echo", "model": "echo-1"})["id"]
    started = api(f"/threads/{thread}/turns", {"requestId": str(uuid.uuid4()), "text": text})
    assert "id" in started, started

    def child():
        for item in api(f"/threads/{thread}/items"):
            data = item["data"]
            if data["type"] == "task_state" and data["state"] == "background":
                activity = api(f"/threads/{data['child_thread_id']}/activity")
                if activity.get("activeTurn"):
                    return data["child_thread_id"]
        return None

    return thread, started["id"], until(child, "the background subagent did not start")


def task(description):
    prompt = "!linger 120000 child finished"
    return "!tool task " + json.dumps({"description": description, "prompt": prompt, "background": True})


def notices(thread):
    return [item["data"]["text"] for item in api(f"/threads/{thread}/items")
            if item["data"]["type"] == "user_message" and item["data"].get("synthetic")]


def states(thread):
    return [item["data"]["state"] for item in api(f"/threads/{thread}/items") if item["data"]["type"] == "task_state"]


def replies(thread):
    return [item["data"]["text"] for item in api(f"/threads/{thread}/items")
            if item["data"]["type"] == "assistant_message"]


def shown(thread):
    """The errors the chat shows the person."""
    return [item["data"]["message"] for item in api(f"/threads/{thread}/items") if item["data"]["type"] == "error"]


def reported(thread, child, description):
    texts = notices(thread)
    assert len(texts) == 1, texts
    text = texts[0]
    assert text.startswith(f'<task id="{child}" state="error">'), text
    assert f"{description}: failed" in text and "not resumed" in text and f'task_id "{child}"' in text, text
    visible = [message for message in shown(thread) if message.startswith("Haarness restarted before the background task")]
    assert len(visible) == 1 and f"\u201c{description}\u201d" in visible[0], shown(thread)
    time.sleep(1)
    assert api(f"/threads/{thread}/activity")["activeTurn"] is None, "a turn started by itself after the restart"
    assert not any(text.startswith('Echo: <task id="') for text in replies(thread)), replies(thread)


def running(marker):
    return [line for line in run(["ps", "-axo", "pid=,command="]).splitlines() if marker in line]


try:
    connect()
    # crash
    crash, crash_turn, crash_child = launch(task("Crash audit"))
    until(lambda: api(f"/turns/{crash_turn}")["status"] == "completed", "the launching turn did not finish")
    # A shell command that writes nothing, running when the daemon dies; and a
    # process of the same kind the daemon never started.
    marker = "left-running-" + uuid.uuid4().hex[:8]
    shell = api("/threads", {"provider": "echo", "model": "echo-1"})["id"]
    api(f"/threads/{shell}/turns", {"requestId": str(uuid.uuid4()),
                                    "text": "!tool bash " + json.dumps({"command": f"sleep 600; echo {marker}"})})
    until(lambda: running(marker), "the shell command did not start")
    unrelated = subprocess.Popen(["/bin/sh", "-c", f"sleep 600; echo unrelated-{marker}"], start_new_session=True)
    pids = daemon_pids()
    assert pids, "the fixture daemon is not running"
    for pid in pids:
        os.kill(pid, signal.SIGKILL)
    until(lambda: not daemon_pids(), "the killed daemon is still listed")
    assert running(marker), "the shell command died with the daemon"
    connect()
    until(lambda: not [line for line in running(marker) if "unrelated-" not in line],
          "the next daemon left the killed one's command running")
    assert running("unrelated-" + marker), "a process the daemon never started was stopped"
    os.killpg(unrelated.pid, signal.SIGKILL)
    unrelated.wait()
    print("PASS the next daemon stops a command a crash left running, and nothing else", flush=True)
    until(lambda: notices(crash), "the crashed task was never reported")
    reported(crash, crash_child, "Crash audit")
    assert states(crash) == ["background", "interrupted"], states(crash)
    asked = api(f"/threads/{crash}/turns", {"requestId": str(uuid.uuid4()), "text": "what happened?"})
    until(lambda: api(f"/turns/{asked['id']}")["status"] == "completed", "the next message did not run")
    assert replies(crash)[-1] == "Echo: what happened?", replies(crash)
    print("PASS a subagent a crash stopped is reported to its chat without starting a turn", flush=True)

    # graceful
    graceful, graceful_turn, graceful_child = launch(task("Graceful audit"))
    until(lambda: api(f"/turns/{graceful_turn}")["status"] == "completed", "the launching turn did not finish")
    run(args + ["serve", "--stop"], env=env, timeout=60)
    until(lambda: not daemon_pids(), "the stopped daemon is still listed")
    connect()
    until(lambda: notices(graceful), "the stopped task was never reported")
    reported(graceful, graceful_child, "Graceful audit")
    assert states(graceful) == ["background", "interrupted"], states(graceful)
    assert len(notices(crash)) == 1, "a task is reported once"
    print("PASS a subagent a graceful stop stopped is reported, not marked cancelled", flush=True)

    # busy: the launching turn lingers after the launch, so the restart stops it too.
    busy, busy_turn, busy_child = launch("!linger 120000 " + task("Busy audit"))
    assert api(f"/turns/{busy_turn}")["status"] == "running"
    for pid in daemon_pids():
        os.kill(pid, signal.SIGKILL)
    until(lambda: not daemon_pids(), "the killed daemon is still listed")
    connect()
    until(lambda: notices(busy), "the busy chat's task was never reported")
    reported(busy, busy_child, "Busy audit")
    assert api(f"/turns/{busy_turn}")["status"] == "interrupted"
    print("PASS a chat the restart stopped gets the same report", flush=True)
finally:
    if connection:
        subprocess.run(args + ["serve", "--stop"], env=env, capture_output=True, timeout=60)
    e2e_scratch.settle(root)
