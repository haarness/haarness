#!/usr/bin/env python3
"""Clicking project paths in an answer, in a real tmux pane on the echo provider.

The echo provider answers with the prompt, so the answer holds `src/app.rs:42`, a path to nothing, a
symbolic link out of the project, a Markdown link with `#L2` and a link to a file called like an
option. Clicks are SGR mouse reports sent with send-keys. Stand-in editors on PATH log what they get:

- vim (in the terminal): `+42` and the absolute path; the stand-in has the screen while it runs and
  the TUI's screen is the same as before once it exits;
- `code --wait` (a GUI editor): `-g path:42`, no `--wait`; the TUI never leaves the screen;
- nothing set: `open -t` on macOS, `vi` elsewhere;
- the path to nothing and the link out open nothing; an editor that is not installed is named in a
  notice, in English and in Arabic;
- the pointer over a path underlines that path and nothing else;
- an editor that dies in raw mode, from a click and from Alt+E, leaves the shell's terminal settings
  intact once the TUI quits, and the TUI sets its window title again.

Colour captures go to OPEN_FILE_EVIDENCE; OPEN_FILE_ROOT is where the run's folder is made.
"""
import os
import re
import subprocess
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from e2e_tmux import drop_socket, pane_command, socket_path  # noqa: E402

ROOT = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get("HAARNESS_BIN", ROOT / "target/debug/haarness")).resolve()
EVIDENCE = Path(os.environ.get("OPEN_FILE_EVIDENCE", tempfile.gettempdir() + "/haarness-open-file"))
EVIDENCE.mkdir(parents=True, exist_ok=True)
PROMPT = (
    "Open `src/app.rs:42`, not `src/missing.rs:3` or `src/outside.rs`. "
    "Read [the notes](docs/notes.md#L2) and [the flag file](./--cmd=x.rs)."
)
TERMINAL_EDITOR = """#!/bin/sh
{ printf -- '--- %s\\n' "$(basename "$0")"; printf '%s\\n' "$@"; } >> "$LINKS_RECORD"
printf '\\033[2J\\033[Hstand-in editor\\n'
sleep 1.5
"""
GUI_EDITOR = """#!/bin/sh
{ printf -- '--- %s\\n' "$(basename "$0")"; printf '%s\\n' "$@"; } >> "$LINKS_RECORD"
"""
# Leaves the terminal raw, sets a title of its own and dies without cleaning up.
DYING_EDITOR = """#!/bin/sh
{ printf -- '--- %s\\n' "$(basename "$0")"; printf '%s\\n' "$@"; } >> "$LINKS_RECORD"
stty raw -echo
printf '\\033]0;stand-in title\\007'
kill -9 $$
"""


def run(*args, **kwargs):
    return subprocess.check_output([str(arg) for arg in args], text=True, **kwargs)


def underlines(line):
    """Whether each cell of a `capture-pane -e` row is underlined (ASCII rows: a character a cell)."""
    cells, underline = [], False
    for part in re.split(r"(\x1b\[[0-9;:]*m)", line):
        match = re.fullmatch(r"\x1b\[([0-9;:]*)m", part)
        if not match:
            cells.extend([underline] * len(part))
            continue
        codes = re.split(r"[;]", match.group(1)) or ["0"]
        index = 0
        while index < len(codes):
            code = codes[index].split(":")[0]
            if code in ("", "0"):
                underline = False
            elif code == "4":
                underline = True
            elif code == "24":
                underline = False
            elif code in ("38", "48", "58") and index + 1 < len(codes) and ":" not in codes[index]:
                # 38;5;n or 38;2;r;g;b
                index += 2 if codes[index + 1] == "5" else 4
            index += 1
    return cells


def until(check, message, timeout=40):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if result := check():
            return result
        time.sleep(0.1)
    raise AssertionError(message)


with tempfile.TemporaryDirectory(prefix="haarness-open-file-", dir=os.environ.get("OPEN_FILE_ROOT")) as directory:
    root = Path(directory)
    project = root / "project"
    (project / ".haarness").mkdir(parents=True)
    (project / ".haarness/config.toml").write_text('[defaults]\nprovider="echo"\nmodel="echo-1"\n')
    (project / "src").mkdir()
    (project / "docs").mkdir()
    (project / "src/app.rs").write_text("".join(f"// line {line}\n" for line in range(1, 61)))
    (project / "docs/notes.md").write_text("one\ntwo\nthree\n")
    (project / "--cmd=x.rs").write_text("// a file named like an option\n")
    (root / "outside.rs").write_text("// beside the project\n")
    (project / "src/outside.rs").symlink_to(root / "outside.rs")
    run("git", "init", "-q", "-b", "main", cwd=project)
    real = project.resolve()
    helpers = root / "bin"
    helpers.mkdir()
    for name, script in (("vim", TERMINAL_EDITOR), ("vi", TERMINAL_EDITOR), ("code", GUI_EDITOR),
                         ("open", GUI_EDITOR), ("dying", DYING_EDITOR)):
        (helpers / name).write_text(script)
        (helpers / name).chmod(0o755)
    record = root / "record.txt"
    socket = socket_path(root)
    env = dict(
        os.environ,
        HAARNESS_HOME=str(root / "home"),
        HAARNESS_DATA_DIR=str(root / "data"),
        HAARNESS_TEST_PROVIDER="1",
        HAARNESS_NO_CONNECT_PROMPT="1",
        LANG="en_US.UTF-8",
        LC_ALL="en_US.UTF-8",
        COLORTERM="truecolor",
        LINKS_RECORD=str(record),
        PATH=f"{helpers}:{os.environ['PATH']}",
    )
    env.pop("VISUAL", None)
    env.pop("EDITOR", None)
    session = "links"
    checks = []

    def tmux(*args):
        return run("tmux", "-S", socket, *args)

    def pane(colours=False):
        return tmux("capture-pane", "-p", *(["-e"] if colours else []), "-t", session)

    def body():
        """The pane without its last row, the band, whose notice ("Ready") passes with time."""
        return pane().splitlines()[:-1]

    def screen(name):
        (EVIDENCE / f"{name}.txt").write_text(pane())
        (EVIDENCE / f"{name}.ansi").write_text(pane(colours=True))

    def text(value):
        tmux("send-keys", "-l", "-t", session, value)

    def keys(*values):
        tmux("send-keys", "-t", session, *values)

    def mouse(button, column, row, release=False):
        """One SGR mouse report; `column` and `row` count from 0."""
        text(f"\033[<{button};{column + 1};{row + 1}{'m' if release else 'M'}")

    def click(position):
        mouse(0, *position)
        mouse(0, *position, release=True)

    def reply_row():
        rows = pane().splitlines()
        # The masthead above shows the chat's title, the reply's first words.
        return next(index for index, row in enumerate(rows) if "▸ Echo: Open" in row)

    def find(needle):
        """(column, row) of `needle` in the reply (the prompt above names the same files)."""
        start = reply_row()
        for row, line in enumerate(pane().splitlines()):
            if row >= start and needle in line:
                return len(line[: line.index(needle)]), row
        raise AssertionError(f"{needle!r} not in the reply:\n{pane()}")

    def recorded():
        """What the stand-in editors were given, one list per run: [name, arguments...]."""
        if not record.exists():
            return []
        runs = []
        for line in record.read_text().splitlines():
            if line.startswith("--- "):
                runs.append([line[4:]])
            else:
                runs[-1].append(line)
        return runs

    def launch(editor, theme=None, after=None):
        """Start the TUI with `editor` as $VISUAL; with `after`, a file the pane's terminal settings are
        written to once the TUI has quit."""
        subprocess.run(["tmux", "-S", str(socket), "kill-server"], capture_output=True)
        unset = ["env", "-u", "VISUAL", "-u", "EDITOR"]
        settings = [f"VISUAL={editor}"] if editor else []
        tui = [BINARY, "--workspace", project, "--trust-project"]
        if after:
            tui = ["sh", "-c", '"$@"; stty -a > "$AFTER_TUI"; sleep 600', "sh", *tui]
            settings.append(f"AFTER_TUI={after}")
        # Narrower than the docked sidebar, which lists the chat's title (the answer's first words).
        tmux("new-session", "-d", "-x", "100", "-y", "36", "-s", session,
             pane_command(env, [*unset, *settings, *tui]))
        until(lambda: "/ commands" in pane(), "the TUI did not open")
        if theme:
            text(f"/themes editorial {theme}")
            keys("Enter")
            time.sleep(1)
        text("/new")
        keys("Enter")
        time.sleep(0.5)
        text(PROMPT)
        keys("Enter")
        until(lambda: "▸ Echo: Open" in pane() and "the flag file" in pane().split("▸ Echo: Open", 1)[1],
              f"the reply did not arrive:\n{pane()}")
        # The lookups behind the links answer within a frame or two.
        time.sleep(1)

    try:
        # A terminal editor has the terminal, then the TUI comes back as it was.
        launch(f"{helpers}/vim", theme="dark")
        before = body()
        screen("reply")
        click(find("src/app.rs:42"))
        until(lambda: "stand-in editor" in pane(), f"the editor did not get the terminal:\n{pane()}", timeout=10)
        screen("terminal-editor-running")
        until(lambda: "▸ Echo: Open" in pane(), f"the TUI did not come back:\n{pane()}", timeout=15)
        time.sleep(0.5)
        screen("terminal-editor-returned")
        assert body() == before, f"the TUI came back different:\n{before}\n---\n{pane()}"
        assert recorded() == [["vim", "+42", f"{real}/src/app.rs"]], recorded()
        checks.append("vim gets +42 and the absolute path, has the screen while it runs, and the TUI's screen "
                      "is the same once it exits")

        # Text that names no file of the project is no link.
        for needle in ("src/missing.rs:3", "src/outside.rs"):
            click(find(needle))
            time.sleep(1.5)
            assert "stand-in editor" not in pane(), needle
            assert len(recorded()) == 1, (needle, recorded())
            assert body() == before, f"{needle}: the screen changed:\n{pane()}"
        checks.append("A missing file and a link that leads out of the project open nothing and change nothing")

        # Markdown links: the words, with a line from #L2; a file named like an option gets its full path.
        click(find("the notes"))
        until(lambda: len(recorded()) == 2, f"the notes did not open:\n{pane()}", timeout=10)
        until(lambda: "▸ Echo: Open" in pane() and "stand-in editor" not in pane(), "the TUI did not come back")
        click(find("flag file"))
        until(lambda: len(recorded()) == 3, f"the flag file did not open:\n{pane()}", timeout=10)
        until(lambda: "▸ Echo: Open" in pane() and "stand-in editor" not in pane(), "the TUI did not come back")
        assert recorded()[1:] == [["vim", "+2", f"{real}/docs/notes.md"], ["vim", f"{real}/--cmd=x.rs"]], recorded()
        checks.append("A Markdown link opens at the line its #L names; a file named --cmd=x.rs reaches the editor "
                      "as its absolute path")

        # The pointer over a file underlines it, and only it.
        time.sleep(0.5)
        column, row = find("src/app.rs:42")
        missing = find("src/missing.rs:3")
        before = body()
        assert not any(underlines(pane(colours=True).splitlines()[row])[column:column + 13])
        mouse(35, column + 2, row)
        time.sleep(0.5)
        rows = pane(colours=True).splitlines()
        assert all(underlines(rows[row])[column:column + 13]), repr(rows[row])
        assert not any(underlines(rows[missing[1]])[missing[0]:missing[0] + 16]), repr(rows[missing[1]])
        # The chip's padding is not underlined.
        assert not underlines(rows[row])[column - 1], repr(rows[row])
        assert body() == before, "hovering changed the text on screen"
        screen("hover-dark")
        mouse(35, 0, 0)
        checks.append("The pointer over a file underlines it; the text on screen stays as it was")

        # A GUI editor is started and left to run, its wait option dropped.
        record.unlink(missing_ok=True)
        launch(f"{helpers}/code --wait")
        before = body()
        click(find("src/app.rs:42"))
        until(lambda: recorded(), f"code did not start:\n{pane()}", timeout=10)
        time.sleep(0.5)
        assert recorded() == [["code", "-g", f"{real}/src/app.rs:42"]], recorded()
        assert body() == before, f"the TUI left the terminal:\n{pane()}"
        checks.append("code --wait gets -g path:42 without --wait, and the TUI never leaves the screen")

        # An editor that is not installed says so.
        launch("/nonexistent/code")
        click(find("src/app.rs:42"))
        until(lambda: "code could not open src/app.rs" in pane(), f"no notice:\n{pane()}", timeout=10)
        screen("editor-missing")
        # The same notice in Arabic, read right to left around the names.
        text("/language ar")
        keys("Enter")
        time.sleep(1)
        click(find("src/app.rs:42"))
        until(lambda: "تعذّر على code فتح src/app.rs" in pane(), f"no Arabic notice:\n{pane()}", timeout=10)
        screen("editor-missing-arabic")
        text("/language en")
        keys("Enter")
        time.sleep(1)
        checks.append("An editor that is not installed says it could not open the file, in English and Arabic")

        # No editor set: a text editor that never runs the file.
        record.unlink(missing_ok=True)
        launch(None, theme="light")
        column, row = find("src/app.rs:42")
        mouse(35, column + 2, row)
        time.sleep(0.5)
        screen("hover-light")
        click((column + 2, row))
        until(lambda: recorded(), f"nothing opened:\n{pane()}", timeout=10)
        if sys.platform == "darwin":
            assert recorded() == [["open", "-t", f"{real}/src/app.rs"]], recorded()
            checks.append("With no editor set, macOS opens the file with open -t")
        else:
            until(lambda: "▸ Echo: Open" in pane() and "stand-in editor" not in pane(), "the TUI did not come back")
            assert recorded() == [["vi", "+42", f"{real}/src/app.rs"]], recorded()
            checks.append("With no editor set, vi opens the file at its line")

        # An editor that dies in raw mode, from Alt+E and from a click: the TUI sets its title again,
        # and the shell gets its terminal settings back when the TUI quits.
        record.unlink(missing_ok=True)
        after = root / "after-tui.txt"
        launch(f"{helpers}/dying", after=after)
        title = lambda: tmux("display-message", "-p", "-t", session, "#{pane_title}").strip()
        until(lambda: title().startswith("haarness"), f"no title: {title()!r}", timeout=10)
        keys("M-e")
        until(lambda: len(recorded()) == 1, "Alt+E did not start the editor", timeout=10)
        until(lambda: "▸ Echo: Open" in pane(), f"the TUI did not come back:\n{pane()}", timeout=15)
        click(find("src/app.rs:42"))
        until(lambda: len(recorded()) == 2, "the click did not start the editor", timeout=10)
        until(lambda: "▸ Echo: Open" in pane(), f"the TUI did not come back:\n{pane()}", timeout=15)
        until(lambda: title().startswith("haarness"), f"the title stayed {title()!r}", timeout=10)
        assert recorded()[1] == ["dying", f"{real}/src/app.rs"], recorded()
        keys("C-c")
        time.sleep(0.3)
        keys("C-c")
        until(lambda: after.exists() and after.read_text().strip(), "the TUI did not quit", timeout=20)
        words = after.read_text().split()
        assert "icanon" in words and "echo" in words, after.read_text()
        checks.append("An editor killed in raw mode, from Alt+E or a click, leaves the shell's terminal settings "
                      "and the TUI's title as they were")

        for check in checks:
            print("ok -", check)
        print(f"evidence: {EVIDENCE}")
    except BaseException:
        subprocess.run(["tmux", "-S", str(socket), "capture-pane", "-p", "-t", session])
        raise
    finally:
        subprocess.run(["tmux", "-S", str(socket), "kill-server"], capture_output=True)
        drop_socket(socket, root)
        subprocess.run([str(BINARY), "--workspace", str(project), "serve", "--stop"], env=env, capture_output=True)
