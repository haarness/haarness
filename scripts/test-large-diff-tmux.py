#!/usr/bin/env python3
"""A change to a large file, drawn in a real TUI in tmux on the echo provider.

A file over the 32 KiB a tool's result keeps of each text is stored with a diff of its change
instead. An edit near the end of a 40 KB file shows its real changed line, numbered as the file is,
and not "diff content unavailable"; an edit near its start that makes it longer shows that one change
and nothing at the 32 KiB mark; a patch to it draws the same way. The stored result holds the diff
and neither text.
Pane captures, with their colours, go to LARGE_DIFF_EVIDENCE; LARGE_DIFF_THEME=light or dark draws
them in that variant of the editorial theme.

Usage: HAARNESS_BIN=target/debug/haarness python3 scripts/test-large-diff-tmux.py
Set HAARNESS_E2E_DIR to keep the run's state under a chosen folder.
"""
import json
import os
import sqlite3
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
THEME = os.environ.get("LARGE_DIFF_THEME", "")
SOCKET = f"haarness-large-diff-{os.getpid()}"
SESSION = "large"

root = Path(tempfile.mkdtemp(prefix="haarness-large-diff-", dir=os.environ.get("HAARNESS_E2E_DIR"))).resolve()
evidence = Path(os.environ.get("LARGE_DIFF_EVIDENCE", root / "evidence"))
evidence.mkdir(parents=True, exist_ok=True)
project = root / "project"
(project / ".haarness").mkdir(parents=True)
(project / ".haarness/config.toml").write_text(
    '[defaults]\nprovider="echo"\nmodel="echo-1"\n[permissions]\nedit="allow"\n'
)


def numbered(number):
    return f"line {number:05} of a large file, as written"


TEXT = "".join(numbered(number) + "\n" for number in range(1, 1001))
assert len(TEXT) > 32 * 1024 + 6 * 1024, len(TEXT)
(project / "large.txt").write_text(TEXT)
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


def stored_diffs():
    """The `diffs` of every stored tool result, oldest first, read from the chat's own database."""
    found = []
    for database in sorted((root / "data").rglob("haarness.db")):
        connection = sqlite3.connect(f"file:{database}?mode=ro", uri=True)
        try:
            rows = connection.execute("SELECT data_json FROM items ORDER BY created_at, sequence").fetchall()
        finally:
            connection.close()
        for (data,) in rows:
            data = json.loads(data)
            diffs = data.get("metadata", {}).get("diffs") if data.get("type") == "tool_result" else None
            if diffs:
                found.append(diffs)
    return found


def run_tool(name, arguments, shown):
    """Run a tool through the echo provider and open its result, which waits for `shown` in it."""
    paste(f"!tool {name} " + json.dumps(arguments))
    keys("Enter")
    until(lambda: f"Tool {name} =>" in pane() or "updated" in pane(), f"the {name} did not finish")
    time.sleep(0.5)
    keys("C-r")
    return until(lambda: shown in pane() and pane(), f"the {name}'s change did not show")


try:
    tmux("new-session", "-d", "-x", "120", "-y", "45", "-s", SESSION,
         pane_command(env, [BINARY, "--workspace", project, "--trust-project"]))
    until(lambda: "/ commands" in pane(), "the TUI did not open")
    if THEME:
        command("themes", f"editorial {THEME}")

    # Far into the file: past the 32 KiB a text keeps.
    shown = run_tool("edit", {"path": "large.txt", "old": "line 00875 of", "new": "line 00875 (changed) of"},
                     "875 + line 00875 (changed)")
    assert f"875 - {numbered(875)}" in shown, shown
    for number in (873, 874, 876, 877):
        assert f"{number}   {numbered(number)}" in shown, shown
    assert "diff content unavailable" not in shown and "diff content truncated" not in shown, shown
    screen("far-edit")
    [diff] = stored_diffs()[-1]
    assert "before" not in diff and "after" not in diff, diff
    assert diff["patch"].startswith("@@ -872,7 +872,7 @@\n"), diff
    assert (diff["additions"], diff["deletions"]) == (1, 1) and not diff.get("truncated"), diff
    check("an edit far into a 40 KB file shows its real changed line, and the chat stores a diff of it")

    # Near the start, making the file longer: one change, none made up where a text was cut.
    keys("C-r")
    shown = run_tool("edit", {"path": "large.txt", "old": "line 00025 of a large file",
                              "new": "line 00025 of a large file, now with a good deal more text on it"},
                     "25 + line 00025 of a large file, now with")
    changed = [line for line in shown.splitlines() if " + line" in line or " - line" in line]
    assert len(changed) == 2, shown
    assert "diff content" not in shown, shown
    screen("early-edit")
    [diff] = stored_diffs()[-1]
    assert diff["patch"].count("@@ -") == 1, diff
    check("an edit near the start that grows the file shows that change alone")

    # A patch to the same file draws the same way.
    keys("C-r")
    patch = ("*** Begin Patch\n*** Update File: large.txt\n@@\n-" + numbered(990)
             + "\n+line 00990 was patched\n*** End Patch\n")
    shown = run_tool("apply_patch", {"input": patch}, "990 + line 00990 was patched")
    assert f"990 - {numbered(990)}" in shown, shown
    assert "diff content" not in shown, shown
    screen("patch")
    [diff] = stored_diffs()[-1]
    assert diff["patch"].startswith("@@ -987,7 +987,7 @@\n") and "before" not in diff, diff
    check("a patch to a large file shows its real changed line")
    assert (project / "large.txt").read_text().count("\n") == 1000
    print(f"PASS {len(checks)} large file diff scenarios in the TUI.", flush=True)
finally:
    subprocess.run(["tmux", "-L", SOCKET, "kill-server"], capture_output=True)
    e2e_scratch.stop_server(BINARY, project, env)
    e2e_scratch.settle(root)
