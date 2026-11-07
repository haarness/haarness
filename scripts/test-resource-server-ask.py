#!/usr/bin/env python3
"""An ask written for all of an MCP server's tools covers the model's reads of its resources too.

Drives an isolated workspace daemon and the TUI in a real tmux pane with the echo provider (no paid
model). Two servers expose the same resources; `"mcp__docs__*": "ask"` asks before docs's tools:

- the model reads wiki's resource without asking, and lists both servers' resources without asking;
- its read of docs's resource raises the read's own card (`mcp_resource_read`, `docs:docs://setup`),
  which the attached TUI shows; Allow once reads it, and the next read asks again;
- a person's `@` mention of docs's resource is read into the message without asking, as before;
- Always keeps `docs:*`: another resource of docs is then read without asking.

Usage: HAARNESS_BIN=target/debug/haarness python3 scripts/test-resource-server-ask.py
Pane captures go to RESOURCE_ASK_EVIDENCE (default: the run's folder, kept with HAARNESS_E2E_KEEP=1).
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
import uuid

import e2e_scratch
from e2e_env import clean_env
from e2e_tmux import pane_command

REPO = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get("HAARNESS_BIN", REPO / "target/debug/haarness")).resolve()
FIXTURE = REPO / "crates/haarness-mcp/tests/fixtures/resources.py"

root = Path(tempfile.mkdtemp(prefix="haarness-resource-server-ask-")).resolve()
evidence = Path(os.environ.get("RESOURCE_ASK_EVIDENCE", root / "evidence"))
evidence.mkdir(parents=True, exist_ok=True)
project = root / "project"
(project / ".haarness").mkdir(parents=True)
# Review, so a request a person answers shows as a card rather than going to Auto's reviewer.
(project / ".haarness/config.toml").write_text('[defaults]\napproval_mode="review"\n[lsp]\nenabled=false\n')
server = {"command": sys.executable, "args": [str(FIXTURE)]}
(project / ".haarness/haarness.json").write_text(json.dumps({
    "model": "echo/echo-1",
    "permission": {"mcp__docs__*": "ask"},
    "mcp": {"docs": server, "wiki": server},
}, indent=2) + "\n")
env = clean_env()
env.update(HAARNESS_HOME=str(root / "home"), HAARNESS_DATA_DIR=str(root / "data"), HAARNESS_TEST_PROVIDER="1",
           HAARNESS_NO_CONNECT_PROMPT="1", HAARNESS_SERVER_TOKEN="", HAARNESS_SERVER_PASSWORD="",
           LANG="en_US.UTF-8", LC_ALL="en_US.UTF-8")
args = [str(BINARY), "--workspace", str(project), "--trust-project"]
session = f"haarness-resource-ask-{os.getpid()}"
connection = {}
passed = 0


def api(path, body=None):
    request = urllib.request.Request(
        connection["url"].rstrip("/") + "/v1" + path, data=None if body is None else json.dumps(body).encode(),
        headers={"Authorization": "Bearer " + connection.get("token", ""), "Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            raw = response.read()
            return json.loads(raw) if raw else None
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


def pane():
    return subprocess.run(["tmux", "capture-pane", "-p", "-t", session], capture_output=True, text=True,
                          check=True).stdout


def results(thread):
    return [item["data"] for item in api(f"/threads/{thread}/items") if item["data"]["type"] == "tool_result"]


def start(thread, text, resources=None):
    body = {"requestId": str(uuid.uuid4()), "text": text}
    if resources:
        body["resources"] = resources
    turn = api(f"/threads/{thread}/turns", body)
    assert "id" in turn, turn
    return turn["id"]


def finished(turn):
    return until(lambda: (lambda info: info if info["status"] not in ("queued", "running") else None)(
        api(f"/turns/{turn}")), f"turn {turn} did not finish")


def read(server, uri):
    return "!tool mcp_resource_read " + json.dumps({"server": server, "uri": uri})


def unasked(thread, text):
    """Run `text` as a turn that nobody is asked about; its tool's result."""
    before = len(results(thread))
    turn = start(thread, text)
    finished(turn)
    assert not api("/permissions"), api("/permissions")
    return results(thread)[before]


def asked(thread, text):
    """Start `text` as a turn; the turn, and the card it raises once that is listed."""
    turn = start(thread, text)
    card = until(lambda: (api("/permissions") or [None])[0], "the read did not ask")
    return turn, card


try:
    connection.update(json.loads(subprocess.run(args + ["desktop"], input='{"action":"connect"}', env=env,
                                                capture_output=True, text=True, check=True, timeout=90).stdout))
    until(lambda: {item["name"]: item["status"] for item in api("/mcp")} == {"docs": "connected", "wiki": "connected"},
          "the MCP servers did not connect", 60)
    thread = api("/threads", {"title": "Resource asks", "provider": "echo", "model": "echo-1"})["id"]
    subprocess.run(["tmux", "new-session", "-d", "-x", "120", "-y", "40", "-s", session,
                    pane_command(env, args + ["attach", connection["url"], "--thread", thread])], check=True)
    until(lambda: "Resource asks" in pane() or "/ commands" in pane(), "the TUI did not attach")

    wiki = unasked(thread, read("wiki", "docs://setup"))
    assert wiki["output"] == "# Setup\nRun the fixture.", wiki
    listing = json.loads(unasked(thread, "!tool mcp_resource_list {}")["output"])
    assert sorted({entry["server"] for entry in listing["resources"]}) == ["docs", "wiki"], listing
    ok("a server without an ask rule is read, and every server listed, without asking")

    other = api("/threads", {"title": "Mentions", "provider": "echo", "model": "echo-1"})["id"]
    finished(start(other, "Look at @setup", [{"server": "docs", "uri": "docs://setup", "name": "setup"}]))
    assert not api("/permissions"), api("/permissions")
    sent = [item["data"]["text"] for item in api(f"/threads/{other}/items") if item["data"]["type"] == "user_message"]
    assert any("Run the fixture." in text for text in sent), sent
    ok("a person's @ mention of that server's resource is read without asking, as before")

    turn, card = asked(thread, read("docs", "docs://setup"))
    assert card["permission"] == "mcp_resource_read" and card["patterns"] == ["docs:docs://setup"], card
    assert card["alwaysPatterns"] == ["mcp_resource_read:docs:*"], card
    until(lambda: "Allow once" in pane(), "the TUI did not show the card")
    (evidence / "tui-resource-card.txt").write_text(pane())
    api(f"/permissions/{card['permissionId']}", {"decision": "once"})
    finished(turn)
    assert results(thread)[-1]["output"] == "# Setup\nRun the fixture.", results(thread)[-1]
    turn, card = asked(thread, read("docs", "docs://setup"))
    ok("the model's read of a server asked about as a whole raises the read's own card, shown in the TUI")

    api(f"/permissions/{card['permissionId']}", {"decision": "always"})
    finished(turn)
    assert results(thread)[-1]["output"] == "# Setup\nRun the fixture.", results(thread)[-1]
    logo = unasked(thread, read("docs", "docs://logo"))
    assert logo["output"] == "[image/png attachment: docs://logo]", logo
    ok("Always on that card reads the server's other resources without asking")
finally:
    subprocess.run(["tmux", "kill-session", "-t", session], capture_output=True)
    if connection:
        subprocess.run(args + ["serve", "--stop"], env=env, capture_output=True, timeout=60)
    print(f"{passed} passed", flush=True)
    e2e_scratch.settle(root)
