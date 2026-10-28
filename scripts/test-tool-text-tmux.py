#!/usr/bin/env python3
"""Text a command writes, and text a command carries, in a real TUI in tmux on the echo provider.

A command's colour, cursor and window title sequences show as their text in its output, and none of
them reaches the terminal: the pane's title stays the TUI's and no red is drawn. A command holding a
right-to-left override, a zero-width space and a carriage return shows them as codes on its approval
card, in the muted colour, and on its row, in English and in Arabic, and denying it runs nothing. Pane captures, with
their colours, go to TOOL_TEXT_EVIDENCE; TOOL_TEXT_THEME=light or dark draws them in that variant of
the editorial theme.

Usage: HAARNESS_BIN=target/debug/haarness python3 scripts/test-tool-text-tmux.py
Set HAARNESS_E2E_DIR to keep the run's state under a chosen folder.
"""
import json
import os
import re
import subprocess
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import e2e_scratch  # noqa: E402
from e2e_env import clean_env  # noqa: E402
from e2e_tmux import pane_command  # noqa: E402

ROOT = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get("HAARNESS_BIN", ROOT / "target/debug/haarness")).resolve()
THEME = os.environ.get("TOOL_TEXT_THEME", "")
SOCKET = f"haarness-tool-text-{os.getpid()}"
SESSION = "text"
COLOURED = "printf '\\033]0;window-title-proof\\007\\033[31mred\\033[0m plain\\n'"
HIDDEN = "echo report\u202efdp.sh\u200b && printf done\r\nprintf more > ran.txt"
CODES = ("\\u{202E}", "\\u{200B}", "\\u{D}")

root = Path(tempfile.mkdtemp(prefix="haarness-tool-text-", dir=os.environ.get("HAARNESS_E2E_DIR"))).resolve()
evidence = Path(os.environ.get("TOOL_TEXT_EVIDENCE", root / "evidence"))
evidence.mkdir(parents=True, exist_ok=True)
project = root / "project"
(project / ".haarness").mkdir(parents=True)
# Every command asks, in Review (chats otherwise start in Auto), so the card shows.
(project / ".haarness/config.toml").write_text(
    '[defaults]\nprovider="echo"\nmodel="echo-1"\napproval_mode="review"\n[permissions]\nbash="ask"\n'
)
env = clean_env()
env.update(
    HAARNESS_HOME=str(root / "home"),
    HAARNESS_DATA_DIR=str(root / "data"),
    HAARNESS_TEST_PROVIDER="1",
    HAARNESS_NO_CONNECT_PROMPT="1",
    LANG="en_US.UTF-8",
    LC_ALL="en_US.UTF-8",
    COLORTERM="truecolor",
)
checks = []


def tmux(*args):
    return subprocess.run(["tmux", "-L", SOCKET, *map(str, args)], check=True, text=True, capture_output=True,
                          timeout=30).stdout


def pane():
    return tmux("capture-pane", "-p", "-t", SESSION)


def coloured():
    return tmux("capture-pane", "-p", "-e", "-t", SESSION)


def until(check, label, seconds=45):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if result := check():
            return result
        time.sleep(0.2)
    raise AssertionError(f"{label}\n{pane()}")


def keys(*values):
    tmux("send-keys", "-t", SESSION, *values)


def paste(value):
    """Pasted, so a leading `!` is text rather than the shell prompt."""
    tmux("set-buffer", "-b", "text", value)
    tmux("paste-buffer", "-p", "-b", "text", "-t", SESSION)


def command(name, value):
    keys("C-u")
    tmux("send-keys", "-l", "-t", SESSION, f"/{name} {value}")
    keys("Enter")
    time.sleep(1)


def screen(name):
    name = f"{name}-{THEME}" if THEME else name
    (evidence / f"{name}.txt").write_text(pane())
    (evidence / f"{name}.ansi").write_text(coloured())


def check(label):
    checks.append(label)
    print("PASS " + label, flush=True)


def colour_of(line, needle):
    """The foreground in force where `needle` starts on `line`, a row captured with `-e`."""
    foreground = None
    for match in re.finditer(r"\x1b\[([0-9;]*)m", line[: line.index(needle)]):
        parts = match.group(1).split(";") if match.group(1) else ["0"]
        index = 0
        while index < len(parts):
            part = parts[index]
            if part in ("38", "48") and parts[index + 1:index + 2] == ["2"]:
                if part == "38":
                    foreground = tuple(parts[index + 2:index + 5])
                index += 5
            elif part in ("38", "48") and parts[index + 1:index + 2] == ["5"]:
                if part == "38":
                    foreground = ("index", parts[index + 2])
                index += 3
            else:
                if part in ("0", "39"):
                    foreground = None
                index += 1
    return foreground


try:
    tmux("new-session", "-d", "-x", "120", "-y", "40", "-s", SESSION,
         pane_command(env, [BINARY, "--workspace", project, "--trust-project"]))
    until(lambda: "/ commands" in pane(), "the TUI did not open")
    if THEME:
        command("themes", f"editorial {THEME}")

    # A command's colours and window title.
    paste("!tool bash " + json.dumps({"command": COLOURED}))
    keys("Enter")
    until(lambda: "Allow once" in pane(), "the approval card did not show")
    keys("y")
    until(lambda: "Tool bash =>" in pane(), "the command did not finish")
    keys("C-r")
    until(lambda: "red plain" in pane(), "the output did not open")
    shown = pane()
    output = [line for line in shown.splitlines() if "▏" in line]
    assert any("red plain" in line for line in output), shown
    for remains in ("[31m", "[0m", "]0;", "window-title-proof"):
        assert not any(remains in line for line in output), f"{remains!r} in the output:\n{shown}"
    # The TUI names the window after the chat; the command's own title never reaches it.
    title = tmux("display-message", "-p", "-t", SESSION, "#{pane_title}")
    assert "window-title-proof" not in title, f"the command's title reached the terminal: {title!r}"
    row = next(line for line in coloured().splitlines() if "red plain" in line)
    assert "\x1b[31m" not in row, f"the colour reached the terminal: {row!r}"
    screen("output")
    check("a command's colours and window title show as their text, and none reaches the terminal")

    # A command that carries what would not show.
    for language in ("en", "ar"):
        if language == "ar":
            command("language", "ar")
        paste("!tool bash " + json.dumps({"command": HIDDEN}))
        keys("Enter")
        # The card's command row starts with `$ `; the call's row above it reads `bash echo report…`.
        until(lambda: "$ echo report" in pane(), "the approval card did not show")
        shown = pane()
        assert all(code in shown for code in CODES), shown
        assert "$ echo report\\u{202E}fdp.sh\\u{200B} && printf done\\u{D}" in shown, shown
        assert "bash echo report\\u{202E}fdp.sh\\u{200B} && printf" in shown, f"the call's row:\n{shown}"
        assert "printf more > ran.txt" in shown, shown
        card = [line for line in shown.splitlines() if "\\u{" in line]
        assert not any(re.search("[\u202e\u200b\r]", line) for line in card), f"an invisible character is on the card: {card!r}"
        line = next(line for line in coloured().splitlines() if "\\u{202E}" in line and "$ echo report" in line)
        quiet = colour_of(line, "\\u{202E}")
        text = colour_of(line, "echo report")
        assert quiet and quiet != text, f"the code is not quieter than the command: {line!r}"
        screen(f"approval-{language}")
        keys("n")
        until(lambda: "$ echo report" not in pane(), "the denial did not close the card")
        assert not (project / "ran.txt").exists(), "the denied command ran"
        check(f"an approval shows a direction override, a zero-width space and a carriage return as quiet codes ({language})")
    command("language", "en")
    print(f"PASS {len(checks)} tool text scenarios in the TUI.", flush=True)
finally:
    subprocess.run(["tmux", "-L", SOCKET, "kill-server"], capture_output=True)
    subprocess.run([str(BINARY), "--workspace", str(project), "serve", "--stop"], capture_output=True, timeout=35,
                   env=env)
    e2e_scratch.settle(root)
