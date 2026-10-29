#!/usr/bin/env python3
"""An agent's shell commands run in the background, through a real daemon.

Drives an isolated workspace daemon with the echo provider (no paid model):

- start: a command started with `run_in_background` answers at once, is
  listed, counted and readable while it runs, and its end reaches the chat
  as a hidden message that starts a turn of its own.
- move: a long foreground command moves to the background by itself only
  in a turn whose client follows background work (`followsBackground`);
  elsewhere the call waits for it, as before.
- person: a person sends a running command to the background and stops it
  through the routes; the agent is told it was stopped.
- ampersand: `sleep 300 & echo started` no longer holds the call until its
  deadline: it answers at once and stops what the command left running.
- agent: `bash_output` matches output the server wrote before the wait
  began, and `bash_stop` ends the command; the agent is not told again.
- run: `haarness run --wait-background` waits for the command and prints
  what the agent says when it ends; without the flag the run says it keeps
  running on the server. A run never moves a command to the background:
  a long one keeps its deadline (`shell_timeout_ms`, 8 s here), one the
  agent starts there stops at it, and a waiting run that gives up stops
  what it waited for, never a command of the chat it did not start.
"""
import json
import os
from pathlib import Path
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

root = Path(tempfile.mkdtemp(prefix="haarness-background-commands-")).resolve()
home, workspace = root / "home", root / "workspace"
home.mkdir()
(workspace / ".haarness").mkdir(parents=True)
(workspace / ".haarness/config.toml").write_text(
    '[defaults]\nprovider="echo"\nmodel="echo-1"\n[permissions]\nbash="allow"\n[lsp]\nenabled=false\n'
    "[execution]\nshell_timeout_ms=8000\nshell_move_after_ms=1500\nshell_stop_grace_ms=500\n")
env = {**ISOLATED, "PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "HOME": str(home), "HAARNESS_HOME": str(home),
       "HAARNESS_DATA_DIR": str(root / "data"), "USER": os.environ.get("USER", "fixture"), "LANG": "en_US.UTF-8",
       "HAARNESS_TEST_PROVIDER": "1"}
args = [str(BINARY), "--workspace", str(workspace), "--trust-project"]
connection = {}
passed = 0


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


def api(path, body=None, method=None):
    request = urllib.request.Request(
        connection["url"] + "/v1" + path, data=None if body is None else json.dumps(body).encode(),
        headers={"Authorization": "Bearer " + connection["token"], "Content-Type": "application/json"},
        method=method or ("GET" if body is None else "POST"))
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            raw = response.read()
            return json.loads(raw) if raw else None
    except urllib.error.HTTPError as error:
        return {"http": error.code, "error": error.read().decode()}


def chat():
    return api("/threads", {"provider": "echo", "model": "echo-1"})["id"]


def turn(thread, text, follows):
    started = api(f"/threads/{thread}/turns",
                  {"requestId": str(uuid.uuid4()), "text": text, "followsBackground": follows})
    assert "id" in started, started
    return started["id"]


def finished(turn_id, timeout=30):
    return until(lambda: api(f"/turns/{turn_id}")["status"] not in ("queued", "running") and api(f"/turns/{turn_id}"),
                 f"turn {turn_id} did not finish", timeout)


def items(thread, kind):
    return [item["data"] for item in api(f"/threads/{thread}/items") if item["data"]["type"] == kind]


def last_result(thread):
    return items(thread, "tool_result")[-1]


def notices(thread):
    return [data["text"] for data in items(thread, "user_message")
            if data.get("synthetic") and data["text"].startswith("<command ")]


def command(thread, command_id):
    return api(f"/threads/{thread}/commands/{command_id}")


def ended(thread, command_id, timeout=30):
    return until(lambda: (lambda info: info if info["state"] not in ("running", "stopping") else None)(
        command(thread, command_id)), f"command {command_id} did not end", timeout)


def quiet(thread):
    """No turn runs and nothing is owed."""
    activity = api(f"/threads/{thread}/activity")
    return activity["activeTurn"] is None and activity["background"] == 0


def ok(message):
    global passed
    passed += 1
    print("PASS " + message, flush=True)


def tool(name, arguments):
    return f"!tool {name} " + json.dumps(arguments)


try:
    connection.update(json.loads(run(args + ["desktop"], input='{"action":"connect"}', env=env, timeout=90)))
    info = api("/info")
    assert "background-commands.v1" in info["capabilities"], info["capabilities"]

    # start
    thread = chat()
    began = time.monotonic()
    started = turn(thread, tool("bash", {"command": "printf 'ready\\n'; sleep 3; printf 'done\\n'; exit 3",
                                         "run_in_background": True}), True)
    finished(started)
    assert time.monotonic() - began < 3, "the call waited for the command"
    result = last_result(thread)
    reference = result["metadata"]["command"]
    assert reference["background"] is True and "runs in the background" in result["output"], result
    listed = api(f"/threads/{thread}/commands")
    assert [entry["id"] for entry in listed] == [reference["id"]], listed
    assert listed[0]["state"] == "running" and listed[0]["movedBy"] == "agent" and listed[0]["deliversNotice"]
    assert any(entry["id"] == reference["id"] for entry in api("/commands/active"))
    assert api(f"/threads/{thread}/activity")["backgroundCommands"] == 1
    until(lambda: api(f"/threads/{thread}/commands/{reference['id']}/output?cursor=0")["text"] == "ready\n",
          "the output was not readable while it ran")
    info = ended(thread, reference["id"])
    assert info["state"] == "exited" and info["exitCode"] == 3, info
    until(lambda: notices(thread) and quiet(thread), "the end never reached the chat")
    told = notices(thread)
    assert len(told) == 1 and told[0].startswith(f'<command id="{reference["id"]}" state="exited" exit="3">'), told
    assert "ready\ndone" in told[0], told[0]
    assert [data["state"] for data in items(thread, "command_state")] == ["running", "exited"]
    page = api(f"/threads/{thread}/commands/{reference['id']}/output?cursor=0&limit=6")
    assert page["text"] == "ready\n" and page["more"] and page["settled"], page
    assert api(f"/threads/{thread}/activity")["backgroundCommands"] == 0
    ok("a command started in the background answers at once and its end starts a turn")

    # move
    thread = chat()
    script = tool("bash", {"command": "printf 'started\\n'; sleep 4; printf 'finished\\n'"})
    began = time.monotonic()
    finished(turn(thread, script, True))
    assert time.monotonic() - began < 4, "the reply waited for the whole command"
    result = last_result(thread)
    assert "moved to the background" in result["output"] and "started" in result["output"], result
    moved = command(thread, result["metadata"]["command"]["id"])
    assert moved["movedBy"] == "limit" and moved["background"], moved
    assert ended(thread, moved["id"])["exitCode"] == 0
    until(lambda: notices(thread) and quiet(thread), "the moved command's end never reached the chat")
    other = chat()
    finished(turn(other, script, False))
    result = last_result(other)
    assert result["output"].startswith("exit: 0\n") and "finished" in result["output"], result
    assert result["metadata"]["command"]["background"] is False
    assert not notices(other)
    ok("a long command moves to the background only where the client follows it")

    # person
    thread = chat()
    held = turn(thread, tool("bash", {"command": "printf 'serving\\n'; sleep 60"}), False)
    command_id = until(lambda: (api(f"/threads/{thread}/commands") or [{}])[0].get("id"), "the command did not start")
    moved = api(f"/threads/{thread}/commands/background", {})
    assert [entry["id"] for entry in moved] == [command_id] and moved[0]["movedBy"] == "person", moved
    finished(held)
    assert "the person sent it to the background" in last_result(thread)["output"]
    refused = api(f"/threads/{thread}/commands/background?commandId={command_id}", {})
    assert refused.get("http") == 409, refused
    stopping = api(f"/threads/{thread}/commands/{command_id}/stop", {})
    assert stopping["state"] in ("stopping", "stopped"), stopping
    info = ended(thread, command_id)
    assert info["state"] == "stopped" and info["endedBy"] == "person", info
    assert api(f"/threads/{thread}/commands/{command_id}/stop", {})["state"] == "stopped"
    until(lambda: notices(thread) and quiet(thread), "the agent was never told of the stop")
    assert 'state="stopped"' in notices(thread)[0], notices(thread)
    ok("a person sends a command to the background and stops it, and the agent is told")

    # ampersand
    thread = chat()
    began = time.monotonic()
    finished(turn(thread, tool("bash", {"command": "sleep 300 & echo started"}), False))
    assert time.monotonic() - began < 5, "a process left running held the call"
    result = last_result(thread)
    assert result["output"].startswith("exit: 0\nstarted") and result["metadata"].get("leftovers"), result
    ok("a process left running with & no longer holds the call")

    # agent
    thread = chat()
    finished(turn(thread, tool("bash", {"command": "printf 'Server READY on 4321\\n'; sleep 60",
                                        "run_in_background": True}), True))
    command_id = last_result(thread)["metadata"]["command"]["id"]
    until(lambda: command(thread, command_id)["outputBytes"] > 0, "the server printed nothing")
    finished(turn(thread, tool("bash_output", {"id": command_id, "wait_seconds": 5, "until": "match",
                                               "pattern": "(?i)ready on \\d+"}), True))
    assert "Matched: Server READY on 4321" in last_result(thread)["output"], last_result(thread)
    finished(turn(thread, tool("bash_stop", {"id": command_id}), True))
    assert "was stopped" in last_result(thread)["output"], last_result(thread)
    assert command(thread, command_id)["endedBy"] == "agent"
    time.sleep(1)
    assert not notices(thread), "the agent was told of its own stop"
    ok("bash_output matches output that came before its wait, and bash_stop ends the command")

    # run
    waited = subprocess.run(args + ["run", "--wait-background", "--background-timeout-ms", "60000",
                                    tool("bash", {"command": "sleep 2; echo run-finished", "run_in_background": True})],
                            env=env, capture_output=True, text=True, timeout=120)
    assert waited.returncode == 0, waited.stderr
    assert 'state="exited" exit="0"' in waited.stdout and "run-finished" in waited.stdout, waited.stdout
    detached = subprocess.run(args + ["run", tool("bash", {"command": "sleep 30", "run_in_background": True})],
                              env=env, capture_output=True, text=True, timeout=120)
    assert detached.returncode == 0, detached.stderr
    assert "1 background command keeps running on the Haarness server." in detached.stderr, detached.stderr
    ok("haarness run --wait-background waits for the command and prints what follows")

    # run limits
    left = [entry for entry in api("/commands/active") if entry["command"] == "sleep 30"]
    assert len(left) == 1 and not left[0].get("movesAt"), left
    began = time.monotonic()
    held = subprocess.run(args + ["run", "--wait-background", tool("bash", {"command": "sleep 25; echo never"})],
                          env=env, capture_output=True, text=True, timeout=120)
    assert time.monotonic() - began < 20, "a run moved a long command and waited for it"
    assert "timed out after 8000 ms" in held.stdout, held.stdout
    gave_up = subprocess.run(args + ["run", "--wait-background", "--background-timeout-ms", "1500",
                                     tool("bash", {"command": "sleep 40", "run_in_background": True})],
                             env=env, capture_output=True, text=True, timeout=120)
    assert gave_up.returncode != 0 and "was stopped" in gave_up.stderr, gave_up.stderr
    until(lambda: not [entry for entry in api("/commands/active") if entry["command"] == "sleep 40"],
          "the run that gave up left its command running")
    detached_chat = left[0]["threadId"]
    info = until(lambda: (lambda info: info if info["state"] not in ("running", "stopping") else None)(
        command(detached_chat, left[0]["id"])), "the detached command outlived its limit", 20)
    assert info["state"] == "timed_out", info
    ok("a run keeps deadlines, stops what it gave up on, and its background commands stop at their limit")

    # A waiting run that continues a chat leaves the commands it did not start alone.
    thread = chat()
    finished(turn(thread, tool("bash", {"command": "echo dev; sleep 120", "run_in_background": True}), True))
    server = last_result(thread)["metadata"]["command"]["id"]
    began = time.monotonic()
    continued = subprocess.run(args + ["run", "--session", thread, "--wait-background", "--background-timeout-ms",
                                       "3000", tool("bash", {"command": "echo quick"})],
                               env=env, capture_output=True, text=True, timeout=120)
    assert continued.returncode == 0, continued.stderr
    assert time.monotonic() - began < 3, "the run waited for a command it did not start"
    assert command(thread, server)["state"] == "running", command(thread, server)
    api(f"/threads/{thread}/commands/{server}/stop", {})
    ok("a run that continues a chat waits for and stops only what it started")
finally:
    if connection:
        subprocess.run(args + ["serve", "--stop"], env=env, capture_output=True, timeout=60)
    print(f"{passed} passed", flush=True)
    e2e_scratch.settle(root)
