#!/usr/bin/env python3
"""Real TUI message keys, driven through tmux with an isolated echo server.

The keys go in as terminals send them: tmux's own Alt+Up (`ESC [1;3A`, what iTerm2
and other xterm-style terminals send) and macOS Terminal's Option+Up (`ESC ESC [A`,
written to the pane as bytes), with a draft in the prompt and over an approval card
that holds the keyboard. A private tmux server and temporary HAARNESS_HOME/DATA_DIR
keep it away from anything already running; captures and a report go to
TUI_NAVIGATION_EVIDENCE (default /tmp/haarness-tui-navigation).
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

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))
from e2e_tmux import pane_command  # noqa: E402

BINARY = Path(os.environ.get("HAARNESS_BIN", ROOT / "target/debug/haarness")).resolve()
EVIDENCE = Path(os.environ.get("TUI_NAVIGATION_EVIDENCE", "/tmp/haarness-tui-navigation"))
EVIDENCE.mkdir(parents=True, exist_ok=True)
SOCKET = f"haarness-navigation-{os.getpid()}"
TERMINAL_APP_OPTION_UP = ["1b", "1b", "5b", "41"]


def run(*args, **kwargs):
    return subprocess.check_output([str(arg) for arg in args], text=True, **kwargs)


def until(check, message, timeout=40):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if result := check():
            return result
        time.sleep(.1)
    raise AssertionError(message)


def tmux(*args):
    return run("tmux", "-L", SOCKET, "-f", "/dev/null", *args)


with tempfile.TemporaryDirectory(prefix="haarness-navigation-") as directory:
    root = Path(directory)
    project = root / "project"
    (project / ".haarness").mkdir(parents=True)
    (project / ".haarness/config.toml").write_text(
        '[defaults]\nprovider="echo"\nmodel="echo-1"\n[permissions]\nbash="ask"\n'
    )
    (project / "fixture.txt").write_text("navigation fixture\n")
    for args in [["init", "-b", "main"], ["add", "."], ["-c", "user.name=Nav Test", "-c", "user.email=nav@example.test", "commit", "-m", "Fixture"]]:
        run("git", *args, cwd=project, stderr=subprocess.DEVNULL)
    env = dict(os.environ, HAARNESS_HOME=str(root / "home"), HAARNESS_DATA_DIR=str(root / "data"), HAARNESS_TEST_PROVIDER="1")
    for name in [name for name in env if name.startswith("HAARNESS_") and name not in ("HAARNESS_HOME", "HAARNESS_DATA_DIR", "HAARNESS_TEST_PROVIDER")]:
        del env[name]
    session = "tui"
    checks = []
    try:
        connection = json.loads(run(BINARY, "--workspace", project, "--trust-project", "desktop", input='{"action":"connect"}', env=env))

        def api(path, body=None, method=None):
            request = urllib.request.Request(
                connection["url"].rstrip("/") + "/v1" + path,
                data=None if body is None else json.dumps(body).encode(),
                headers={"Authorization": "Bearer " + connection["token"], "Content-Type": "application/json"},
                method=method,
            )
            with urllib.request.urlopen(request, timeout=40) as response:
                raw = response.read()
                return json.loads(raw) if raw else None

        def pane():
            return tmux("capture-pane", "-p", "-t", session)

        def keys(*values):
            tmux("send-keys", "-t", session, *values)

        def raw(*hex_bytes):
            tmux("send-keys", "-t", session, "-H", *hex_bytes)

        def text(value):
            tmux("send-keys", "-l", "-t", session, value)

        def screen(name):
            (EVIDENCE / f"{name}.txt").write_text(pane())
            (EVIDENCE / f"{name}.ansi").write_text(tmux("capture-pane", "-p", "-e", "-t", session))

        def turn(thread, value):
            started = api(f"/threads/{thread}/turns", {"requestId": str(uuid.uuid4()), "text": value, "provider": "echo", "model": "echo-1"})
            return started["id"] if started and "id" in started else None

        def idle():
            until(lambda: not api("/turns/active"), "echo turns did not finish")

        def top():
            """The transcript's top row: the first line under the frame's quiet top row."""
            return next((line.strip() for line in pane().splitlines()[:2] if line.strip()), "")

        def lands(expected, message):
            # A prompt is drawn as typed; its reply as "Echo: …".
            return until(lambda: expected in top() and top() or None, f"{message}: top row is {top()!r}", timeout=10)

        tmux("new-session", "-d", "-x", "100", "-y", "30", "-s", session, pane_command(env, [BINARY, "--workspace", project, "--trust-project"]))
        until(lambda: "/ commands" in pane(), "TUI did not open")
        text("NAV-PROMPT-0 opening")
        keys("Enter")
        until(lambda: api("/threads") and not api("/turns/active"), "opening prompt did not finish")
        thread = api("/threads")[0]["id"]
        body = "".join(f"\nline {line:02} of a long prompt" for line in range(30))
        for index in range(1, 6):
            turn(thread, f"NAV-PROMPT-{index}{body}")
            idle()
        until(lambda: "line 29 of a long prompt" in pane() and "NAV-PROMPT" not in pane().split("\n")[0], "seeded prompts did not reach the TUI")
        screen("latest")

        # tmux's Alt+Up, the xterm form: the reply that starts above the window.
        keys("M-Up")
        lands("Echo: NAV-PROMPT-5", "Alt+Up did not reach the newest reply")
        keys("M-Up")
        lands("NAV-PROMPT-5", "Alt+Up did not reach the newest prompt")
        assert "Echo:" not in top()
        keys("M-Down")
        lands("Echo: NAV-PROMPT-5", "Alt+Down did not come back to the reply")
        screen("alt-up")
        checks.append("Alt+Up/Alt+Down (ESC [1;3A / [1;3B) put the previous/next message on the top row")

        # macOS Terminal's Option+Up, written as its bytes: one key, never Escape.
        text("draft kept")
        raw(*TERMINAL_APP_OPTION_UP)
        lands("NAV-PROMPT-5", "Option+Up from macOS Terminal did not move to the prompt")
        raw(*TERMINAL_APP_OPTION_UP)
        lands("Echo: NAV-PROMPT-4", "a second Option+Up did not move on")
        time.sleep(.3)
        assert "[A" not in pane(), "Option+Up typed [A into the prompt"
        assert "draft kept" in pane(), "Option+Up disturbed the draft"
        assert "interrupt" not in pane().lower(), "Option+Up armed an interrupt"
        screen("terminal-app-option-up")
        checks.append("macOS Terminal's Option+Up (ESC ESC [A) is Alt+Up: no Escape, no [A typed, draft kept")

        # By prompt, the last prompt and the first message.
        keys("S-M-Up")
        lands("NAV-PROMPT-4", "Alt+Shift+Up did not reach the prompt above")
        keys("S-M-Up")
        lands("NAV-PROMPT-3", "Alt+Shift+Up did not skip the reply")
        assert "Echo:" not in top()
        keys("S-M-Down")
        lands("NAV-PROMPT-4", "Alt+Shift+Down did not reach the next prompt")
        keys("M-End")
        lands("NAV-PROMPT-5", "Alt+End did not reach the last prompt")
        keys("M-Home")
        until(lambda: "NAV-PROMPT-0 opening" in pane(), "Alt+Home did not reach the start")
        keys("M-Down")
        lands("NAV-PROMPT-0", "Alt+Down from the start did not reach the first prompt")
        keys("C-End")
        until(lambda: "line 29 of a long prompt" in pane().split("Ask anything")[0] and "scrolled" not in pane(), "Ctrl+End did not return to the latest output")
        checks.append("Alt+Shift+Up/Down step by prompt, Alt+End is the last prompt, Alt+Home the start")

        # An approval card that holds the keyboard: the keys read back, never answer.
        keys("C-u")
        turn(thread, '!tool bash {"command":"echo navigation"}')
        until(lambda: api("/permissions"), "permission did not wait")
        until(lambda: "Allow once" in pane(), "approval card did not show")
        keys("M-Up")
        lands("Echo: NAV-PROMPT-5", "Alt+Up over the approval card did not move the transcript")
        raw(*TERMINAL_APP_OPTION_UP)
        lands("NAV-PROMPT-5", "Option+Up over the approval card did not move the transcript")
        time.sleep(.5)
        assert len(api("/permissions")) == 1, "a message key answered the approval"
        assert "Allow once" in pane(), "the approval card went away"
        screen("over-approval")
        keys("y")
        until(lambda: not api("/permissions") and not api("/turns/active"), "approval did not finish")
        checks.append("Over a focused approval card both forms move the transcript and leave the request waiting")

        (EVIDENCE / "results.json").write_text(json.dumps({"checks": checks, "status": "passed"}, indent=2))
        print("PASS: " + "\nPASS: ".join(checks))
        print("Evidence: " + str(EVIDENCE))
    except BaseException:
        try:
            screen("failure")
        except Exception:
            pass
        raise
    finally:
        subprocess.run(["tmux", "-L", SOCKET, "kill-server"], capture_output=True)
        subprocess.run([str(BINARY), "--workspace", str(project), "serve", "--stop"], env=env, capture_output=True, timeout=30)
