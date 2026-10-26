#!/usr/bin/env python3
"""The TUI's home screen says when a newer Haarness is out, and `haarness --update` installs it.

A stand-in release service on this computer answers as haarness.ai does. Checked in a real tmux
pane: the notice shows under the tagline with the command; a service that fails says nothing and
the TUI starts as usual; the answer is kept, so the next launch shows it with the service down;
HAARNESS_NO_UPDATE_CHECK=1 asks nothing; a release with no file for this computer is not offered.
Then `haarness --update` installs the stand-in release through install.sh, checksum and version
checked, and refuses to replace a copy that came with Haarness Desktop.
"""
import hashlib
import http.server
import json
import os
import platform
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "sdks/tests"))
sys.path.insert(0, str(ROOT / "scripts"))
from server import Fixture, BINARY  # noqa: E402
from e2e_tmux import pane_command  # noqa: E402

OS = {"Darwin": "macos", "Linux": "linux"}[platform.system()]
ARCH = {"arm64": "arm64", "aarch64": "arm64", "x86_64": "x64", "AMD64": "x64"}[platform.machine()]
NEW = "9.9.9"
PAYLOAD = f"#!/bin/sh\necho 'haarness {NEW}'\n".encode()


class Service(http.server.BaseHTTPRequestHandler):
    """haarness.ai's release API, as far as the TUI and install.sh read it."""

    mode = "ok"  # ok, down, other-platform
    requests: list[str] = []

    def log_message(self, *_):
        pass

    def manifest(self):
        artifacts = [{"os": OS, "arch": ARCH, "kind": "binary", "file": f"haarness-{OS}-{ARCH}", "id": "a1",
                      "url": f"http://127.0.0.1:{self.server.server_port}/payload", "size": len(PAYLOAD),
                      "digest": {"sha256": hashlib.sha256(PAYLOAD).hexdigest()}}]
        if Service.mode == "other-platform":
            artifacts[0]["os"] = "plan9"
        return {"channel": "stable", "manifest": {"version": NEW, "products": {"cli": {"version": NEW, "artifacts": artifacts}}}}

    def answer(self, body: bytes, kind: str, extra=()):
        self.send_response(200)
        self.send_header("Content-Type", kind)
        self.send_header("Content-Length", str(len(body)))
        for name, value in extra:
            self.send_header(name, value)
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)

    def do_GET(self):
        Service.requests.append(self.path)
        if Service.mode == "down":
            self.send_error(503)
            return
        if self.path == "/api/v1/releases/stable/cli":
            self.answer(json.dumps(self.manifest()).encode(), "application/json")
        elif self.path.startswith("/api/v1/releases/stable/cli/download"):
            self.answer(PAYLOAD, "application/octet-stream", [
                ("X-Content-SHA256", hashlib.sha256(PAYLOAD).hexdigest()), ("X-Haarness-Version", NEW)])
        else:
            self.send_error(404)

    do_HEAD = do_GET


def until(check, description, seconds=30):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(0.1)
    raise AssertionError(description)


service = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Service)
threading.Thread(target=service.serve_forever, daemon=True).start()
release_url = f"http://127.0.0.1:{service.server_port}"
server = Fixture()
session = f"haarness-update-notice-{os.getpid()}"
scratch = Path(tempfile.mkdtemp(prefix="haarness-update-notice-"))
evidence = []


def pane():
    return subprocess.run(["tmux", "capture-pane", "-p", "-t", session], capture_output=True, text=True).stdout


def launch(**extra):
    env = {"HOME": str(server.home), "HAARNESS_DATA_DIR": str(server.data), "HAARNESS_TEST_PROVIDER": "1",
           "HAARNESS_SERVER_TOKEN": server.token, "HAARNESS_RELEASE_URL": release_url, **extra}
    subprocess.run(["tmux", "new-session", "-d", "-x", "120", "-y", "36", "-s", session,
                    pane_command(env, [BINARY, "attach", server.url])], check=True)
    until(lambda: "h a a r n e s s" in pane(), "the TUI did not start")


def close():
    subprocess.run(["tmux", "kill-session", "-t", session], capture_output=True)
    time.sleep(0.3)


def state():
    path = server.data / "config/tui-state.toml"
    return path.read_text() if path.exists() else ""


try:
    server.start()
    notice = f"Haarness {NEW} is available."

    # A service that fails: nothing said, the TUI as usual, nothing kept.
    Service.mode = "down"
    launch()
    until(lambda: any(path.endswith("/stable/cli") for path in Service.requests), "the TUI did not check")
    time.sleep(1)
    assert notice not in pane(), pane()
    assert "latest_release" not in state()
    evidence.append("SERVICE DOWN\n" + pane())
    close()

    # A release without a file for this computer is not offered.
    Service.mode, Service.requests = "other-platform", []
    launch()
    until(lambda: Service.requests, "the TUI did not check")
    time.sleep(1)
    assert notice not in pane(), pane()
    close()
    (server.data / "config/tui-state.toml").write_text("")

    # A newer release: the home screen says so, with the command.
    Service.mode, Service.requests = "ok", []
    launch()
    text = until(lambda: notice in pane() and pane(), "the notice did not show")
    assert "haarness --update" in text, text
    assert "/update" not in text.replace("haarness --update", ""), text
    assert f'latest_release = "{NEW}"' in state(), state()
    evidence.append("NOTICE\n" + text)
    # It belongs to the home screen: a conversation scrolls it away with the masthead.
    subprocess.run(["tmux", "send-keys", "-l", "-t", session, "update-notice-proof"], check=True)
    subprocess.run(["tmux", "send-keys", "-t", session, "Enter"], check=True)
    until(lambda: "update-notice-proof" in pane() and notice not in pane(), "the notice stayed in the conversation")
    close()

    # The answer is kept: the next launch shows it at once, without asking again.
    Service.mode, Service.requests = "down", []
    launch()
    until(lambda: notice in pane(), "the kept answer did not show")
    time.sleep(1)
    assert not Service.requests, Service.requests
    close()

    # Turned off: nothing asked, nothing shown.
    Service.mode, Service.requests = "ok", []
    launch(HAARNESS_NO_UPDATE_CHECK="1")
    time.sleep(2)
    assert notice not in pane() and not Service.requests, (pane(), Service.requests)
    close()

    # `haarness --update` installs the release in place, verified.
    install = scratch / "bin"
    install.mkdir()
    shutil.copy2(BINARY, install / "haarness")
    env = {**os.environ, "HAARNESS_RELEASE_URL": release_url, "HOME": str(server.home)}
    done = subprocess.run([install / "haarness", "--update"], env=env, capture_output=True, text=True, timeout=120)
    assert done.returncode == 0, done.stderr
    assert subprocess.run([install / "haarness", "--version"], capture_output=True, text=True).stdout.strip() == f"haarness {NEW}"
    evidence.append("--update\n" + done.stdout + done.stderr)
    # A copy inside Desktop is Desktop's to update.
    bundled = scratch / "Haarness.app/Contents/Resources/bin"
    bundled.mkdir(parents=True)
    shutil.copy2(BINARY, bundled / "haarness")
    refused = subprocess.run([bundled / "haarness", "--update"], env=env, capture_output=True, text=True, timeout=60)
    assert refused.returncode != 0 and "Haarness Desktop" in refused.stderr, refused.stderr
    assert subprocess.run([bundled / "haarness", "--version"], capture_output=True, text=True).stdout.strip() != f"haarness {NEW}"
    Path(tempfile.gettempdir(), "haarness-update-notice-tmux.txt").write_text("\n\n".join(evidence))
    print("PASS: home-screen notice with `haarness --update`, quiet on failure and on other platforms, kept between "
          "launches, off with HAARNESS_NO_UPDATE_CHECK, gone in a conversation; --update installs verified, refuses Desktop's copy")
except BaseException:
    Path(tempfile.gettempdir(), "haarness-update-notice-tmux-failure.txt").write_text(pane() + "\n\n" + "\n\n".join(evidence))
    raise
finally:
    close()
    server.close()
    service.shutdown()
    shutil.rmtree(scratch, ignore_errors=True)
