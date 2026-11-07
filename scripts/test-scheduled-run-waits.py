#!/usr/bin/env python3
"""What a scheduled run waits for, and what it stops, through a real daemon.

Drives `haarness schedule run` in an isolated workspace with the echo provider (no paid model).
Background commands there have no time limit unless the agent gives one
(`execution.shell_timeout_ms = 0`), and the run's 30 minutes are shortened to 1.5 s (only the test
provider may do that):

- subagent: a subagent the run's turn starts in the background is waited for, and the record
  carries what the agent said once its result came back; one that fails fails the run, naming it.
- command: a command with a time limit of its own is waited for until it ends, past the run's
  limit, and so is the turn its end starts; one with no time limit holds nothing, is stopped when
  the rest has ended, and is named in the record's note while the run succeeds.
- workflow: a workflow the turn starts in the background is waited for, and the turn its result
  starts; one still going at the limit is cancelled and named.
- limit: what is left at the limit is stopped without telling the chat, and the run fails, naming it.
- ceiling: nothing holds a run past its ceiling (12 hours, shortened to 4 s), not even a command
  with a time limit of its own; the run fails, naming it.
- person: a person's turn and dev server in the run's chat are neither waited for nor stopped.
"""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
import uuid

import e2e_scratch
from e2e_env import clean_env

REPO = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get("HAARNESS_BIN", REPO / "target/debug/haarness")).resolve()

root = Path(tempfile.mkdtemp(prefix="haarness-scheduled-run-waits-")).resolve()
home, workspace = root / "home", root / "workspace"
home.mkdir()
(workspace / ".haarness/workflows").mkdir(parents=True)
(workspace / ".haarness/config.toml").write_text(
    '[defaults]\nprovider="echo"\nmodel="echo-1"\n[permissions]\nbash="allow"\nworkflow="allow"\n'
    "[lsp]\nenabled=false\n[execution]\nshell_timeout_ms=0\nshell_stop_grace_ms=500\n")
(workspace / ".haarness/workflows/quick.ts").write_text(
    'import { workflow } from "@haarness-ai/plugin"\n'
    'export default workflow({ name: "quick", async run() { return "workflow-finished" } })\n')
(workspace / ".haarness/workflows/slow.ts").write_text(
    'import { workflow } from "@haarness-ai/plugin"\n'
    'export default workflow({ name: "slow", async run() {\n'
    '  await new Promise((done) => setTimeout(done, 60000))\n  return "never"\n} })\n')
env = clean_env()
# A workflow runs on Bun: keep the one this machine has on PATH.
bun = shutil.which("bun") or str(Path.home() / ".bun/bin/bun")
env.update(PATH=f"{Path(bun).parent}:/usr/bin:/bin:/usr/sbin:/sbin", HOME=str(home), HAARNESS_HOME=str(home),
           HAARNESS_DATA_DIR=str(root / "data"), LANG="en_US.UTF-8", HAARNESS_TEST_PROVIDER="1",
           HAARNESS_SERVER_TOKEN="", HAARNESS_SERVER_PASSWORD="")
args = [str(BINARY), "--workspace", str(workspace), "--trust-project"]
connection = {}
passed = 0
LIMIT = 1500


def cli(*extra, timeout=120):
    return subprocess.run(args + list(extra), check=True, capture_output=True, text=True, env=env,
                          timeout=timeout).stdout


def api(path, body=None):
    request = urllib.request.Request(
        connection["url"] + "/v1" + path, data=None if body is None else json.dumps(body).encode(),
        headers={"Authorization": "Bearer " + connection.get("token", ""), "Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return json.loads(response.read() or b"null")
    except urllib.error.HTTPError as error:
        return {"http": error.code, "error": error.read().decode()}


def until(check, label, timeout=30):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(0.1)
    raise AssertionError(label)


def ok(message):
    global passed
    passed += 1
    print("PASS " + message, flush=True)


def tool(name, arguments):
    return f"!tool {name} " + json.dumps(arguments)


def schedule(name, prompt):
    cli("schedule", "add", "--name", name, "--every", "1h", "--disabled", "--model", "echo/echo-1",
        "--prompt", prompt)


def runner(name, limit_ms, ceiling_ms=None):
    limits = {"HAARNESS_SCHEDULE_BACKGROUND_LIMIT_MS": str(limit_ms)}
    if ceiling_ms:
        limits["HAARNESS_SCHEDULE_CEILING_MS"] = str(ceiling_ms)
    return subprocess.Popen(args + ["schedule", "run", name, "--json"], stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, text=True, env={**env, **limits})


def record_of(process):
    out, err = process.communicate(timeout=180)
    assert out.strip(), err
    record = json.loads(out)
    assert (process.returncode == 0) == (record["status"] == "success"), (process.returncode, record, err)
    return record


def run(name, limit_ms=LIMIT, ceiling_ms=None):
    """`schedule run --json`: the run's record, how long it took, and the chat's turns once it ended."""
    began = time.monotonic()
    record = record_of(runner(name, limit_ms, ceiling_ms))
    return record, time.monotonic() - began, turns(record["threadId"])


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


def notices(thread):
    return [item for item in api(f"/threads/{thread}/items")
            if item["data"]["type"] == "user_message" and item["data"]["text"].startswith(("<command ", "<workflow "))]


try:
    connection.update(json.loads(subprocess.run(args + ["desktop"], input='{"action":"connect"}', env=env,
                                                capture_output=True, text=True, check=True, timeout=90).stdout))

    # subagent
    schedule("late-reply", tool("task", {"description": "Late reply", "prompt": "!linger 2500 child finished",
                                         "background": True}))
    record, took, before = run("late-reply", 60000)
    assert record["status"] == "success" and "error" not in record, record
    assert took >= 2.5, f"the run ended after {took:.1f} s, before its subagent answered"
    assert "child finished" in record["response"] and 'state="completed"' in record["response"], record
    assert len(before) >= 2, "the subagent's result started no turn of its own"
    settled_after(record, before)
    schedule("failing-check", tool("task", {"description": "Failing check", "prompt": "::finish refusal",
                                            "background": True}))
    record, _, before = run("failing-check", 60000)
    assert record["status"] == "failed", record
    assert record["error"] == "The background subagent “Failing check” failed.", record
    settled_after(record, before)
    ok("a run waits for its background subagent, records what came after, and fails when it fails")

    # commands
    schedule("timed-command", tool("bash", {"command": "sleep 3; echo late-finished", "run_in_background": True,
                                            "timeout": 20000}))
    record, took, before = run("timed-command")
    assert record["status"] == "success" and "error" not in record and "note" not in record, record
    assert took >= 3, f"the run ended after {took:.1f} s, before its command did"
    assert "late-finished" in record["response"] and 'state="exited" exit="0"' in record["response"], record
    settled_after(record, before)
    schedule("dev-server", tool("bash", {"command": "sleep 40", "run_in_background": True}))
    record, took, before = run("dev-server", 60000)
    assert took < 20, f"a command with no time limit held the run for {took:.1f} s"
    assert record["status"] == "success" and "error" not in record, record
    assert record["note"] == ("Stopped as the run ended, having no time limit: command “sleep 40”."), record
    commands = api(f"/threads/{record['threadId']}/commands")
    assert [(c["state"], c.get("endedBy")) for c in commands] == [("stopped", "turn_ended")], commands
    settled_after(record, before)
    assert not notices(record["threadId"]), notices(record["threadId"])
    ok("a command is waited for until its own limit, past the run's; one with no limit ends with the run, named")

    # workflows
    schedule("quick-flow", tool("workflow", {"action": "start", "name": "quick"}))
    record, _, before = run("quick-flow", 60000)
    assert record["status"] == "success" and "error" not in record, record
    assert "Started workflow" in record["response"] and "workflow-finished" in record["response"], record
    settled_after(record, before)
    schedule("slow-flow", tool("workflow", {"action": "start", "name": "slow"}))
    record, took, before = run("slow-flow")
    assert took < 30, f"the run took {took:.1f} s past a 1.5 s limit"
    assert record["status"] == "failed", record
    assert record["error"] == ("Background work was still going after 1.5 seconds and was stopped: "
                               "workflow “slow”."), record
    runs = [entry for entry in api("/workflows/runs") or [] if entry.get("workflowName") == "slow"]
    assert runs and all(entry["status"] == "cancelled" for entry in runs), runs
    settled_after(record, before)
    assert not notices(record["threadId"]), notices(record["threadId"])
    ok("a run waits for its background workflow and the turn its result starts, and cancels one at its limit")

    # limit
    schedule("slow-check", tool("task", {"description": "Slow check", "prompt": "!linger 60000 never",
                                         "background": True}))
    record, took, before = run("slow-check")
    assert took < 30, f"the run took {took:.1f} s past a 1.5 s limit"
    assert record["status"] == "failed", record
    assert record["error"] == ("Background work was still going after 1.5 seconds and was stopped: "
                               "subagent “Slow check”."), record
    assert "never" not in record["response"], record["response"]
    settled_after(record, before)
    ok("past its limit a run stops what is left of its work, untold, and records failed, naming it")

    # ceiling
    schedule("long-command", tool("bash", {"command": "sleep 40", "run_in_background": True, "timeout": 60000}))
    record, took, before = run("long-command", ceiling_ms=4000)
    assert took < 20, f"a command with a time limit held the run for {took:.1f} s past a 4 s ceiling"
    assert record["status"] == "failed", record
    assert record["error"] == ("The run was stopped after 4 seconds, the longest a scheduled run may last: "
                               "command “sleep 40”."), record
    commands = api(f"/threads/{record['threadId']}/commands")
    assert [(c["state"], c.get("endedBy")) for c in commands] == [("stopped", "turn_ended")], commands
    settled_after(record, before)
    assert not notices(record["threadId"]), notices(record["threadId"])
    ok("nothing holds a run past its ceiling, not even a command with a time limit of its own")

    # person
    schedule("person-check", tool("task", {"description": "Person check", "prompt": "!linger 60000 never",
                                           "background": True}))
    process = runner("person-check", 6000)
    thread = until(lambda: next((chat["id"] for chat in api("/threads")
                                 if chat.get("title", "").startswith("Schedule: person-check")), None),
                   "the run's chat never appeared")
    until(lambda: api(f"/threads/{thread}/background"), "the run's subagent never started")
    until(lambda: api(f"/threads/{thread}/activity")["activeTurn"] is None, "the run's first turn never ended")
    started = api(f"/threads/{thread}/turns", {"requestId": str(uuid.uuid4()),
                                                "text": tool("bash", {"command": "sleep 30", "run_in_background": True})})
    until(lambda: api(f"/turns/{started['id']}")["status"] == "completed", "the person's command never started")
    dev = api(f"/threads/{thread}/commands")[0]
    lingering = api(f"/threads/{thread}/turns", {"requestId": str(uuid.uuid4()), "text": "!linger 8000 person typing"})
    record = record_of(process)
    assert record["status"] == "failed", record
    assert record["error"] == ("Background work was still going after 6 seconds and was stopped: "
                               "subagent “Person check”."), record
    assert "person typing" not in record["response"], record["response"]
    assert api(f"/turns/{lingering['id']}")["status"] == "running", "the run stopped the person's turn"
    until(lambda: api(f"/turns/{lingering['id']}")["status"] == "completed", "the person's turn never ended")
    assert api(f"/threads/{thread}/commands/{dev['id']}")["state"] == "running", "the run stopped the person's command"
    api(f"/threads/{thread}/commands/{dev['id']}/stop", {})
    ok("a person's turn and dev server in the run's chat are neither waited for nor stopped")
finally:
    if connection:
        subprocess.run(args + ["serve", "--stop"], env=env, capture_output=True, timeout=60)
    print(f"{passed} passed", flush=True)
    e2e_scratch.settle(root)
