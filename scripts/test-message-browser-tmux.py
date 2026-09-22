#!/usr/bin/env python3
"""Real local/attached TUI message browser, using an isolated Git repo and echo server."""
import json
import os
import re
import shlex
import subprocess
import tempfile
import time
import urllib.request
import uuid
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get("HAARNESS_BIN", ROOT / "target/debug/haarness")).resolve()
EVIDENCE = Path(os.environ.get("MESSAGE_BROWSER_EVIDENCE", "/tmp/haarness-message-browser"))
EVIDENCE.mkdir(parents=True, exist_ok=True)


def run(*args, **kwargs):
    return subprocess.check_output([str(arg) for arg in args], text=True, **kwargs)


def until(check, message, timeout=40):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if result := check():
            return result
        time.sleep(.1)
    raise AssertionError(message)


with tempfile.TemporaryDirectory(prefix="haarness-messages-") as directory:
    root = Path(directory)
    project = root / "repository 日本"
    (project / ".haarness").mkdir(parents=True)
    (project / ".haarness/config.toml").write_text('[defaults]\nprovider="echo"\nmodel="echo-1"\n')
    (project / "fixture.txt").write_text("Message browser fixture\n")
    for args in [["init", "-b", "main"], ["add", "."], ["-c", "user.name=Browser Test", "-c", "user.email=browser@example.test", "commit", "-m", "Fixture"]]:
        run("git", *args, cwd=project, stderr=subprocess.DEVNULL)
    env = dict(os.environ, HAARNESS_HOME=str(root / "home"), HAARNESS_DATA_DIR=str(root / "data"), HAARNESS_TEST_PROVIDER="1")
    session = f"haarness-messages-{os.getpid()}"
    sessions = [session]
    clipboard = subprocess.check_output(["pbpaste"]) if os.uname().sysname == "Darwin" else None
    checks = []
    try:
        connection = json.loads(run(BINARY, "--workspace", project, "--trust-project", "desktop", input='{"action":"connect"}', env=env))

        def api(path, body=None):
            request = urllib.request.Request(connection["url"].rstrip("/") + "/v1" + path,
                data=None if body is None else json.dumps(body).encode(),
                headers={"Authorization": "Bearer " + connection["token"], "Content-Type": "application/json"})
            with urllib.request.urlopen(request, timeout=40) as response:
                raw = response.read()
                return json.loads(raw) if raw else None

        def pane():
            return run("tmux", "capture-pane", "-p", "-t", session)

        def keys(*values):
            run("tmux", "send-keys", "-t", session, *values)

        def text(value):
            run("tmux", "send-keys", "-l", "-t", session, value)

        def command(value):
            keys("C-c")
            text(value)
            keys("Enter")

        def screen(name):
            (EVIDENCE / f"{name}.txt").write_text(pane())

        def open_browser(query=None):
            if query is None:
                keys("M-m")
            else:
                command("/messages " + query)
            until(lambda: "Messages" in pane() and ("Search" in pane() or "Type to filter" in pane()), "message browser did not open")

        def action(index):
            keys("Enter")
            until(lambda: "Jump to message" in pane(), "message actions did not open")
            keys(*(["Down"] * index), "Enter")

        def draft_start():
            for _ in range(6):
                if "PROTECTED UNSENT DRAFT" in pane():
                    return
                keys("Up")
                time.sleep(.05)
            raise AssertionError("source draft prefix was not preserved")

        def start_turn(thread, value):
            api(f"/threads/{thread}/turns", {"requestId": str(uuid.uuid4()), "text": value, "provider": "echo", "model": "echo-1"})
            until(lambda: not api("/turns/active"), "echo turn did not finish")

        launch_env = [f"{name}={env[name]}" for name in ["HAARNESS_HOME", "HAARNESS_DATA_DIR", "HAARNESS_TEST_PROVIDER"]]
        launch = shlex.join(["env", *launch_env, str(BINARY), "--workspace", str(project), "--trust-project"])
        run("tmux", "new-session", "-d", "-x", "112", "-y", "34", "-s", session, launch)
        until(lambda: "/ commands" in pane(), "local TUI did not open")
        open_browser()
        until(lambda: "Nothing to choose from" in pane(), "empty browser did not show its empty state")
        keys("Escape")
        text("Message browser terminal seed")
        keys("Enter")
        until(lambda: any(i["data"].get("text", "").startswith("Echo: Message browser terminal seed") for t in api("/threads") for i in api(f'/threads/{t["id"]}/items')), "terminal prompt did not persist")
        until(lambda: not api("/turns/active"), "terminal seed did not finish")
        source = api("/threads")[0]["id"]
        for i in range(12):
            start_turn(source, f"PROMPT-{i:02} ordinary opening\nNEEDLE-{i:02} ÉLÈVE 日本語 🔎\n" + "A wrapped sentence with full message text. " * 8 + f"\nTAIL-{i:02}")
        until(lambda: "TAIL-11" in pane(), "seeded messages did not reach TUI")
        items = api(f"/threads/{source}/items")
        user = next(item for item in items if item["data"]["type"] == "user_message" and "NEEDLE-03" in item["data"]["text"])
        reply = next(item for item in items if item["data"]["type"] == "assistant_message" and "NEEDLE-03" in item["data"]["text"])
        text("PROTECTED UNSENT DRAFT")
        open_browser()
        text("needle-03")
        until(lambda: re.search(r"\bof 2\b", pane()), "full-message search did not find prompt and reply")
        screen("search")
        action(2)
        until(lambda: "Message copied" in pane(), "copy action did not finish")
        if clipboard is not None:
            until(lambda: subprocess.check_output(["pbpaste"]).decode() == reply["data"]["text"], "clipboard omitted or altered message text")
        assert "PROTECTED UNSENT DRAFT" in pane()
        checks.append("Full-text search and exact multiline Unicode clipboard; unsent draft preserved")

        open_browser()
        action(1)
        until(lambda: "message" in pane() and "NEEDLE-03" in pane(), "read panel did not show full text")
        keys("End")
        until(lambda: "TAIL-03" in pane(), "read panel could not reach message end")
        screen("read")
        keys("Escape")
        open_browser()
        keys("Down")  # User prompt, after the newest matching assistant reply.
        action(0)
        until(lambda: "Viewing You message" in pane() and "PROMPT-03" in pane(), "jump missed the selected prompt")
        assert "TAIL-11" not in pane()
        start_turn(source, "NEW LIVE OUTPUT AFTER JUMP")
        time.sleep(.4)
        assert "PROMPT-03" in pane() and "NEW LIVE OUTPUT AFTER JUMP" not in pane(), "live output moved the reader"
        screen("jump")
        keys("C-End")
        until(lambda: "NEW LIVE OUTPUT AFTER JUMP" in pane(), "return to latest failed")
        checks.append("Read to the end, exact transcript jump, stable viewport during new output, return to latest")

        open_browser()
        before = len(api(f"/threads/{source}/items"))
        action(3)
        until(lambda: "Message text added to your draft" in pane(), "append-to-draft failed")
        assert len(api(f"/threads/{source}/items")) == before, "draft action sent a turn"
        draft_start()
        screen("draft")
        checks.append("Appending message text preserves the existing draft and never sends it")

        # User boundary: selected prompt and all later content must be absent.
        open_browser()
        action(4)
        forks = until(lambda: [t for t in api("/threads") if t.get("parentId") == source], "user fork did not create a session")
        user_fork = forks[0]["id"]
        fork_items = api(f"/threads/{user_fork}/items")
        assert any("NEEDLE-02" in i["data"].get("text", "") for i in fork_items)
        assert not any("NEEDLE-03" in i["data"].get("text", "") for i in fork_items)
        until(lambda: "Forked from selected message" in pane(), "TUI did not switch to user fork")
        command("/sessions " + source)
        until(lambda: "NEW LIVE OUTPUT AFTER JUMP" in pane(), "source did not resume after fork")
        draft_start()
        command("/messages needle-03")
        until(lambda: re.search(r"\bof 2\b", pane()), "source browser did not reopen")
        action(4)  # Fresh explicit query selects the newest matching assistant.
        forks = until(lambda: [t for t in api("/threads") if t.get("parentId") == source and t["id"] != user_fork], "assistant fork did not create a session")
        assistant_items = api(f'/threads/{forks[0]["id"]}/items')
        assert any(i["data"] == reply["data"] for i in assistant_items)
        assert not any("NEEDLE-04" in i["data"].get("text", "") for i in assistant_items)
        checks.append("User fork excludes the selected prompt; assistant fork includes selected reply and excludes later turns")

        command("/new")
        until(lambda: "/ commands" in pane(), "new session did not open")
        open_browser()
        until(lambda: "Nothing to choose from" in pane(), "message history leaked into an empty session")
        screen("empty-session")
        keys("Escape")
        command("/sessions " + source)
        until(lambda: "NEW LIVE OUTPUT AFTER JUMP" in pane(), "source did not resume")
        open_browser("needle-05")
        start_turn(source, "PROMPT live NEEDLE-05")
        until(lambda: re.search(r"\bof 4\b", pane()), "open browser did not refresh through live events")
        screen("live-browser")
        run("tmux", "resize-window", "-t", session, "-x", "48", "-y", "18")
        until(lambda: "Messages" in pane(), "narrow browser lost its dialog")
        keys("End", "Enter")
        until(lambda: "Jump to message" in pane(), "narrow browser selection failed")
        keys("Enter")
        until(lambda: "PROMPT-05" in pane(), "narrow jump missed the message")
        screen("narrow")
        checks.append("Live result refresh, empty-session isolation, narrow terminal navigation")

        # A separate client workspace attaches to the same server and source.
        session = f"haarness-messages-attach-{os.getpid()}"
        sessions.append(session)
        client_workspace = root / "client"
        client_workspace.mkdir()
        attach = shlex.join(["env", *launch_env, f'HAARNESS_SERVER_TOKEN={connection["token"]}', str(BINARY), "--workspace", str(client_workspace), "--session", source, "attach", connection["url"]])
        run("tmux", "new-session", "-d", "-x", "100", "-y", "30", "-s", session, attach)
        until(lambda: "NEW LIVE OUTPUT" in pane() or "PROMPT live" in pane(), "attached TUI did not load source history")
        open_browser("needle-00")
        until(lambda: re.search(r"\bof 2\b", pane()), "attached browser did not search server history")
        action(0)
        until(lambda: "PROMPT-00" in pane(), "attached jump failed")
        screen("attached")
        checks.append("Independent attached client searches and navigates the server's history")
        (EVIDENCE / "results.json").write_text(json.dumps({"checks": checks, "source_messages": len(items), "status": "passed"}, indent=2))
        print("PASS: " + "\nPASS: ".join(checks))
        print("Evidence: " + str(EVIDENCE))
    except BaseException:
        try:
            screen("failure")
        except Exception:
            pass
        raise
    finally:
        for target in sessions:
            subprocess.run(["tmux", "kill-session", "-t", target], capture_output=True)
        subprocess.run([str(BINARY), "--workspace", str(project), "serve", "--stop"], env=env, capture_output=True, timeout=30)
        if clipboard is not None:
            subprocess.run(["pbcopy"], input=clipboard, check=True)
