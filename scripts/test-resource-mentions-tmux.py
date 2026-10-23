#!/usr/bin/env python3
"""`@` mentions of MCP resources in the TUI, in a real tmux pane on the echo provider.

Two MCP servers expose the same resources. Nothing asks them for their resources until `@` opens; the first
`@` asks each once and a later one within the minute asks nothing. The rows follow the files, each with its
description and server, and filter by name. A pick is a chip; the message goes with it as a mention, so the
server reads the resource into the message and the model gets it (the echo provider repeats it), while the
transcript shows the chip it was picked as. Prompt history brings the chip back, and sending it again reads the
resource again. A resource whose server is gone by then is named in the message as not read.

Usage: HAARNESS_BIN=target/debug/haarness python3 scripts/test-resource-mentions-tmux.py
Pane captures, with their colours, go to RESOURCE_TMUX_EVIDENCE; HAARNESS_E2E_DIR holds the run's state.
RESOURCE_TMUX_THEME=light or dark draws the pane in that variant of the editorial theme.
"""
import json
import os
import subprocess
import sys
import tempfile
import time
import urllib.request
import uuid
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import e2e_scratch  # noqa: E402
from e2e_tmux import pane_command  # noqa: E402

ROOT = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get("HAARNESS_BIN", ROOT / "target/debug/haarness")).resolve()
FIXTURE = ROOT / "crates/haarness-mcp/tests/fixtures/resources.py"
EVIDENCE = Path(os.environ.get("RESOURCE_TMUX_EVIDENCE", tempfile.gettempdir() + "/haarness-resource-mentions-tmux"))
EVIDENCE.mkdir(parents=True, exist_ok=True)
THEME = os.environ.get("RESOURCE_TMUX_THEME", "")


def run(*args, **kwargs):
    return subprocess.check_output([str(arg) for arg in args], text=True, **kwargs)


def until(check, message, timeout=40):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if result := check():
            return result
        time.sleep(0.1)
    raise AssertionError(message)


root = Path(tempfile.mkdtemp(prefix="haarness-resource-mentions-", dir=os.environ.get("HAARNESS_E2E_DIR"))).resolve()
project = root / "project"
(project / ".haarness").mkdir(parents=True)
(project / "settings.md").write_text("A file the `@set` query finds too.\n")
requests = root / "requests.log"
server = {"command": sys.executable, "args": [str(FIXTURE)], "env": {"REQUEST_LOG": str(requests)}}
(project / ".haarness/haarness.json").write_text(
    json.dumps({"model": "echo/echo-1", "mcp": {"docs": server, "wiki": server}}, indent=2) + "\n"
)
env = {k: v for k, v in os.environ.items() if not k.startswith("HAARNESS_")}
env.update(
    HAARNESS_HOME=str(root / "home"),
    HAARNESS_DATA_DIR=str(root / "data"),
    HAARNESS_TEST_PROVIDER="1",
    HAARNESS_NO_CONNECT_PROMPT="1",
    LANG="en_US.UTF-8",
    LC_ALL="en_US.UTF-8",
    COLORTERM="truecolor",
)
session = f"haarness-resource-mentions-{os.getpid()}"
checks = []
try:
    connection = json.loads(run(BINARY, "--workspace", project, "--trust-project", "desktop", input='{"action":"connect"}', env=env))

    def api(path, body=None):
        request = urllib.request.Request(
            connection["url"].rstrip("/") + "/v1" + path,
            data=None if body is None else json.dumps(body).encode(),
            headers={"Authorization": "Bearer " + connection["token"], "Content-Type": "application/json"},
        )
        with urllib.request.urlopen(request, timeout=40) as response:
            raw = response.read()
            return json.loads(raw) if raw else None

    def pane():
        return run("tmux", "capture-pane", "-p", "-t", session)

    def keys(*values):
        run("tmux", "send-keys", "-t", session, *values)

    def text(value):
        run("tmux", "send-keys", "-l", "-t", session, value)

    def screen(name):
        name = f"{name}-{THEME}" if THEME else name
        (EVIDENCE / f"{name}.txt").write_text(pane())
        (EVIDENCE / f"{name}.ansi").write_text(run("tmux", "capture-pane", "-p", "-e", "-t", session))

    def listings():
        return requests.read_text().splitlines().count("resources/list") if requests.exists() else 0

    def idle():
        until(lambda: not api("/turns/active"), "the turn did not finish")

    def sent():
        thread = api("/threads")[0]["id"]
        return [item["data"] for item in api(f"/threads/{thread}/items") if item["data"]["type"] == "user_message"]

    def prompts():
        """The rows of the prompts' own blocks, not the echoed replies."""
        return "\n".join(line for line in pane().splitlines() if line.startswith("▎"))

    def selected():
        return next((line for line in pane().splitlines() if "›" in line and "@" in line), "")

    until(lambda: {item["name"]: item["status"] for item in api("/mcp")} == {"docs": "connected", "wiki": "connected"},
          "the MCP servers did not connect", 60)
    run("tmux", "new-session", "-d", "-x", "110", "-y", "50", "-s", session,
        pane_command(env, [BINARY, "--workspace", project, "--trust-project"]))
    until(lambda: "/ commands" in pane(), "the TUI did not open")
    if THEME:
        text(f"/themes editorial {THEME}")
        keys("Enter")
    time.sleep(1)
    assert listings() == 0, requests.read_text()
    checks.append("nothing asks the MCP servers for their resources before `@`")

    text("@set")
    until(lambda: "Setup guide · wiki" in pane(), "the resource rows did not come")
    until(lambda: listings() == 2, "each server was not asked once")
    rows = pane()
    assert "files · agents · resources" in rows, rows
    assert "settings.md" in rows and "Setup guide · docs" in rows, rows
    assert rows.index("settings.md") < rows.index("Setup guide · docs"), rows
    assert "logo" not in rows, rows
    screen("tui-resource-rows")
    checks.append("`@set` lists the matching resources of both servers after the file, with description and server")

    # The wiki server's copy: down to its row, Enter picks it.
    for _ in range(6):
        if "· wiki" in selected():
            break
        keys("Down")
        time.sleep(0.1)
    assert "· wiki" in selected(), pane()
    keys("Enter")
    until(lambda: "[@setup]" in pane(), "the pick did not become a chip")
    assert "Attached @setup" in pane(), pane()
    text("compare")
    screen("tui-resource-chip")
    keys("Enter")
    idle()
    until(lambda: "Run the fixture." in pane(), "the echo reply did not show what was read")
    message = sent()[-1]
    assert message["text"].startswith(
        "compare\n\n<details>\n<summary>MCP resource: setup · wiki:docs://setup</summary>\n\n```\n# Setup\nRun the fixture.\n```"
    ), message
    assert "@setup" in prompts() and "<details>" not in prompts(), prompts()
    screen("tui-resource-sent")
    checks.append("a pick is a [@setup] chip; sent, the server reads it into the message and the transcript shows @setup")

    # A later `@` within the minute asks nothing.
    text("@lo")
    until(lambda: "docs://logo · docs" in pane(), "the logo rows did not come")
    assert listings() == 2, requests.read_text()
    keys("Escape")
    for _ in range(3):
        keys("BSpace")
    checks.append("a second `@` within the minute reuses the listing")

    # History brings the chip back; the wiki server is gone by the time it is sent.
    keys("Up")
    until(lambda: "[@setup] compare" in pane(), "history did not bring the chip back")
    api("/mcp/wiki/disconnect", {})
    keys("Enter")
    idle()
    message = until(lambda: (items := sent()) and len(items) == 2 and items[-1], "the recalled prompt was not sent")
    assert "<summary>MCP resource: setup · wiki:docs://setup (not read)</summary>" in message["text"], message
    assert "MCP server wiki is not connected" in message["text"], message
    until(lambda: "@setup (not read)" in prompts(), "the transcript did not say it was not read")
    screen("tui-resource-not-read")
    checks.append("history brings the chip back and sending it reads it again; a server gone by then is named as not read")
    for check in checks:
        print("ok -", check)
    print(f"evidence: {EVIDENCE}")
finally:
    subprocess.run(["tmux", "kill-session", "-t", session], capture_output=True)
    subprocess.run([str(BINARY), "--workspace", str(project), "serve", "--stop"], env=env, capture_output=True)
    e2e_scratch.settle(root)
