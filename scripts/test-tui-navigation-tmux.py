#!/usr/bin/env python3
"""Real TUI message keys and the waiting-first session list, through tmux with an echo server.

The keys go in as terminals send them: tmux's own Alt+Up (`ESC [1;3A`, what iTerm2
and other xterm-style terminals send) and macOS Terminal's Option+Up (`ESC ESC [A`,
written to the pane as bytes), with a draft in the prompt and over an approval card
that holds the keyboard. Then other chats ask for an approval and an answer: the
session list leads with them, marked, keeps its cursor on its chat while a request
arrives and lets them go when answered elsewhere. A private tmux server and temporary
HAARNESS_HOME/DATA_DIR keep it away from anything already running; captures (text and
`capture-pane -e`, the list in the dark and light palettes) and a report go to
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

        tui = pane_command(env, [BINARY, "--workspace", project, "--trust-project"])
        tmux("new-session", "-d", "-x", "100", "-y", "30", "-s", session, tui)
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
        # A burst, as a held key or bunched packets bring it: one read, three keys.
        raw(*TERMINAL_APP_OPTION_UP * 3)
        lands("NAV-PROMPT-3", "three Option+Ups in one read did not move three messages")
        time.sleep(.3)
        assert "[A" not in pane(), "Option+Up typed [A into the prompt"
        assert "draft kept" in pane(), "Option+Up disturbed the draft"
        assert "interrupt" not in pane().lower(), "Option+Up armed an interrupt"
        screen("terminal-app-option-up")
        for _ in range(3):
            keys("M-Down")
            time.sleep(.15)
        lands("Echo: NAV-PROMPT-4", "Alt+Down did not come back after the burst")
        checks.append("macOS Terminal's Option+Up (ESC ESC [A) is Alt+Up, three in one read too: no Escape, no [A typed, draft kept")

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
        raw(*TERMINAL_APP_OPTION_UP * 3)
        lands("Echo: NAV-PROMPT-3", "a burst over the approval card did not move three messages")
        time.sleep(.5)
        assert len(api("/permissions")) == 1, "a message key answered the approval"
        assert "Allow once" in pane(), "the approval card went away"
        assert "[A" not in pane(), "a burst typed [A"
        screen("over-approval")
        # A lone Escape still rejects the focused card.
        keys("Escape")
        until(lambda: not api("/permissions") and not api("/turns/active"), "Escape did not reject the approval")
        replies = [item["data"].get("text", "") for item in api(f"/threads/{thread}/items") if item["data"]["type"] == "assistant_message"]
        assert "rejected" in replies[-1].lower(), f"Escape did not reject the request: {replies[-1]!r}"
        checks.append("Over a focused approval card both forms, and a burst, move the transcript and leave the request waiting; a lone Escape still rejects it")

        # A running turn: a burst never interrupts it; a double Escape still does.
        keys("C-End")
        turn(thread, '!tool bash {"command":"sleep 30"}')
        until(lambda: "Allow once" in pane(), "the sleep approval did not show")
        keys("y")
        until(lambda: not api("/permissions") and api("/turns/active"), "the sleep did not start")
        time.sleep(.5)
        raw(*TERMINAL_APP_OPTION_UP * 3)
        time.sleep(1)
        assert api("/turns/active"), "a burst of Option+Up stopped the turn"
        assert "[A" not in pane() and "esc again" not in pane().lower(), "a burst of Option+Up armed an interrupt"
        keys("C-End")
        until(lambda: "scrolled" not in pane(), "Ctrl+End did not return to the latest output")
        keys("Escape")
        until(lambda: "esc again" in pane().lower(), "Escape did not arm the interrupt")
        time.sleep(.3)
        keys("Escape")
        until(lambda: not api("/turns/active"), "a double Escape did not stop the turn", timeout=15)
        checks.append("During a turn a burst of Option+Up leaves it running; a double Escape still stops it")

        # Other chats wait: one for an approval, one for an answer. The list
        # leads with them, newest first, and marks them as Desktop's sidebar does.
        def chat(title):
            return api("/threads", {"title": title, "provider": "echo", "model": "echo-1"})["id"]

        late = chat("Late approval")
        quiet = chat("Quiet chat")
        approval = chat("Waits for approval")
        question = chat("Asks a question")
        turn(quiet, "nothing to wait for")
        idle()
        turn(approval, '!tool bash {"command":"echo waiting"}')
        until(lambda: len(api("/permissions")) == 1, "the approval did not wait")
        turn(question, "!tool question " + json.dumps({"prompt": "Which colour?", "choices": ["red", "blue"]}))
        until(lambda: len(api("/questions")) == 1, "the question did not wait")

        def rows():
            """The list's rows as (gutter sign, row): the sign sits in the filter line's column.
            Each row's detail names the agent and model (long ones are cut)."""
            lines = pane().splitlines()
            column = next((line.index(hint) for line in lines for hint in ("Type to filter", "Search  ") if hint in line), None)
            if column is None:
                return []
            listed = [line[column:] for line in lines if "build · Echo" in line and len(line) > column]
            return [(line[0] if line[0] in "⚠?●" else "", line) for line in listed]

        def titles():
            names = ("Asks a question", "Waits for approval", "Late approval", "Quiet chat")
            return [next((name for name in names if name in line), "this chat" if " current " in line else line) for _, line in rows()]

        keys("C-l")
        until(lambda: "Sessions" in pane() and len(rows()) >= 5, "the session list did not open")
        assert titles()[:4] == ["Asks a question", "Waits for approval", "Quiet chat", "Late approval"], titles()
        signs = [sign for sign, _ in rows()]
        assert signs[:2] == ["?", "⚠"], signs
        assert "waiting for your answer" in rows()[0][1] and "waiting for approval" in rows()[1][1], rows()
        assert "waiting" not in rows()[2][1]
        screen("session-list-waiting")
        checks.append("The session list leads with the chats that wait, ? for an answer and ⚠ for an approval, in words too")

        # A request arrives while the cursor is on a quiet chat: it stays there.
        current = titles().index("this chat")
        keys(*["Up"] * (current - titles().index("Quiet chat")))
        turn(late, '!tool bash {"command":"echo late"}')
        until(lambda: len(api("/permissions")) == 2, "the late approval did not wait")
        until(lambda: titles()[:3] == ["Asks a question", "Waits for approval", "Late approval"], "the late approval did not join the waiting chats")
        keys("Right")
        until(lambda: "Resume" in pane(), "session actions did not open")
        heading = next(line for line in pane().splitlines() if "esc" in line)
        assert "Quiet chat" in heading, f"the cursor left its chat: {heading!r}"
        screen("session-list-cursor-kept")
        keys("Escape")
        checks.append("A chat that starts waiting moves up while the cursor stays on its own chat")

        # The list in both palettes and in Arabic, with three chats waiting.
        config = root / "data" / "config" / "config.toml"
        base = config.read_text() if config.exists() else ""
        for name, variant, language in (("dark", "dark", "en"), ("light", "light", "en"), ("arabic", "dark", "ar")):
            config.parent.mkdir(parents=True, exist_ok=True)
            config.write_text(base + f'\n[tui]\ntheme = "haarness"\nvariant = "{variant}"\nbackground_mode = "theme"\nlanguage = "{language}"\n')
            tmux("respawn-pane", "-k", "-t", session, tui)
            until(lambda: "haarness" in pane().replace(" ", ""), f"TUI did not reopen ({name})")
            time.sleep(1)
            keys("C-l")
            until(lambda: len([line for line in pane().splitlines() if "Echo 1" in line]) >= 5, f"the session list did not open ({name})")
            screen(f"session-list-{name}")
            keys("Escape")
        config.write_text(base)
        tmux("respawn-pane", "-k", "-t", session, tui)
        until(lambda: "haarness" in pane().replace(" ", ""), "TUI did not reopen in English")
        time.sleep(1)

        # Answered elsewhere (here: through the API), a chat goes back to its place.
        keys("C-l")
        until(lambda: "Sessions" in pane() and len(rows()) >= 5, "the session list did not reopen")
        for pending in api("/permissions"):
            api(f"/permissions/{pending['permissionId']}", {"decision": "once"})
        until(lambda: titles()[0] == "Asks a question" and [sign for sign, _ in rows()].count("⚠") == 0, "answered approvals stayed marked")
        assert [sign for sign, _ in rows()].count("?") == 1
        screen("session-list-answered")
        api(f"/questions/{api('/questions')[0]['questionId']}", {"answer": "red"})
        until(lambda: not any(sign in ("⚠", "?") for sign, _ in rows()), "the answered question stayed marked")
        idle()
        keys("Escape")
        checks.append("Answered elsewhere, a chat loses its mark and its place at the top")

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
