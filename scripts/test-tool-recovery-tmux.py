#!/usr/bin/env python3
"""Exercise tool validation, recovery and permission prompts in a real TUI."""
import json
import os
from pathlib import Path
import shlex
import subprocess
import tempfile
import time
import urllib.request


def run(*args, **kwargs):
    return subprocess.check_output(args, text=True, **kwargs)


def until(check, description):
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(0.1)
    raise AssertionError(description)


binary = str(Path(os.environ.get("HAARNESS_BIN", "target/release/haarness")).resolve())
session = f"haarness-recovery-{os.getpid()}"
with tempfile.TemporaryDirectory(prefix="haarness-recovery-tmux-") as directory:
    root = Path(directory)
    project = root / "project"
    config = project / ".haarness"
    config.mkdir(parents=True)
    (project / "sample.txt").write_text("tmux-recovery-file-proof")
    (config / "config.toml").write_text(
        '[defaults]\nprovider="echo"\nmodel="echo-1"\n[permissions]\nbash="ask"\n'
    )
    env = dict(os.environ, HAARNESS_DATA_DIR=str(root / "data"), HAARNESS_TEST_PROVIDER="1")
    command = shlex.join([
        "env", f"HAARNESS_DATA_DIR={env['HAARNESS_DATA_DIR']}", "HAARNESS_TEST_PROVIDER=1",
        "HAARNESS_DEFAULT_PROVIDER=echo", "HAARNESS_DEFAULT_MODEL=echo-1",
        binary, "--workspace", str(project), "--trust-project",
    ])
    try:
        connection = json.loads(run(binary, "--workspace", str(project), "--trust-project", "desktop",
                                    input='{"action":"connect"}', env=env))

        def api(path):
            request = urllib.request.Request(
                connection["url"].rstrip("/") + "/v1" + path,
                headers={"Authorization": "Bearer " + connection["token"]},
            )
            with urllib.request.urlopen(request, timeout=5) as response:
                return json.load(response)

        def pane():
            return run("tmux", "capture-pane", "-p", "-t", session)

        def send(message):
            run("tmux", "send-keys", "-l", "-t", session, " " + message)
            run("tmux", "send-keys", "-t", session, "Enter")

        run("tmux", "new-session", "-d", "-x", "180", "-y", "45", "-s", session, command)
        until(lambda: "h a a r n e s s" in pane(), "TUI did not open")
        send('!tool Write {"path":"invalid.txt","content":17}')
        until(lambda: "content: expected string" in pane(), "Validation feedback was not rendered")
        until(lambda: not api("/turns/active"), "Invalid call left the turn active")
        assert not api("/permissions"), "Invalid payload must not ask for permission"
        assert not (project / "invalid.txt").exists()
        validation = pane()
        send('!tool FUNCTIONS.Read {"path":"sample.txt"}')
        until(lambda: "tmux-recovery-file-proof" in pane(), "Corrected tool did not read the file")
        until(lambda: not api("/turns/active"), "Corrected call did not finish")
        recovery = pane()
        send('!tool FUNCTIONS.Bash {"command":"touch approval-proof"}')
        until(lambda: api("/permissions"), "Capitalization repair bypassed permission prompting")
        assert not (project / "approval-proof").exists()
        until(lambda: "approval-proof" in pane(), "Permission detail did not reach the TUI")
        evidence = Path(os.environ.get("HAARNESS_RECOVERY_TMUX_EVIDENCE", "/tmp/haarness-tool-recovery-tmux.txt"))
        evidence.write_text("VALIDATION\n" + validation + "\nRECOVERY\n" + recovery + "\nPERMISSION\n" + pane())
        print(f"PASS: real TUI validates inputs, recovers in the same chat, and preserves approval; capture: {evidence}")
    finally:
        subprocess.run(["tmux", "kill-session", "-t", session], capture_output=True)
        subprocess.run([binary, "--workspace", str(project), "serve", "--stop"],
                       env=env, capture_output=True, timeout=30)
