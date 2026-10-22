#!/usr/bin/env python3
"""Drive a real TUI's /btw side chat in tmux on the echo provider.

The side chat opens over the chat, streams its answers, sends follow-ups with
their thread, keeps the thread and the draft when Esc goes back to the chat,
stops an answer with Ctrl+C, starts over with /clear, and never adds a row to
the chat itself. HAARNESS_SIDE_CHAT_EVIDENCE=<folder> keeps colour captures
(`capture-pane -e`) of each step there.
"""
import json
import os
from pathlib import Path
import shlex
import subprocess
import tempfile
import time
import urllib.request

import e2e_scratch


def run(*args, **kwargs):
    return subprocess.check_output(args, text=True, **kwargs)


def until(check, description, seconds=20):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(0.1)
    raise AssertionError(description + "\n" + pane())


binary = str(Path(os.environ.get("HAARNESS_BIN", "target/debug/haarness")).resolve())
session = f"haarness-side-chat-{os.getpid()}"
root = Path(tempfile.mkdtemp(prefix="haarness-side-chat-")).resolve()
project = root / "project"
config = project / ".haarness"
config.mkdir(parents=True)
(config / "config.toml").write_text('[defaults]\nprovider="echo"\nmodel="echo-1"\n')
home = root / "home"
home.mkdir()
evidence = os.environ.get("HAARNESS_SIDE_CHAT_EVIDENCE")
theme = os.environ.get("HAARNESS_SIDE_CHAT_THEME")
isolated = {
    "HOME": str(home),
    "HAARNESS_HOME": str(home / ".haarness"),
    "HAARNESS_DATA_DIR": str(root / "data"),
    "HAARNESS_TEST_PROVIDER": "1",
    "XDG_CONFIG_HOME": str(home / ".config"),
    "XDG_STATE_HOME": str(home / ".local/state"),
    "COLORTERM": "truecolor",
}
env = dict(os.environ, **isolated)
command = shlex.join([
    "env", *[f"{key}={value}" for key, value in isolated.items()],
    "HAARNESS_DEFAULT_PROVIDER=echo", "HAARNESS_DEFAULT_MODEL=echo-1",
    binary, "--workspace", str(project), "--trust-project",
])


def pane():
    return subprocess.run(
        ["tmux", "capture-pane", "-p", "-t", session], capture_output=True, text=True
    ).stdout


def keep(name):
    if evidence:
        Path(evidence).mkdir(parents=True, exist_ok=True)
        colour = run("tmux", "capture-pane", "-p", "-e", "-t", session)
        (Path(evidence) / f"{name}.ansi").write_text(colour)


def type_line(text):
    run("tmux", "send-keys", "-l", "-t", session, text)
    run("tmux", "send-keys", "-t", session, "Enter")


try:
    connection = json.loads(run(
        binary, "--workspace", str(project), "--trust-project", "desktop",
        input='{"action":"connect"}', env=env,
    ))

    def api(path):
        request = urllib.request.Request(
            connection["url"].rstrip("/") + "/v1" + path,
            headers={"Authorization": "Bearer " + connection["token"]},
        )
        with urllib.request.urlopen(request, timeout=5) as response:
            return json.load(response)

    run("tmux", "new-session", "-d", "-x", "120", "-y", "36", "-s", session, command)
    run("tmux", "set-option", "-t", session, "-g", "default-terminal", "tmux-256color")
    until(lambda: "h a a r n e s s" in pane(), "TUI did not open")
    if theme:
        type_line(f"/themes {theme}")
        time.sleep(0.5)
    type_line("ship the release notes")
    until(lambda: "Echo: ship the release notes" in pane(), "the chat did not answer")
    threads = api("/threads")
    chat = next(t for t in threads if any(
        i["data"].get("text") == "ship the release notes" for i in api(f"/threads/{t['id']}/items")
    ))
    rows = len(api(f"/threads/{chat['id']}/items"))

    # /btw asks in the side chat, which takes the screen and streams.
    type_line("/btw what changed?")
    until(lambda: "Echo: what changed?" in pane(), "the side answer did not arrive")
    shown = pane()
    assert "/btw" in shown and "reads this chat, never changes it" in shown, shown
    assert "Ask a follow-up" in shown, shown
    keep("1-answer")

    # A follow-up carries the thread: the echo provider names the question before.
    type_line("and why?")
    until(lambda: "Echo: and why? (after: what changed?)" in pane(), "the follow-up lost its thread")
    keep("2-follow-up")

    # Answers stream: the first piece shows while "Answering" runs, then the rest.
    type_line("::mdslow")
    until(lambda: "Live markdown" in pane() and "Answering" in pane(), "the answer did not stream")
    keep("3-streaming")
    until(lambda: "streams while working" in pane() and "Answering" not in pane(), "the stream did not finish")

    # Esc goes back to the chat; the side chat and its draft wait for /btw.
    run("tmux", "send-keys", "-l", "-t", session, "half a thought")
    run("tmux", "send-keys", "-t", session, "Escape")
    until(lambda: "Echo: ship the release notes" in pane() and "reads this chat" not in pane(),
          "Esc did not go back to the chat")
    type_line("/btw")
    until(lambda: "half a thought" in pane() and "Echo: what changed?" in pane(),
          "/btw did not bring the thread and its draft back")

    # Ctrl+C clears the box, then stops an answer being written.
    run("tmux", "send-keys", "-t", session, "C-c")
    until(lambda: "half a thought" not in pane(), "Ctrl+C did not clear the box")
    type_line("!linger 8000 take your time")
    until(lambda: "Thinking…" in pane(), "the slow answer did not start")
    keep("4-thinking")
    run("tmux", "send-keys", "-t", session, "C-c")
    until(lambda: "Stopped" in pane(), "Ctrl+C did not stop the answer")
    keep("5-stopped")

    # /clear starts over.
    type_line("/clear")
    until(lambda: "Ask a side question" in pane() and "Echo: what changed?" not in pane(),
          "/clear did not start a new thread")
    keep("6-empty")
    run("tmux", "send-keys", "-t", session, "Escape")
    until(lambda: "reads this chat" not in pane(), "Esc did not leave the side chat")

    assert len(api(f"/threads/{chat['id']}/items")) == rows, "the side chat changed the chat"
    print("PASS: /btw side chat streams, follows up, keeps its thread and draft, stops and clears"
          " in a real TUI, and leaves the chat unchanged")
finally:
    subprocess.run(["tmux", "kill-session", "-t", session], capture_output=True)
    subprocess.run([binary, "--workspace", str(project), "serve", "--stop"],
                   env=env, capture_output=True, timeout=30)
    e2e_scratch.settle(root)
