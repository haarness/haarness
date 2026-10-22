#!/usr/bin/env python3
"""Real attached TUI: TODO tools update the sidebar without transcript copies."""
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import time
import urllib.request
from uuid import uuid4

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "sdks/tests"))
from server import Fixture, BINARY


def run(*args):
    return subprocess.check_output(args, text=True)


def until(check, description):
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        result = check()
        if result:
            return result
        time.sleep(0.1)
    raise AssertionError(description)


server = Fixture()
session = f"haarness-todo-sidebar-{os.getpid()}"
evidence = []
try:
    server.start()

    def api(path, body=None):
        request = urllib.request.Request(
            server.url + "/v1" + path,
            data=None if body is None else json.dumps(body).encode(),
            headers={"Authorization": "Bearer " + server.token,
                     "Content-Type": "application/json"},
        )
        with urllib.request.urlopen(request, timeout=10) as response:
            return json.load(response)

    thread = api("/threads", {"title": "TODO sidebar proof", "provider": "echo", "model": "echo-1"})
    tid = thread["id"]
    command = shlex.join([
        "env", f"HOME={server.home}", f"HAARNESS_DATA_DIR={server.data}",
        "HAARNESS_TEST_PROVIDER=1", f"HAARNESS_SERVER_TOKEN={server.token}",
        str(BINARY), "attach", server.url, "--thread", tid,
    ])

    def attach():
        run("tmux", "new-session", "-d", "-x", "170", "-y", "45", "-s", session, command)
        until(lambda: "h a a r n e s s" in pane(), "TUI did not attach")

    def pane():
        return run("tmux", "capture-pane", "-p", "-t", session)

    def items():
        return api(f"/threads/{tid}/items")

    def tool(name, arguments):
        previous = len(items())
        api(f"/threads/{tid}/turns", {
            "requestId": str(uuid4()), "text": f"!tool {name} " + json.dumps(arguments),
            "synthetic": True, "provider": "echo", "model": "echo-1",
        })
        until(lambda: any(i["data"]["type"] == "tool_result" and i["data"].get("name") == name
                          for i in items()[previous:]), f"{name} did not execute")
        until(lambda: not api("/turns/active"), "turn did not complete")

    def assert_sidebar():
        def visible():
            screen = pane()
            return screen if "todo-alpha-proof" in screen and "todo-beta-proof" in screen else None
        screen = until(visible, "TODO sidebar missing")
        assert "▾ todo" in screen and "# Todos" not in screen, screen
        assert screen.count("todo-alpha-proof") == 1, screen
        assert screen.count("todo-beta-proof") == 1, screen
        assert "Updating todos" not in screen, screen
        assert "ctrl+b shows the plan" not in screen, screen
        evidence.append(screen)

    attach()
    entries = [
        {"content": "todo-alpha-proof", "status": "in_progress", "priority": "high"},
        {"content": "todo-beta-proof", "status": "pending", "priority": "medium"},
    ]
    tool("todo", {"items": entries})
    assert_sidebar()
    assert any(i["data"]["type"] == "todo" for i in items()), "durable snapshot lost"
    run("tmux", "kill-session", "-t", session)
    attach()
    assert_sidebar()
    run("tmux", "send-keys", "-t", session, "C-b")
    until(lambda: "▾ todo" not in pane(), "sidebar did not hide")
    # Hidden, the plan folds to one transcript row: the step in hand, the
    # progress and the key that shows the rest, never the whole list.
    def plan_row():
        screen = pane()
        return screen if "ctrl+b shows the plan" in screen else None
    screen = until(plan_row, "hidden plan left no row in the transcript")
    assert "[•] todo-alpha-proof  ·  0/2 done  ·  ctrl+b shows the plan" in screen, screen
    assert screen.count("todo-alpha-proof") == 1 and "todo-beta-proof" not in screen, screen
    run("tmux", "send-keys", "-t", session, "C-b")
    assert_sidebar()
    # Complete through the current tool alias and verify the historical update
    # remains durable without leaving a stale list in the conversation.
    for entry in entries:
        entry["status"] = "completed"
    tool("todowrite", {"items": entries})
    until(lambda: "▾ todo" not in pane(), "completed TODO list stayed pinned")
    assert "todo-alpha-proof" not in pane() and "# Todos" not in pane(), pane()
    # Ordinary chat still renders after invisible TODO updates.
    run("tmux", "send-keys", "-l", "-t", session, " normal-chat-after-todos")
    run("tmux", "send-keys", "-t", session, "Enter")
    until(lambda: any(i["data"]["type"] == "assistant_message" and
                     "normal-chat-after-todos" in i["data"].get("text", "") for i in items()),
          "normal TUI chat did not finish")
    until(lambda: "normal-chat-after-todos" in pane(), "normal chat missing from screen")
    evidence.append(pane())
    Path("/tmp/haarness-todo-sidebar-tmux.txt").write_text("\n\n".join(evidence))
    Path("/tmp/haarness-todo-sidebar-items.json").write_text(json.dumps(items(), indent=2))
    print("PASS: TODO sidebar only, durable tool records, replay, hide/show, completion, and normal attached TUI chat")
except BaseException:
    try:
        Path("/tmp/haarness-todo-sidebar-failure.txt").write_text(pane())
    except Exception:
        pass
    raise
finally:
    subprocess.run(["tmux", "kill-session", "-t", session], capture_output=True)
    server.close()
