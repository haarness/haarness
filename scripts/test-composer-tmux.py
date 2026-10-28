#!/usr/bin/env python3
"""The TUI's composer features in a real tmux pane, on the echo provider:
long pastes (its own and Desktop's), slash commands shown as typed, the
thinking display (Compact, Show, Hide), and files dropped into the prompt
(media attached, other files and every file in shell mode kept as paths,
media over a message's limit refused with the draft kept). Pane captures,
with their colours, go to COMPOSER_TMUX_EVIDENCE."""
import json
import os
import shlex
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
EVIDENCE = Path(os.environ.get("COMPOSER_TMUX_EVIDENCE", tempfile.gettempdir() + "/haarness-composer-tmux"))
EVIDENCE.mkdir(parents=True, exist_ok=True)
# `light` or `dark` draws the pane in that variant of the editorial theme.
THEME = os.environ.get("COMPOSER_TMUX_THEME", "")
TEMPLATE = "REVIEW_TEMPLATE_START\nReview $ARGUMENTS for correctness.\nList each problem with its line.\nREVIEW_TEMPLATE_END\n"


def run(*args, **kwargs):
    return subprocess.check_output([str(arg) for arg in args], text=True, **kwargs)


def until(check, message, timeout=40):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if result := check():
            return result
        time.sleep(0.1)
    raise AssertionError(message)


with tempfile.TemporaryDirectory(prefix="haarness-composer-") as directory:
    root = Path(directory)
    project = root / "project"
    (project / ".haarness/commands").mkdir(parents=True)
    (project / ".haarness/config.toml").write_text('[defaults]\nprovider="echo"\nmodel="echo-1"\n')
    (project / ".haarness/commands/review-file.md").write_text("---\ndescription: Review one file\n---\n" + TEMPLATE)
    for args in [["init", "-b", "main"], ["add", "."], ["-c", "user.name=Composer", "-c", "user.email=composer@example.test", "commit", "-m", "Fixture"]]:
        run("git", *args, cwd=project, stderr=subprocess.DEVNULL)
    env = dict(
        os.environ,
        HAARNESS_HOME=str(root / "home"),
        HAARNESS_DATA_DIR=str(root / "data"),
        HAARNESS_TEST_PROVIDER="1",
        HAARNESS_NO_CONNECT_PROMPT="1",
        LANG="en_US.UTF-8",
        LC_ALL="en_US.UTF-8",
        COLORTERM="truecolor",
    )
    session = f"haarness-composer-{os.getpid()}"
    checks = []
    try:
        connection = json.loads(run(BINARY, "--workspace", project, "--trust-project", "desktop", input='{"action":"connect"}', env=env))

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

        def screen(name):
            name = f"{name}-{THEME}" if THEME else name
            (EVIDENCE / f"{name}.txt").write_text(pane())
            (EVIDENCE / f"{name}.ansi").write_text(run("tmux", "capture-pane", "-p", "-e", "-t", session))

        def click(label):
            rows = pane().splitlines()
            row = next(index for index, line in enumerate(rows) if label in line)
            column = len(rows[row][: rows[row].index(label)]) + 2
            for event in ("M", "m"):
                text(f"\033[<0;{column};{row + 1}{event}")

        def prompts():
            """The rows of the prompts' own blocks, not the echoed replies."""
            return "\n".join(line for line in pane().splitlines() if line.startswith("▎"))

        def composer():
            """The prompt box: the rows just above the status band's rule."""
            return "\n".join(pane().split("─" * 20)[0].rstrip().splitlines()[-3:])

        def idle():
            until(lambda: not api("/turns/active"), "the turn did not finish")

        def launch():
            run("tmux", "new-session", "-d", "-x", "100", "-y", "90", "-s", session,
                pane_command(env, [BINARY, "--workspace", project, "--trust-project"]))
            until(lambda: "/ commands" in pane(), "the TUI did not open")

        launch()
        if THEME:
            text(f"/themes editorial {THEME}")
            keys("Enter")
            time.sleep(1)

        # The TUI's own long paste stays one chip and is sent whole.
        pasted = "\n".join(f"tui paste line {index}" for index in range(1, 21))
        run("tmux", "set-buffer", "-b", "composer", pasted)
        run("tmux", "paste-buffer", "-p", "-b", "composer", "-t", session)
        until(lambda: "[#1 pasted 20 lines]" in pane(), "a long paste did not become a chip")
        screen("tui-paste-chip")
        text(" summarise")
        keys("Enter")
        until(lambda: "+" in pane() and "more lines · ctrl+r or click to expand" in pane(), "the long prompt did not fold")
        idle()
        thread = api("/threads")[0]["id"]
        sent = next(item for item in api(f"/threads/{thread}/items") if item["data"]["type"] == "user_message")
        assert "tui paste line 20" in sent["data"]["text"], sent
        screen("tui-paste-sent")
        checks.append("TUI paste: one [#1 pasted 20 lines] chip, sent whole, folded in the transcript")

        # A paste Desktop sent folds under its label until the prompt opens.
        desktop_paste = "\n".join(f"desktop log line {index}" for index in range(1, 31))
        block = f"<details>\n<summary>Pasted text · 30 lines</summary>\n\n```\n{desktop_paste}\n```\n\n</details>"
        api(f"/threads/{thread}/turns", {"requestId": str(uuid.uuid4()), "text": "Why does this fail?\n\n" + block})
        until(lambda: "▸ Pasted text · 30 lines" in pane(), "a Desktop paste did not fold under its label")
        idle()
        assert "<details>" not in prompts() and "desktop log line 1" not in prompts(), prompts()
        screen("desktop-paste-folded")
        click("▸ Pasted text · 30 lines")
        until(lambda: "▾ Pasted text · 30 lines" in pane() and "▎ desktop log line 30" in prompts(), "the Desktop paste did not open")
        screen("desktop-paste-open")
        click("▾ Pasted text · 30 lines")
        until(lambda: "▸ Pasted text · 30 lines" in pane(), "the Desktop paste did not close")
        checks.append("Desktop paste: one ▸ Pasted text · 30 lines row that a click opens and closes")

        # A slash command reads as the line typed, its prompt folded below.
        text("/review-file src/foo.rs")
        keys("Enter")
        until(lambda: "/review-file src/foo.rs" in pane() and "▸ Show prompt · 4 lines" in pane(), "the command line or its fold did not show")
        idle()
        assert "REVIEW_TEMPLATE_START" not in prompts(), prompts()
        items = api(f"/threads/{thread}/items")
        command = [item for item in items if item["data"]["type"] == "user_message"][-1]["data"]
        assert command.get("command_line") == "/review-file src/foo.rs", command
        assert command["text"].startswith("REVIEW_TEMPLATE_START"), command
        screen("command-folded")
        click("▸ Show prompt · 4 lines")
        until(lambda: "▾ Hide prompt" in pane() and "Review src/foo.rs for correctness." in prompts(), "the command's prompt did not open")
        screen("command-open")
        text("/undo")
        keys("Enter")
        until(lambda: "/review-file src/foo.rs" in composer(), "undo did not bring back the line typed")
        keys("C-u")
        checks.append("Slash command: /review-file src/foo.rs with ▸ Show prompt · 4 lines; click opens it; /undo restores the line typed")

        # Thinking: Compact ticks on one row and settles as a Thought row.
        text("::thinkslow")
        keys("Enter")
        until(lambda: "Thinking · " in pane() and "files it names." in pane(), "the compact thinking row did not tick")
        live = [line for line in pane().splitlines() if "Thinking · " in line]
        assert len(live) == 1, live
        screen("thinking-live")
        until(lambda: "Thinking · " in pane() and "Weighing two ways to fix the parser." in pane(), "the thinking row did not move to the newest line")
        assert "files it names." not in pane(), pane()
        until(lambda: "▹ Thought for" in pane() and "Thought it through" in pane(), "the thought did not settle")
        idle()
        assert "keeps the public API stable" not in pane()
        screen("thought-folded")
        click("▹ Thought for")
        until(lambda: "▿ Thought for" in pane() and "keeps the public API stable" in pane(), "the thought did not open")
        screen("thought-open")
        click("▿ Thought for")
        until(lambda: "▹ Thought for" in pane(), "the thought did not close")
        keys("C-t")
        until(lambda: "Thinking display: Show" in pane() and "keeps the public API stable" in pane() and "▹ Thought for" not in pane(), "Ctrl+T did not switch to Show")
        screen("thinking-show")
        keys("C-t")
        until(lambda: "Thinking display: Hide" in pane() and "thinking hidden" in pane(), "Ctrl+T did not switch to Hide")
        assert "keeps the public API stable" not in pane() and "Thought for" not in pane()
        screen("thinking-hide")
        # The choice outlives the TUI.
        keys("C-c")
        keys("C-c")
        until(lambda: subprocess.run(["tmux", "has-session", "-t", session], capture_output=True).returncode != 0, "the TUI did not exit", timeout=15)
        subprocess.run(["tmux", "kill-session", "-t", session], capture_output=True)
        launch()
        assert "thinking hidden" in pane(), pane()
        keys("C-t")
        until(lambda: "Thinking display: Compact" in pane(), "Ctrl+T did not cycle back to Compact")
        checks.append("Thinking: one Thinking · <newest line> row, ▹ Thought for Ns that a click opens, Ctrl+T Compact → Show → Hide → Compact, kept across launches")

        # Files dropped into the prompt: a terminal pastes a dropped file's
        # path, its spaces escaped.
        drops = root / "drops"
        drops.mkdir()
        (drops / "README.md").write_text("# Notes\n")
        (drops / "design.pdf").write_text("version https://git-lfs.github.com/spec/v1\noid sha256:4d7a\nsize 1048576\n")
        (drops / "report.pdf").write_bytes(b"%PDF-1.7\n%%EOF\n")
        (drops / "screen shot.png").write_bytes(b"\x89PNG\r\n\x1a\nfixture")
        with open(drops / "scan.pdf", "wb") as handle:
            handle.write(b"%PDF-1.7\n")
            handle.truncate(21 * 1024 * 1024)

        def drop(path):
            run("tmux", "set-buffer", "-b", "composer", str(path).replace(" ", "\\ "))
            run("tmux", "paste-buffer", "-p", "-b", "composer", "-t", session)

        def draft():
            """The prompt's text with its wrapped rows put back together."""
            return "".join(line.strip() for line in composer().splitlines())

        for name in ("README.md", "design.pdf"):
            drop(drops / name)
            until(lambda: f"{drops}/{name}" in draft(), f"a dropped {name} did not stay its path")
            time.sleep(0.5)
            assert "Attached" not in pane() and "not a supported" not in pane(), pane()
            assert "No matches" not in pane(), "a dropped path opened the command list"
            screen(f"drop-{name}")
            keys("C-u")
        drop(drops / "report.pdf")
        until(lambda: "Attached 1 file" in pane() and "report.pdf" in composer(), "a dropped PDF was not attached")
        screen("drop-pdf-attached")
        keys("BSpace")
        until(lambda: "report.pdf" not in composer(), "Backspace did not remove the PDF")
        # Two screenshots of 12 MiB each: the server shrinks images, so the
        # prompt takes both.
        for name in ("one.png", "two.png"):
            with open(drops / name, "wb") as handle:
                handle.write(b"\x89PNG\r\n\x1a\n")
                handle.truncate(12 * 1024 * 1024)
            drop(drops / name)
            until(lambda: name in composer(), f"the 12 MiB screenshot {name} was not attached")
        assert "limit for media" not in pane(), pane()
        screen("drop-two-screenshots")
        keys("BSpace")
        keys("BSpace")
        until(lambda: "one.png" not in composer(), "Backspace did not remove the screenshots")
        checks.append("Drops: README.md and a .pdf that is not a PDF stay their paths, no command list; a real PDF and two 12 MiB screenshots are attached")

        text("!")
        until(lambda: "shell — Enter runs the command" in pane(), "shell mode did not open")
        text("ls -l ")
        drop(drops / "screen shot.png")
        until(lambda: f"{drops}/screen\\ shot.png" in draft(), "a file dropped in shell mode did not stay its path")
        assert "Attached" not in pane(), pane()
        screen("drop-shell")
        keys("Enter")

        def items():
            return [item for chat in api("/threads") for item in api(f"/threads/{chat['id']}/items")]

        def listed():
            return any(item["data"]["type"] == "tool_result" and "screen shot.png" in item["data"].get("output", "")
                       for item in items())
        until(listed, "the shell command did not get the dropped path")
        checks.append("Shell mode: a dropped image stays its path and the command runs on it")

        before = len(items())
        text(f"Summarise @{drops}/scan")
        until(lambda: "scan.pdf" in pane(), "the large PDF was not offered")
        keys("Tab")
        until(lambda: "scan.pdf" in composer() and "@" not in composer(), "the large PDF was not attached")
        keys("Enter")
        until(lambda: "scan.pdf is 21 MiB, over the 20 MiB" in pane(), "the refusal of the large PDF did not show")
        screen("large-pdf-refused")
        assert "Summarise" in composer() and "scan.pdf" in composer(), composer()
        assert len(items()) == before, "a refused message was stored"
        keys("C-u")
        keys("BSpace")
        checks.append("A 21 MiB PDF picked with @ is refused in plain words; the draft and its attachment stay")
        for check in checks:
            print("ok -", check)
        print(f"evidence: {EVIDENCE}")
    finally:
        subprocess.run(["tmux", "kill-session", "-t", session], capture_output=True)
        subprocess.run([str(BINARY), "--workspace", str(project), "serve", "--stop"], env=env, capture_output=True)
