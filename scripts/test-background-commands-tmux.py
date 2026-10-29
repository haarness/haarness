#!/usr/bin/env python3
"""The terminal UI with an agent's shell commands in the background, in a real tmux pane.

A private tmux server runs the TUI against an isolated workspace daemon with the echo provider
(no paid model) and fast limits (`shell_move_after_ms` 1.5 s, `shell_stop_grace_ms` 1.5 s):

- a command started in the background reads `in background · Ns` on its row, the sidebar lists
  it under `background`, and the row ends `in background · exit 3`; a click opens its output;
- a long command the TUI's own turn ran moves there by itself (`movedBy` limit) and completes;
- a command a script started (no followsBackground) names Ctrl+X on its row after a few seconds;
  Ctrl+X sends it to the background and the draft in the prompt stays;
- with nothing else running the band counts it; 40 columns keep the state; `/language ar` reads it;
- `/sessions` reads `running in background` for another chat whose only work is a command;
- `/tasks` lists it and opens its output; `s` stops it (`stopping`, then `stopped`) and the output
  stays;
- Esc Esc with nothing running stops a command in the background; Esc Esc while a reply runs
  stops the reply and leaves it running.

Captures (text and `capture-pane -e`, also in the light palette and Arabic) go to
BACKGROUND_TUI_EVIDENCE (default /tmp/haarness-background-tui).
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

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))
import e2e_scratch  # noqa: E402
from e2e_env import clean_env  # noqa: E402
from e2e_tmux import pane_command  # noqa: E402

BINARY = Path(os.environ.get("HAARNESS_BIN", ROOT / "target/debug/haarness")).resolve()
EVIDENCE = Path(os.environ.get("BACKGROUND_TUI_EVIDENCE", "/tmp/haarness-background-tui"))
EVIDENCE.mkdir(parents=True, exist_ok=True)
SOCKET = f"haarness-background-{os.getpid()}"
SESSION = "tui"
WIDTH, HEIGHT = 120, 60
# The conversation's columns beside the docked sidebar.
MAIN = WIDTH - 42

root = Path(tempfile.mkdtemp(prefix="haarness-background-tui-")).resolve()
project = root / "project"
(project / ".haarness").mkdir(parents=True)
CONFIG = (
    '[defaults]\nprovider="echo"\nmodel="echo-1"\n[permissions]\nbash="allow"\n[lsp]\nenabled=false\n'
    "[execution]\nshell_move_after_ms=1500\nshell_stop_grace_ms=1500\n"
)
(project / ".haarness/config.toml").write_text(CONFIG)
env = dict(
    clean_env("RUST_LOG"),
    HOME=str(root / "home"),
    HAARNESS_HOME=str(root / "home"),
    HAARNESS_DATA_DIR=str(root / "data"),
    HAARNESS_TEST_PROVIDER="1",
    HAARNESS_NO_CONNECT_PROMPT="1",
    LANG="en_US.UTF-8",
)
(root / "home").mkdir()
connection = {}
passed = []


def run(*args, **kwargs):
    return subprocess.check_output([str(arg) for arg in args], text=True, **kwargs)


def until(check, message, timeout=30):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if result := check():
            return result
        time.sleep(0.1)
    raise AssertionError(message)


def tmux(*args):
    return run("tmux", "-L", SOCKET, "-f", "/dev/null", *args, env=env)


def pane():
    return tmux("capture-pane", "-p", "-t", SESSION)


def keys(*values):
    tmux("send-keys", "-t", SESSION, *values)


def text(value):
    tmux("send-keys", "-l", "-t", SESSION, value)


def prompt(value):
    text(value)
    keys("Enter")


def click(column, row):
    """A left click at a 0-based cell, as an SGR mouse report from the terminal."""
    for final in ("M", "m"):
        sequence = f"\x1b[<0;{column + 1};{row + 1}{final}"
        tmux("send-keys", "-t", SESSION, "-H", *[f"{byte:02x}" for byte in sequence.encode()])


def screen(name):
    (EVIDENCE / f"{name}.txt").write_text(pane())
    (EVIDENCE / f"{name}.ansi").write_text(tmux("capture-pane", "-p", "-e", "-t", SESSION))


def api(path, body=None, method=None):
    request = urllib.request.Request(
        connection["url"].rstrip("/") + "/v1" + path,
        data=None if body is None else json.dumps(body).encode(),
        headers={"Authorization": "Bearer " + connection["token"], "Content-Type": "application/json"},
        method=method or ("GET" if body is None else "POST"),
    )
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            raw = response.read()
            return json.loads(raw) if raw else None
    except urllib.error.HTTPError as error:
        return {"http": error.code, "error": error.read().decode()}


def tool(name, arguments):
    # The leading space keeps `!` from opening the prompt's shell mode; the echo
    # provider reads the directive after it.
    return f" !tool {name} " + json.dumps(arguments)


def idle():
    until(lambda: not api("/turns/active"), "turns did not finish", timeout=40)


def commands(thread):
    return api(f"/threads/{thread}/commands")


def command_with(thread, marker):
    return until(lambda: next((entry for entry in commands(thread) if marker in entry["command"]), None),
                 f"no command running {marker!r}")


def row(marker):
    """The `bash` row of the command `: marker; …`, with its index; the sidebar beside it is left
    out, and so is the prompt that asked for it."""
    for index, line in enumerate(pane().splitlines()):
        if f"bash : {marker}" in line[:MAIN]:
            return index, line[:MAIN]
    return None


def row_reads(marker, state, timeout=30):
    return until(lambda: (found := row(marker)) and state in found[1] and found,
                 f"the row of {marker!r} never read {state!r}:\n{pane()}", timeout)


def output_line(text):
    """The output view shows `text` as a line of the command's output, not just in its header (a
    toast may sit over the right of the line)."""
    return any(line[:MAIN].rstrip() == f"  {text}" for line in pane().splitlines()[2:])


def ok(message):
    passed.append(message)
    print("PASS " + message, flush=True)


try:
    connection.update(json.loads(run(BINARY, "--workspace", project, "--trust-project", "desktop",
                                      input='{"action":"connect"}', env=env, timeout=90)))
    assert "background-commands.v1" in api("/info")["capabilities"]
    tui = pane_command(env, [BINARY, "--workspace", project, "--trust-project"])
    tmux("new-session", "-d", "-x", str(WIDTH), "-y", str(HEIGHT), "-s", SESSION, tui)
    until(lambda: "/ commands" in pane(), "the TUI did not open")
    time.sleep(1)
    assert "▾ background" not in pane(), "an empty background section showed"

    # A command started in the background, its row, the sidebar, its end and its output.
    prompt(tool("bash", {"command": ": started-bg; printf 'ready\\n'; sleep 5; printf 'done\\n'; exit 3",
                         "run_in_background": True}))
    thread = until(lambda: (api("/threads") or [None])[0], "the chat was not created")["id"]
    running = row_reads("started-bg", "in background · ")
    assert "▾ background" in pane(), pane()
    screen("01-started-running")
    ended = row_reads("started-bg", "in background · exit 3", timeout=40)
    assert "✗" in ended[1], ended
    idle()
    screen("02-started-ended")
    index, line = row("started-bg")
    click(line.index("bash"), index)
    until(lambda: pane().startswith(" bash ") and output_line("done"), "a click did not open the output")
    view = pane()
    assert output_line("ready") and "in background · exit 3" in view.splitlines()[0], view
    assert "s stop" not in view, "an ended command offered Stop"
    screen("03-output-ended")
    keys("Escape")
    until(lambda: "/ commands" in pane() or "Ask anything" in pane(), "Esc did not leave the output")
    ok("a command started in the background reads running, then exit 3, and a click opens its output")

    # A long command of the TUI's own turn moves there by itself.
    prompt(tool("bash", {"command": ": auto-move; printf 'started\\n'; sleep 4; printf 'finished\\n'"}))
    row_reads("auto-move", "in background · ")
    moved = command_with(thread, "auto-move")
    assert moved["background"] and moved["movedBy"] == "limit", moved
    row_reads("auto-move", "in background · completed", timeout=40)
    idle()
    ok("a long command of the TUI's turn moves to the background by itself and completes")

    # A command a script started: the row names Ctrl+X; it moves and the draft stays.
    serving = ": serving; trap '' INT TERM; printf 'serving\\n'; sleep 120"
    held = api(f"/threads/{thread}/turns", {"requestId": str(uuid.uuid4()), "text": tool("bash", {"command": serving}).strip()})
    assert "id" in held, held
    row_reads("serving", "ctrl+x sends to background", timeout=20)
    screen("04-move-hint")
    text("keep this draft")
    keys("C-x")
    row_reads("serving", "in background · ")
    info = command_with(thread, "serving")
    assert info["background"] and info["movedBy"] == "person", info
    idle()
    assert "keep this draft" in pane(), pane()
    screen("05-moved-draft-kept")
    keys("C-u")
    until(lambda: "keep this draft" not in pane(), "the draft did not clear")
    ok("a foreground command names Ctrl+X, which sends it to the background and keeps the draft")

    # Nothing else running: the band counts it.
    until(lambda: "1 command in background" in pane(), "the band did not count the command", timeout=20)
    screen("06-idle-band")
    ok("with nothing else running the band counts the command in the background")

    # 40 columns keep the row's state.
    tmux("resize-window", "-t", SESSION, "-x", "40", "-y", "20")
    narrow = until(lambda: (found := row("serving")) and found[1].rstrip().endswith("in background") and found,
                   f"the narrow row lost its state:\n{pane()}")
    assert len(narrow[1].rstrip()) <= 40
    screen("07-forty-columns")
    tmux("resize-window", "-t", SESSION, "-x", str(WIDTH), "-y", str(HEIGHT))
    until(lambda: "▾ background" in pane(), "the sidebar did not come back")
    ok("at 40 columns a row still says it runs in the background")

    # Arabic.
    prompt("/language ar")
    until(lambda: "في الخلفية" in pane() and "▾ الخلفية" in pane(), f"Arabic did not show:\n{pane()}")
    screen("08-arabic")
    prompt("/language en")
    until(lambda: "▾ background" in pane(), "English did not come back")
    ok("/language ar reads the row and the sidebar in Arabic")

    # Another chat whose only work is a command in the background.
    other = api("/threads", {"title": "Dev server chat", "provider": "echo", "model": "echo-1"})["id"]
    started = api(f"/threads/{other}/turns", {
        "requestId": str(uuid.uuid4()), "followsBackground": True,
        "text": tool("bash", {"command": ": other-chat; sleep 120", "run_in_background": True}).strip()})
    assert "id" in started, started
    command_with(other, "other-chat")
    idle()
    keys("C-l")
    listed = until(lambda: next((line for line in pane().splitlines()
                                 if "Dev server chat" in line or "running in background" in line), None)
                   and "running in background" in pane() and pane(),
                   f"the session list did not say running in background:\n{pane()}")
    assert "Dev server chat" in listed, listed
    screen("09-sessions")
    keys("Escape")
    ok("/sessions reads running in background for a chat whose only work is a command")

    # /tasks opens the output; s stops the command, which reads stopping, then stopped.
    keys("C-o")
    until(lambda: "enter opens its output" in pane(), f"/tasks did not list commands:\n{pane()}")
    screen("10-tasks")
    keys("Enter")
    until(lambda: pane().startswith(" bash ") and output_line("serving"), "Enter did not open the output")
    assert "s stop" in pane().splitlines()[-1], pane()
    screen("11-output-running")
    keys("s")
    until(lambda: "in background · stopping" in pane().splitlines()[0], f"Stop did not read stopping:\n{pane()}", timeout=10)
    screen("12-output-stopping")
    until(lambda: "in background · stopped" in pane().splitlines()[0], "the command never read stopped", timeout=20)
    assert output_line("serving") and "s stop" not in pane().splitlines()[-1], pane()
    assert command_with(thread, "serving")["endedBy"] == "person"
    screen("13-output-stopped")
    keys("q")
    row_reads("serving", "in background · stopped")
    idle()
    ok("/tasks opens the output and s stops the command, which reads stopping then stopped, output kept")

    # Esc Esc with nothing running stops a command in the background.
    prompt(tool("bash", {"command": ": esc-stops; sleep 120", "run_in_background": True}))
    row_reads("esc-stops", "in background · ")
    idle()
    time.sleep(0.5)
    keys("Escape")
    until(lambda: "Press Esc again to stop 1 command in background" in pane(), "the first Esc did not say what it stops")
    keys("Escape")
    row_reads("esc-stops", "in background · stopped", timeout=20)
    assert command_with(thread, "esc-stops")["state"] == "stopped"
    ok("Esc Esc with nothing running stops the chat's command in the background")

    # Esc Esc while a reply runs stops the reply and leaves the command running.
    prompt(tool("bash", {"command": ": survives; sleep 120", "run_in_background": True}))
    row_reads("survives", "in background · ")
    idle()
    prompt(" !linger 8000 still replying")
    until(lambda: api("/turns/active"), "the lingering reply did not start")
    keys("Escape")
    keys("Escape")
    until(lambda: not api("/turns/active"), "Esc Esc did not stop the reply", timeout=20)
    time.sleep(1)
    survivor = command_with(thread, "survives")
    assert survivor["state"] == "running", survivor
    assert "in background · " in row("survives")[1] and "stopped" not in row("survives")[1]
    screen("14-stop-reply-keeps-background")
    ok("Esc Esc while a reply runs stops the reply and leaves the command in the background running")

    # A shell step of a batch runs as call `<batch call>:<step>`: its line under the batch row
    # reads as a command row does.
    prompt(tool("batch", {"tool_calls": [
        {"tool": "read", "parameters": {"path": ".haarness/config.toml"}},
        {"tool": "bash", "parameters": {"command": ": batch-step; sleep 120", "run_in_background": True}},
    ]}))
    step = row_reads("batch-step", "in background · ")
    assert command_with(thread, "batch-step")["callId"].endswith(":2"), commands(thread)
    idle()
    screen("15-batch-step")
    ok("a shell step of a batch reads in background under the batch row")

    # Progress without newlines, far past what the view keeps: the view keeps showing the newest.
    prompt(tool("bash", {"command": ": progress; i=0; while :; do i=$((i+1)); printf 'progress %08d\\r' $i; done",
                         "run_in_background": True}))
    row_reads("progress", "in background · ")
    idle()
    time.sleep(2)
    # Where the row is now: the reply below it may have moved it.
    index, line = row_reads("progress", "in background · ")
    click(line.index("bash"), index)
    until(lambda: pane().startswith(" bash "), "the progress output did not open")

    def newest():
        return next((line[:MAIN].strip() for line in pane().splitlines()[2:]
                     if line.strip().startswith("progress ")), None)
    seen = set()
    for _ in range(8):
        time.sleep(1)
        seen.add(newest())
    progress = command_with(thread, "progress")
    assert progress["outputBytes"] > 2 * 1024 * 1024, progress
    assert len(seen - {None}) >= 6, f"the view stopped following: {seen}"
    screen("16-progress-follows")
    keys("s")
    until(lambda: "in background · stopped" in pane().splitlines()[0], "the progress command did not stop", timeout=20)
    keys("q")
    ok("progress without newlines keeps the output view following past 2 MiB")

    # The views in the light palette and in Arabic, reopened on the same chat.
    config = root / "data" / "config" / "config.toml"
    resume = pane_command(env, [BINARY, "--workspace", project, "--trust-project", "-s", thread])
    for name, variant, language in (("dark", "dark", "en"), ("light", "light", "en"), ("arabic", "dark", "ar")):
        config.parent.mkdir(parents=True, exist_ok=True)
        config.write_text(f'[tui]\ntheme = "haarness"\nvariant = "{variant}"\nbackground_mode = "theme"\n')
        tmux("respawn-pane", "-k", "-t", SESSION, resume)
        until(lambda: "survives" in pane(), f"the TUI did not reopen on the chat ({name})")
        if language == "ar":
            prompt("/language ar")
            until(lambda: "في الخلفية" in pane(), "Arabic did not come back")
        time.sleep(1)
        screen(f"20-chat-{name}")
        keys("C-o")
        until(lambda: pane().count("survives") >= 2, f"/tasks did not open ({name})")
        keys("Enter")
        until(lambda: pane().startswith(" bash "), f"the output did not open ({name})")
        time.sleep(1)
        screen(f"21-output-{name}")
        keys("Escape")
        if language == "ar":
            prompt("/language en")
    config.unlink()
    ok("the chat and the output view render in the dark and light palettes and in Arabic")

    # Ctrl+C twice still quits; the server keeps the command in the background running.
    keys("C-c")
    until(lambda: "Press Ctrl+C again to quit" in pane(), "the first Ctrl+C did not offer to quit")
    keys("C-c")
    until(lambda: subprocess.run(["tmux", "-L", SOCKET, "has-session", "-t", SESSION],
                                 capture_output=True, env=env).returncode != 0, "Ctrl+C twice did not quit")
    assert command_with(thread, "survives")["state"] == "running", commands(thread)
    ok("Ctrl+C twice quits while a command runs in the background, which keeps running")
finally:
    if connection:
        for chat in api("/threads") or []:
            for entry in commands(chat["id"]) or []:
                if entry.get("state") == "running":
                    api(f"/threads/{chat['id']}/commands/{entry['id']}/stop", {})
    subprocess.run(["tmux", "-L", SOCKET, "kill-server"], capture_output=True)
    if connection:
        subprocess.run([str(BINARY), "--workspace", str(project), "serve", "--stop"], env=env,
                       capture_output=True, timeout=60)
    print(f"{len(passed)} passed", flush=True)
    e2e_scratch.settle(root)
