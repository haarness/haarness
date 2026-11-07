#!/usr/bin/env python3
"""A scheduled run follows the work its turn leaves in the background, through a real daemon.

Drives `haarness schedule run` in an isolated workspace with the echo provider (no paid model):

- subagent: a subagent the run's turn starts in the background is waited for; the run's record
  carries what the agent said once its result came back, and no turn starts in the chat after it.
- failed subagent: one that fails records the run as failed, naming it.
- command: a command the turn starts in the background is waited for as before, with the turn its
  end starts in the record.
- limit: what is left once the run's limit has passed (1.5 s here, which only the test provider may
  set), a subagent or a command, is stopped without telling the chat, and the run records failed,
  naming it.
"""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
import urllib.error
import urllib.request

import e2e_scratch
from e2e_env import clean_env

REPO = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get("HAARNESS_BIN", REPO / "target/debug/haarness")).resolve()

root = Path(tempfile.mkdtemp(prefix="haarness-schedule-background-")).resolve()
home, workspace = root / "home", root / "workspace"
home.mkdir()
(workspace / ".haarness").mkdir(parents=True)
(workspace / ".haarness/config.toml").write_text(
    '[defaults]\nprovider="echo"\nmodel="echo-1"\n[permissions]\nbash="allow"\n[lsp]\nenabled=false\n'
    "[execution]\nshell_timeout_ms=60000\nshell_stop_grace_ms=500\n")
env = clean_env()
env.update(PATH="/usr/bin:/bin:/usr/sbin:/sbin", HOME=str(home), HAARNESS_HOME=str(home),
           HAARNESS_DATA_DIR=str(root / "data"), LANG="en_US.UTF-8", HAARNESS_TEST_PROVIDER="1",
           HAARNESS_SERVER_TOKEN="", HAARNESS_SERVER_PASSWORD="")
args = [str(BINARY), "--workspace", str(workspace), "--trust-project"]
connection = {}
passed = 0


def cli(*extra, timeout=120):
    return subprocess.run(args + list(extra), check=True, capture_output=True, text=True, env=env,
                          timeout=timeout).stdout


def api(path):
    request = urllib.request.Request(connection["url"] + "/v1" + path,
                                     headers={"Authorization": "Bearer " + connection.get("token", "")})
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return json.loads(response.read() or b"null")
    except urllib.error.HTTPError as error:
        return {"http": error.code, "error": error.read().decode()}


def ok(message):
    global passed
    passed += 1
    print("PASS " + message, flush=True)


def tool(name, arguments):
    return f"!tool {name} " + json.dumps(arguments)


def schedule(name, prompt):
    cli("schedule", "add", "--name", name, "--every", "1h", "--disabled", "--model", "echo/echo-1",
        "--prompt", prompt)


def run(name, limit_ms=None):
    """`schedule run --json`: the run's record, how long it took, and the chat's turns once it ended."""
    extra = {"HAARNESS_SCHEDULE_BACKGROUND_LIMIT_MS": str(limit_ms)} if limit_ms else {}
    began = time.monotonic()
    done = subprocess.run(args + ["schedule", "run", name, "--json"], capture_output=True, text=True,
                          env={**env, **extra}, timeout=180)
    took = time.monotonic() - began
    assert done.stdout.strip(), done.stderr
    record = json.loads(done.stdout)
    assert (done.returncode == 0) == (record["status"] == "success"), (done.returncode, record, done.stderr)
    return record, took, turns(record["threadId"])


def turns(thread):
    return {item["turnId"] for item in api(f"/threads/{thread}/items") if item.get("turnId")}


def settled_after(record, before):
    """Nothing runs in the run's chat a while after it ended, and no turn started there since."""
    thread = record["threadId"]
    time.sleep(2)
    activity = api(f"/threads/{thread}/activity")
    assert activity["activeTurn"] is None and activity["background"] == 0, activity
    assert activity["backgroundCommands"] == 0, activity
    assert not api(f"/threads/{thread}/background"), api(f"/threads/{thread}/background")
    assert turns(thread) == before, (sorted(before), sorted(turns(thread)))


try:
    connection.update(json.loads(subprocess.run(args + ["desktop"], input='{"action":"connect"}', env=env,
                                                capture_output=True, text=True, check=True, timeout=90).stdout))

    # subagent
    schedule("late-reply", tool("task", {"description": "Late reply", "prompt": "!linger 2500 child finished",
                                         "background": True}))
    record, took, before = run("late-reply")
    assert record["status"] == "success" and "error" not in record, record
    assert took >= 2.5, f"the run ended after {took:.1f} s, before its subagent answered"
    assert "child finished" in record["response"], record["response"]
    assert 'state="completed"' in record["response"], record["response"]
    assert len(before) >= 2, "the subagent's result started no turn of its own"
    settled_after(record, before)
    ok("a run waits for its background subagent and records what the agent said once its result came back")

    # failed subagent
    schedule("failing-check", tool("task", {"description": "Failing check", "prompt": "::finish refusal",
                                            "background": True}))
    record, _, before = run("failing-check")
    assert record["status"] == "failed", record
    assert record["error"] == "The background subagent “Failing check” failed.", record
    assert 'state="error"' in record["response"], record["response"]
    settled_after(record, before)
    ok("a run whose background subagent failed records failed, naming it")

    # command
    schedule("background-command", tool("bash", {"command": "sleep 1; echo command-finished",
                                                 "run_in_background": True}))
    record, _, before = run("background-command")
    assert record["status"] == "success" and "error" not in record, record
    assert "command-finished" in record["response"] and 'state="exited" exit="0"' in record["response"], record
    settled_after(record, before)
    ok("a run waits for its background command and the turn its end starts, as before")

    # limit
    schedule("slow-check", tool("task", {"description": "Slow check", "prompt": "!linger 60000 never",
                                         "background": True}))
    record, took, before = run("slow-check", limit_ms=1500)
    assert took < 30, f"the run took {took:.1f} s past a 1.5 s limit"
    assert record["status"] == "failed", record
    assert record["error"] == ("Background work was still going after 1.5 seconds and was stopped: "
                               "subagent “Slow check”."), record
    assert "never" not in record["response"], record["response"]
    settled_after(record, before)
    schedule("slow-command", tool("bash", {"command": "sleep 40", "run_in_background": True}))
    record, took, before = run("slow-command", limit_ms=1500)
    assert took < 30, f"the run took {took:.1f} s past a 1.5 s limit"
    assert record["status"] == "failed", record
    assert record["error"] == ("Background work was still going after 1.5 seconds and was stopped: "
                               "command “sleep 40”."), record
    commands = api(f"/threads/{record['threadId']}/commands")
    assert [command["state"] for command in commands] == ["stopped"], commands
    settled_after(record, before)
    told = [item for item in api(f"/threads/{record['threadId']}/items")
            if item["data"]["type"] == "user_message" and item["data"]["text"].startswith("<command ")]
    assert not told, told
    ok("past its limit a run stops what is left, untold, and records failed, naming it")
finally:
    if connection:
        subprocess.run(args + ["serve", "--stop"], env=env, capture_output=True, timeout=60)
    print(f"{passed} passed", flush=True)
    e2e_scratch.settle(root)
