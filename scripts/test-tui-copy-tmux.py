#!/usr/bin/env python3
"""Copying with the mouse in the TUI, in a real tmux pane on the echo provider.

A drag stays in the pane it started in: a reply copied beside the docked sidebar (120 columns) or
without it (80 columns) comes out as written, a dialog's rows without the chat behind them. Tool
output copies without its gutter, while the same bar written in a reply is copied. A press that
slips one cell is a click. The clipboard helper is a stand-in on PATH that writes to a file, so the
system clipboard is never touched. Pane captures, with their colours, go to TUI_COPY_EVIDENCE;
TUI_COPY_THEME=light or dark draws them in that variant of the editorial theme.
"""
import json
import os
import shlex
import subprocess
import sys
import tempfile
import time
import unicodedata
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from e2e_tmux import pane_command  # noqa: E402

ROOT = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get("HAARNESS_BIN", ROOT / "target/debug/haarness")).resolve()
EVIDENCE = Path(os.environ.get("TUI_COPY_EVIDENCE", tempfile.gettempdir() + "/haarness-tui-copy"))
EVIDENCE.mkdir(parents=True, exist_ok=True)
THEME = os.environ.get("TUI_COPY_THEME", "")
SOCKET = f"haarness-copy-{os.getpid()}"
FAILING = "printf 'error: first failure\\n▏ printed bar\\n'; exit 1"


def run(*args, **kwargs):
    return subprocess.check_output([str(arg) for arg in args], text=True, **kwargs)


def tmux(*args):
    return run("tmux", "-L", SOCKET, *args)


def until(check, message, timeout=40):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if result := check():
            return result
        time.sleep(0.1)
    raise AssertionError(message)


def cells(text):
    """Terminal cells `text` takes, as the TUI lays it out."""
    width = 0
    for character in text:
        if unicodedata.combining(character):
            continue
        width += 2 if unicodedata.east_asian_width(character) in ("W", "F") else 1
    return width


with tempfile.TemporaryDirectory(prefix="haarness-copy-", dir=os.environ.get("TUI_COPY_ROOT")) as directory:
    root = Path(directory)
    project = root / "project"
    (project / ".haarness").mkdir(parents=True)
    (project / ".haarness/config.toml").write_text('[defaults]\nprovider="echo"\nmodel="echo-1"\n')
    run("git", "init", "-q", "-b", "main", cwd=project)
    helpers = root / "bin"
    helpers.mkdir()
    sink = root / "clipboard.txt"
    for name in ("pbcopy", "wl-copy", "xclip", "xsel"):
        helper = helpers / name
        helper.write_text('#!/bin/sh\ncat > "$COPY_SINK.tmp" && mv "$COPY_SINK.tmp" "$COPY_SINK"\n')
        helper.chmod(0o755)
    env = dict(
        os.environ,
        HAARNESS_HOME=str(root / "home"),
        HAARNESS_DATA_DIR=str(root / "data"),
        HAARNESS_TEST_PROVIDER="1",
        HAARNESS_NO_CONNECT_PROMPT="1",
        LANG="en_US.UTF-8",
        LC_ALL="en_US.UTF-8",
        COLORTERM="truecolor",
        COPY_SINK=str(sink),
        PATH=f"{helpers}:{os.environ['PATH']}",
    )
    checks = []
    session = "copy"
    try:
        def pane():
            return tmux("capture-pane", "-p", "-t", session)

        def screen(name):
            name = f"{name}-{THEME}" if THEME else name
            (EVIDENCE / f"{name}.txt").write_text(pane())
            (EVIDENCE / f"{name}.ansi").write_text(tmux("capture-pane", "-p", "-e", "-t", session))

        def text(value):
            tmux("send-keys", "-l", "-t", session, value)

        def keys(*values):
            tmux("send-keys", "-t", session, *values)

        def paste(value):
            """Pasted, so a leading `!` is text, not the shell prompt."""
            tmux("set-buffer", "-b", "copy", value)
            tmux("paste-buffer", "-p", "-b", "copy", "-t", session)

        def mouse(button, column, row, release=False):
            """One SGR mouse report; `column` and `row` count from 0."""
            text(f"\033[<{button};{column + 1};{row + 1}{'m' if release else 'M'}")

        def find(needle, start=0, prefix=None):
            """(column, row) where `needle` starts on the first row at or after `start` that has it
            (and, given `prefix`, starts with it)."""
            for row, line in enumerate(pane().splitlines()):
                if row >= start and needle in line and (prefix is None or line.startswith(prefix)):
                    return cells(line[: line.index(needle)]), row
            raise AssertionError(f"{needle!r} not on screen:\n{pane()}")

        def drag(start, end, name=None):
            """Press at `start`, drag through to `end` (inclusive) and release; what was copied."""
            before = sink.read_text() if sink.exists() else None
            sink.unlink(missing_ok=True)
            mouse(0, *start)
            mouse(32, start[0] + 2, start[1])
            mouse(32, *end)
            time.sleep(0.4)
            if name:
                screen(name)
            mouse(0, *end, release=True)
            copied = until(lambda: sink.exists() and sink.read_text(), f"nothing was copied:\n{pane()}", timeout=10)
            assert copied != before or before is None
            return copied

        def last_cell(needle, start=0, prefix=None):
            column, row = find(needle, start, prefix)
            return column + cells(needle) - 1, row

        def launch(width):
            subprocess.run(["tmux", "-L", SOCKET, "kill-server"], capture_output=True)
            tmux("new-session", "-d", "-x", str(width), "-y", "40", "-s", session,
                 pane_command(env, [BINARY, "--workspace", project, "--trust-project", "--yolo"]))
            # The stand-in clipboard is the only one: tmux keeps OSC 52 to itself.
            tmux("set-option", "-g", "set-clipboard", "off")
            until(lambda: "/ commands" in pane(), "the TUI did not open")
            if THEME:
                text(f"/themes editorial {THEME}")
                keys("Enter")
                time.sleep(1)

        for width in (120, 80):
            launch(width)
            text("/new")
            keys("Enter")
            time.sleep(0.5)
            text("::long 8")
            keys("Enter")
            until(lambda: "marker-8" in pane(), "the reply did not arrive")
            time.sleep(0.5)
            start, end = find("line 1 marker-1"), last_cell("line 5 marker-5")
            rows = pane().splitlines()[start[1] : end[1] + 1]
            beside = [row[width - 40 :].strip() for row in rows]
            copied = drag(start, end, f"reply-drag-{width}")
            expected = "\n".join(f"line {index} marker-{index}" for index in range(1, 6))
            assert copied == expected, f"{width}: {copied!r}\n{pane()}"
            if width == 120:
                assert any(beside), f"the sidebar has no text beside the reply:\n{pane()}"
                checks.append("120 columns: five reply rows beside the docked sidebar copy as written, no sidebar text, marker or margin")
            else:
                checks.append("80 columns: the same five rows copy as written")

            paste(f'!tool bash {json.dumps({"command": FAILING})}')
            keys("Enter")
            until(lambda: "Tool bash =>" in pane(), "the tool did not run")
            time.sleep(0.5)
            screen(f"tool-output-{width}")
            gutter = "  ▏ error: first failure"
            start = find("error: first failure", prefix=gutter)
            end = last_cell("▏ printed bar", start[1], prefix="  ▏ ▏")
            copied = drag(start, end, f"tool-output-drag-{width}")
            assert copied == "error: first failure\n▏ printed bar", f"{width}: {copied!r}\n{pane()}"
            # The reply quotes the output: there the bar was written, and is copied.
            reply = find("Tool bash =>")
            end = last_cell("▏ printed bar", reply[1])
            copied = drag(reply, end)
            assert copied.endswith("\n▏ printed bar"), f"{width}: {copied!r}"
            assert copied.startswith("Tool bash =>"), f"{width}: {copied!r}"
            checks.append(f"{width} columns: tool output copies without its gutter; the same bar written in the reply copies")

            # A click that slips one cell opens a folded thought instead of copying.
            text("::reason before the reply")
            keys("Enter")
            until(lambda: "Reasoned reply" in pane() and "▹ Thought" in pane(), "no folded thought")
            time.sleep(0.5)
            sink.unlink(missing_ok=True)
            target = find("▹ Thought")
            mouse(0, *target)
            mouse(32, target[0] + 1, target[1])
            mouse(0, target[0] + 1, target[1], release=True)
            until(lambda: "▿ Thought" in pane() and "Considering before the reply" in pane(),
                  f"{width}: the click did not open the thought:\n{pane()}", timeout=5)
            assert not sink.exists(), f"{width}: a one-cell slip copied {sink.read_text()!r}"
            screen(f"slip-click-{width}")
            checks.append(f"{width} columns: a press that slips one cell opens the thought it is on, nothing copied")

            if width == 120:
                text("/agents")
                keys("Enter")
                until(lambda: "explore" in pane(), "/agents did not open")
                time.sleep(0.5)
                screen("agents-dialog")
                # Three rows of the card, from an agent's name to the middle of the card two rows
                # below; the dimmed chat shows on both sides of the card on those rows.
                start = find("Build ")
                end = (start[0] + 30, start[1] + 2)
                backdrop = "".join(line[: start[0] - 4] for line in pane().splitlines()[start[1] : end[1] + 1])
                assert "Tool bash" in backdrop, f"no chat beside the card:\n{pane()}"
                copied = drag(start, end, "agents-drag")
                assert copied.startswith("Build"), copied
                assert len(copied.splitlines()) == 3, copied
                for word in ("Tool", "error", "printed", "usage"):
                    assert word not in copied, copied
                keys("Escape")
                checks.append("A drag inside the /agents dialog copies its rows only, not the chat beside the card")
        for check in checks:
            print("ok -", check)
        print(f"evidence: {EVIDENCE}")
    except BaseException:
        subprocess.run(["tmux", "-L", SOCKET, "capture-pane", "-p", "-t", session])
        raise
    finally:
        subprocess.run(["tmux", "-L", SOCKET, "kill-server"], capture_output=True)
        subprocess.run([str(BINARY), "--workspace", str(project), "serve", "--stop"], env=env, capture_output=True)
