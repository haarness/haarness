#!/usr/bin/env python3
"""Real tmux proof: hide native cursor on every screen, restore it on exit."""

import json
import os
from pathlib import Path
import shlex
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get("HAARNESS_BIN", ROOT / "target/debug/haarness")).resolve()


def tmux(*args):
    return subprocess.check_output(["tmux", *args], text=True).strip()


def wait(predicate, label):
    deadline = time.monotonic() + 12
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.1)
    raise AssertionError("Timed out: " + label)


with tempfile.TemporaryDirectory(prefix="haarness-cursor-") as tmp:
    root = Path(tmp)
    workspace = root / "workspace"
    workspace.mkdir()
    config = workspace / ".haarness"
    config.mkdir()
    (config / "config.toml").write_text('[defaults]\nprovider="echo"\nmodel="echo-1"\n')
    data = root / "data"
    home = root / "home"
    home.mkdir()
    session = "haarness-cursor-" + str(os.getpid())
    target = session + ":0.0"
    env = {
        **os.environ,
        "HAARNESS_DATA_DIR": str(data),
        "HAARNESS_TEST_PROVIDER": "1",
        "HOME": str(home),
    }
    command = shlex.join(
        [
            "env",
            "HAARNESS_DATA_DIR=" + str(data),
            "HAARNESS_TEST_PROVIDER=1",
            "HOME=" + str(home),
            str(BINARY),
            "--workspace",
            str(workspace),
            "--trust-project",
        ]
    )
    try:
        tmux("new-session", "-d", "-s", session, "-x", "100", "-y", "32")
        tmux("send-keys", "-t", target, "-l", command)
        tmux("send-keys", "-t", target, "Enter")
        wait(
            lambda: (
                tmux("display-message", "-p", "-t", target, "#{alternate_on}") == "1"
            ),
            "TUI entry",
        )

        def hidden(label):
            time.sleep(0.25)
            assert (
                tmux("display-message", "-p", "-t", target, "#{cursor_flag}") == "0"
            ), "Native cursor visible: " + label

        def send(text):
            tmux("send-keys", "-t", target, "-l", text)
            tmux("send-keys", "-t", target, "Enter")

        hidden("home")
        send("hello cursor fixture")
        wait(lambda: "Echo:" in tmux("capture-pane", "-p", "-t", target), "chat reply")
        hidden("chat")
        for command in ("/models", "/sessions", "/help", "/diff"):
            send(command)
            hidden(command)
            tmux("send-keys", "-t", target, "Escape")
            tmux("send-keys", "-t", target, "C-u")
        send("/terminal")
        hidden("nested terminal")
        send("printf 'CURSOR_PTY_OK\\n'")
        wait(
            lambda: "CURSOR_PTY_OK" in tmux("capture-pane", "-p", "-t", target),
            "PTY input",
        )
        hidden("nested terminal after output")
        tmux("send-keys", "-t", target, "-l", "\x1b[93;5u")
        wait(
            lambda: "ctrl+] chat" not in tmux("capture-pane", "-p", "-t", target),
            "PTY detach",
        )
        send("::mdslow")
        hidden("streaming")
        time.sleep(1.6)
        hidden("completed stream")
        tmux("resize-window", "-t", session, "-x", "90", "-y", "28")
        hidden("resize")
        tmux("send-keys", "-t", target, "Escape")
        tmux("send-keys", "-t", target, "C-u")
        tmux("send-keys", "-t", target, "C-z")
        wait(
            lambda: (
                tmux("display-message", "-p", "-t", target, "#{alternate_on}") == "0"
            ),
            "suspend",
        )
        assert tmux("display-message", "-p", "-t", target, "#{cursor_flag}") == "1", (
            "shell cursor not restored on suspend"
        )
        send("fg")
        wait(
            lambda: (
                tmux("display-message", "-p", "-t", target, "#{alternate_on}") == "1"
            ),
            "resume",
        )
        hidden("resume")
        send("/exit")
        wait(
            lambda: (
                tmux("display-message", "-p", "-t", target, "#{alternate_on}") == "0"
            ),
            "exit",
        )
        assert tmux("display-message", "-p", "-t", target, "#{cursor_flag}") == "1", (
            "shell cursor not restored on exit"
        )
        print(
            "TUI cursor hidden: home, chat, selectors, help, diff, PTY, streaming, resize, resume; shell cursor restored on suspend/exit"
        )
    finally:
        subprocess.run(
            ["tmux", "kill-session", "-t", session],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        subprocess.run(
            [str(BINARY), "--workspace", str(workspace), "serve", "--stop"],
            env=env,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=15,
        )
