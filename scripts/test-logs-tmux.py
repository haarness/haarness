#!/usr/bin/env python3
"""`/logs` in a real TUI: export this computer's logs and turn detailed logging on and off.

Drives an attached TUI in tmux, with the echo provider and an isolated data folder:

- `/logs` says detailed logging is off and how to use it; `/lo` completes to it.
- `/logs detail on` raises every Haarness process here to debug for 30 minutes,
  through the same control file `haarness logs level` writes, and the running
  workspace server follows it; `/logs detail` turns it off again.
- `/logs export` writes a scrubbed zip to Downloads, says where, and copies the
  path; a typed folder (made when missing), `~`, a relative path and a quoted
  path without an extension work too, and a file that is not a zip is never
  replaced.
- An unknown argument says how to use it.

Evidence (pane captures) goes to `$HAARNESS_LOGS_EVIDENCE` when set.
"""
from datetime import datetime
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import time
import urllib.request
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "sdks/tests"))
from server import Fixture, BINARY  # noqa: E402

SOCKET = f"haarness-logs-{os.getpid()}"
SESSION = "logs"
evidence_dir = os.environ.get("HAARNESS_LOGS_EVIDENCE")


def tmux(*args):
    return subprocess.run(["tmux", "-L", SOCKET, *args], check=True, capture_output=True, text=True).stdout


def until(check, description, timeout=30):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(0.1)
    raise AssertionError(description + "\n" + pane())


def pane():
    try:
        return tmux("capture-pane", "-p", "-J", "-t", SESSION)
    except subprocess.CalledProcessError:
        return ""


def send(text):
    tmux("send-keys", "-l", "-t", SESSION, text)
    tmux("send-keys", "-t", SESSION, "Enter")


def keep(name):
    if evidence_dir:
        Path(evidence_dir).mkdir(parents=True, exist_ok=True)
        (Path(evidence_dir) / f"{name}.txt").write_text(pane())


server = Fixture()
try:
    server.start()

    def api(path, body=None):
        request = urllib.request.Request(
            server.url + "/v1" + path,
            data=None if body is None else json.dumps(body).encode(),
            headers={"Authorization": "Bearer " + server.token, "Content-Type": "application/json"},
        )
        with urllib.request.urlopen(request, timeout=10) as response:
            return json.load(response)

    thread = api("/threads", {"title": "Logs proof", "provider": "echo", "model": "echo-1"})
    data = server.data / "data"
    downloads = server.home / "Downloads"
    downloads.mkdir()
    started_in = server.root / "started-here"
    started_in.mkdir()
    # The clipboard tools record what they were given instead of touching the
    # person's own clipboard.
    tools = server.root / "bin"
    tools.mkdir()
    copied = server.root / "copied.txt"
    for name in ("pbcopy", "wl-copy", "xclip"):
        tool = tools / name
        tool.write_text(f"#!/bin/sh\ncat > {shlex.quote(str(copied))}\n")
        tool.chmod(0o755)
    # A log line holding a credential, which an export must not carry.
    (data / "logs").mkdir(parents=True, exist_ok=True)
    (data / "logs" / "haarness.log.2026-10-21").write_text(
        "2026-10-21T08:00:00.000Z 4242 serve  WARN haarness_core::provider: request failed "
        "Authorization: Bearer sk-live-SECRETVALUE123456789\n"
        # Shapes a hook or a tool prints: a DB_PASS-style name, a proxy's user and password without a
        # scheme, and a credential parameter of a path.
        "2026-10-21T08:00:01.000Z 4242 serve  WARN haarness_server::hooks: hook command failed stderr=DB_PASS=SECRETDB "
        "curl -x user:SECRETPROXY@proxy.fixture.test:3128 GET /cb?code=SECRETCODE&state=s\n"
    )
    command = shlex.join([
        "env", f"PATH={tools}:/usr/bin:/bin:/usr/sbin:/sbin", f"HOME={server.home}",
        f"HAARNESS_DATA_DIR={server.data}", "HAARNESS_TEST_PROVIDER=1", "LANG=en_US.UTF-8",
        f"HAARNESS_SERVER_TOKEN={server.token}", str(BINARY), "attach", server.url, "--thread", thread["id"],
    ])
    tmux("new-session", "-d", "-x", "190", "-y", "45", "-s", SESSION, "-c", str(started_in), command)
    until(lambda: "h a a r n e s s" in pane(), "the TUI did not attach")

    # The command is in the catalog.
    tmux("send-keys", "-l", "-t", SESSION, "/lo")
    until(lambda: "/logs" in pane() and "This computer's logs" in pane(), "/lo did not offer /logs")
    keep("logs-completion")
    tmux("send-keys", "-t", SESSION, "C-u")

    send("/logs")
    until(lambda: "Detailed logging off · /logs detail on · /logs export [path]" in pane(), "/logs did not say it is off")
    keep("logs-status-off")

    send("/logs detail on")
    until(lambda: "Detailed logging on until " in pane(), "/logs detail on did not answer")
    keep("logs-detail-on")
    control = json.loads((data / "log-level.json").read_text())
    assert control["level"] == "debug", control
    until_at = datetime.fromisoformat(control["until"].replace("Z", "+00:00")).timestamp()
    assert 29 * 60 < until_at - time.time() <= 30 * 60 + 5, control
    # The running workspace server follows it within a few seconds.
    def followed():
        return any("log level raised for a while" in path.read_text(errors="replace")
                   for path in (data / "logs").glob("haarness.log.*"))
    until(followed, "the server did not follow the raised level", timeout=20)

    send("/logs detail")
    until(lambda: "Detailed logging off" in pane(), "/logs detail did not turn it off")
    assert not (data / "log-level.json").exists()

    send("/logs export")
    until(lambda: "Logs saved to " in pane(), "/logs export did not say where it wrote")
    keep("logs-export")
    zips = sorted(downloads.glob("haarness-logs-*.zip"))
    assert len(zips) == 1, zips
    with zipfile.ZipFile(zips[0]) as archive:
        text = "".join(archive.read(name).decode(errors="replace") for name in archive.namelist())
    assert "request failed Authorization:" in text and "SECRETVALUE" not in text, text[:400]
    for secret in ("SECRETDB", "SECRETPROXY", "SECRETCODE"):
        assert secret not in text, (secret, text[:800])
    assert "@proxy.fixture.test:3128 GET /cb?code=[redacted]&state=s" in text, text[:800]
    assert copied.read_text() == str(zips[0]), copied.read_text()
    # Said where, in few enough words to fit the status line.
    assert f"Logs saved to ~/Downloads/{zips[0].name} · path copied" in pane(), pane()

    send("/logs export ~/bug report/")
    until(lambda: list((server.home / "bug report").glob("haarness-logs-*.zip")), "a typed folder was not made and used")
    until(lambda: "Logs saved to ~/bug report/haarness-logs-" in pane(), "the typed folder's export did not finish")
    send("/logs export picked.zip")
    until(lambda: (started_in / "picked.zip").exists(), "a relative path did not start where the TUI started")

    until(lambda: "Logs saved to " in pane() and "picked.zip" in pane(), "the relative export did not finish")
    # As a shell takes it: quotes around a path with a space, no extension.
    send('/logs export "~/My Logs/run two"')
    until(lambda: (server.home / "My Logs" / "run two.zip").exists(), "a quoted path without an extension was not written as .zip")
    until(lambda: "Logs saved to ~/My Logs/run two.zip" in pane(), "the quoted export did not finish")
    # A file that is not a zip is never replaced.
    notes = server.home / "notes.txt"
    notes.write_text("keep me\n")
    send("/logs export ~/notes.txt")
    until(lambda: "notes.txt is not a zip of logs; choose another name" in pane(), "a non-zip file was not refused")
    assert notes.read_text() == "keep me\n"
    keep("logs-refused")

    send("/logs loud")
    until(lambda: "Use /logs export [path] or /logs detail [on|off]" in pane(), "an unknown argument did not say how to use /logs")
    keep("logs-usage")
    print("PASS: /logs status, detail on (the server follows) and off, export to Downloads (scrubbed, path copied), a typed folder, a relative path, a quoted path without extension, refusing a non-zip file, and usage")
finally:
    subprocess.run(["tmux", "-L", SOCKET, "kill-server"], capture_output=True)
    server.close()
