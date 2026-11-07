#!/usr/bin/env python3
"""The TUI's undo of a turn that changed files no checkpoint can put back, in a real tmux pane on the
echo provider. One turn writes an ignored file, shrinks a tracked file over the checkpoints' 2 MiB
limit and edits a tracked file. `/undo` puts the tracked file back, leaves the other two as the turn
left them, and says so on one line under the undone marker; `/redo` brings the turn back and the
line goes. The same line in another interface language follows `/language`.

`UNDO_TMUX_THEME` (`light` or `dark`) draws the pane in that variant of the editorial theme, and
`UNDO_TMUX_LANGUAGE` (`ar`, `ja`, ...) also checks the line in that language. Pane captures, with
their colours, go to `UNDO_TMUX_EVIDENCE`. Isolated HOME, HAARNESS_HOME and HAARNESS_DATA_DIR;
stdlib only."""
import json
import os
import subprocess
import sys
import tempfile
import time
import urllib.request
import uuid
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from e2e_tmux import pane_command  # noqa: E402

ROOT = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get("HAARNESS_BIN", ROOT / "target/debug/haarness")).resolve()
EVIDENCE = Path(os.environ.get("UNDO_TMUX_EVIDENCE", tempfile.gettempdir() + "/haarness-undo-tmux"))
EVIDENCE.mkdir(parents=True, exist_ok=True)
THEME = os.environ.get("UNDO_TMUX_THEME", "")
LANGUAGE = os.environ.get("UNDO_TMUX_LANGUAGE", "")
LINE = "2 files weren't restored · assets/model.bin, local.env"
TRANSLATED = {
    "ar": "لم تتم استعادة 2 ملفات · assets/model.bin, local.env",
    "ja": "2 個のファイルは復元されませんでした · assets/model.bin, local.env",
}


def run(*args, **kwargs):
    return subprocess.check_output([str(arg) for arg in args], text=True, **kwargs)


def until(check, message, timeout=40):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if result := check():
            return result
        time.sleep(0.1)
    raise AssertionError(message)


with tempfile.TemporaryDirectory(prefix="haarness-undo-tmux-") as directory:
    root = Path(directory)
    project = root / "project"
    (project / ".haarness").mkdir(parents=True)
    (project / ".haarness/config.toml").write_text(
        '[defaults]\nprovider="echo"\nmodel="echo-1"\n'
        '[permissions]\nread="allow"\nwrite="allow"\nedit="allow"\n[lsp]\nenabled=false\n')
    (project / ".gitignore").write_text("local.env\n")
    (project / "local.env").write_text("TOKEN=fixture\n")
    (project / "tracked.txt").write_text("before\n")
    (project / "assets").mkdir()
    large = bytes(range(256)) * (12 * 1024)
    (project / "assets/model.bin").write_bytes(large)
    for args in [["init", "-b", "main"], ["add", "."],
                 ["-c", "user.name=Fixture", "-c", "user.email=fixture@example.test", "commit", "-m", "Fixture"]]:
        run("git", *args, cwd=project, stderr=subprocess.DEVNULL)
    env = dict(
        os.environ,
        HOME=str(root / "home"),
        HAARNESS_HOME=str(root / "home"),
        HAARNESS_DATA_DIR=str(root / "data"),
        HAARNESS_TEST_PROVIDER="1",
        HAARNESS_NO_CONNECT_PROMPT="1",
        HAARNESS_SCHEDULE_SKIP_INSTALL="1",
        LANG="en_US.UTF-8",
        LC_ALL="en_US.UTF-8",
        COLORTERM="truecolor",
    )
    (root / "home").mkdir()
    session = f"haarness-undo-{os.getpid()}"
    checks = []
    try:
        connection = json.loads(run(BINARY, "--workspace", project, "--trust-project", "desktop",
                                    input='{"action":"connect"}', env=env))

        def api(path, body=None):
            request = urllib.request.Request(
                connection["url"].rstrip("/") + "/v1" + path,
                data=None if body is None else json.dumps(body).encode(),
                headers={"Authorization": "Bearer " + connection["token"], "Content-Type": "application/json"},
            )
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
            text(value)
            time.sleep(0.2)
            keys("Enter")

        def screen(name):
            name = "-".join(part for part in (name, THEME, LANGUAGE) if part)
            (EVIDENCE / f"{name}.txt").write_text(pane())
            (EVIDENCE / f"{name}.ansi").write_text(run("tmux", "capture-pane", "-p", "-e", "-t", session))

        def idle():
            until(lambda: not api("/turns/active"), "the turn did not finish")

        run("tmux", "new-session", "-d", "-x", "100", "-y", "34", "-s", session,
            pane_command(env, [BINARY, "--workspace", project, "--trust-project"]))
        until(lambda: "/ commands" in pane(), "the TUI did not open")
        if THEME:
            command(f"/themes editorial {THEME}")
            time.sleep(1)

        command("Tidy up the settings")
        until(lambda: "Echo: Tidy up the settings" in pane(), "the first turn did not answer")
        idle()
        thread = api("/threads")[0]["id"]
        calls = [{"name": "write", "arguments": {"path": path, "content": content}} for path, content in (
            ("local.env", "TOKEN=changed\n"), ("assets/model.bin", "small now\n"), ("tracked.txt", "after\n"))]
        api(f"/threads/{thread}/turns", {"requestId": str(uuid.uuid4()), "text": "!multi " + json.dumps(calls)})
        idle()
        assert (project / "tracked.txt").read_text() == "after\n"

        command("/undo")
        until(lambda: "1 step undone" in pane(), "the undo did not fold the turn")
        until(lambda: LINE in pane(), "the undo did not say which files it left")
        rows = pane().splitlines()
        marker = next(index for index, row in enumerate(rows) if "1 step undone" in row)
        assert LINE in rows[marker + 1], rows[marker:marker + 2]
        assert rows[marker + 1].startswith("    " + LINE[:1]), rows[marker + 1]
        assert (project / "tracked.txt").read_text() == "before\n"
        assert (project / "local.env").read_text() == "TOKEN=changed\n"
        assert (project / "assets/model.bin").read_bytes() == b"small now\n"
        revert = api(f"/threads/{thread}")["revert"]
        assert revert["unrestored"] == ["assets/model.bin", "local.env"], revert
        screen("undo-unrestored")
        checks.append("/undo restores tracked.txt and says: " + LINE)

        if LANGUAGE:
            command(f"/language {LANGUAGE}")
            expected = TRANSLATED[LANGUAGE]
            until(lambda: expected in pane(), f"the line did not follow /language {LANGUAGE}")
            screen("undo-unrestored-language")
            checks.append(f"/language {LANGUAGE}: " + expected)
            command("/language en")
            until(lambda: LINE in pane(), "the line did not come back in English")

        command("/redo")
        until(lambda: (project / "tracked.txt").read_text() == "after\n", "redo did not bring the turn back")
        until(lambda: "wasn't restored" not in pane() and "weren't restored" not in pane(),
              "the line stayed after redo")
        assert api(f"/threads/{thread}").get("revert") is None
        screen("redo")
        checks.append("/redo brings tracked.txt back and the line goes")
        for check in checks:
            print("ok -", check)
        print(f"evidence: {EVIDENCE}")
    finally:
        subprocess.run(["tmux", "kill-session", "-t", session], capture_output=True)
        subprocess.run([str(BINARY), "--workspace", str(project), "serve", "--stop"], env=env, capture_output=True)
