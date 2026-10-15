#!/usr/bin/env python3
"""Open terminals survive a client of another Haarness build.

  scripts/test-server-replace-terminals.py [state-folder]   (HAARNESS_BIN picks the binary, default target/debug/haarness)

Two copies of one binary at different paths count as different builds, as
Desktop's bundled binary and an installed CLI do. Build A starts the project
server and a terminal runs in it; build B then connects from Desktop, from the
CLI (through a terminal, so its notice shows), and with the other project
configuration setting. The terminal and the server must survive all three;
once the terminal is closed, B replaces the idle server.
"""

import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.request

REPO = pathlib.Path(__file__).resolve().parents[1]


def main() -> None:
    root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else tempfile.mkdtemp(prefix="haarness-replace-terminals-")).resolve()
    source = pathlib.Path(os.environ.get("HAARNESS_BIN", REPO / "target/debug/haarness")).resolve()
    shutil.rmtree(root, ignore_errors=True)
    workspace, data, home = root / "workspace", root / "data", root / "home"
    for folder in (workspace, data, home, root / "a", root / "b"):
        folder.mkdir(parents=True)
    build_a, build_b = root / "a" / "haarness", root / "b" / "haarness"
    shutil.copy2(source, build_a)
    shutil.copy2(source, build_b)
    env = {**os.environ, "HAARNESS_HOME": str(home), "HAARNESS_TEST_PROVIDER": "1", "RUST_LOG": "warn"}
    env.pop("HAARNESS_DISABLE_PROJECT_CONFIG", None)

    def cli(build, *args, tty=False, trust=True, stdin=None):
        command = [str(build), "--data-dir", str(data), "--workspace", str(workspace)]
        if not trust:
            command.append("--disable-project-config")
        command += list(args)
        if tty:
            # A terminal on stderr, as the TUI and an interactive CLI have.
            command = ["script", "-q", "/dev/null", *command]
        return subprocess.run(command, input=stdin, capture_output=True, text=True, env=env, timeout=120)

    def connect(build):
        done = cli(build, "desktop", stdin=json.dumps({"action": "connect"}))
        assert done.returncode == 0, done.stderr
        return json.loads(done.stdout)

    def api(server, method, path, body=None):
        request = urllib.request.Request(
            server["url"] + path,
            data=None if body is None else json.dumps(body).encode(),
            headers={"authorization": f"Bearer {server['token']}", "content-type": "application/json"},
            method=method,
        )
        with urllib.request.urlopen(request, timeout=10) as response:
            text = response.read()
            return json.loads(text) if text else None

    results = {}
    server = connect(build_a)
    first_pid = api(server, "GET", "/v1/info")["pid"]
    terminal = api(server, "POST", "/v1/ptys", {"title": "npm run dev", "command": "/bin/sh"})
    assert terminal["status"] == "running", terminal
    try:
        # Desktop of the other build: connects to the busy server as it is.
        desktop = connect(build_b)
        results["desktop_upgrade_pending"] = desktop["upgradePending"]
        assert desktop["upgradePending"] is True and desktop["url"] == server["url"], desktop

        # The CLI (and the TUI, through the same call) of the other build.
        listed = cli(build_b, "sessions", "list", tty=True)
        results["cli_exit"] = listed.returncode
        results["cli_notice"] = next((line.strip() for line in listed.stdout.splitlines() if "Using it as it is" in line), None)
        assert listed.returncode == 0, listed.stdout + listed.stderr
        assert results["cli_notice"] and "1 open terminal (npm run dev)" in results["cli_notice"], listed.stdout

        # The other project configuration setting is never used as it is.
        refused = cli(build_b, "sessions", "list", trust=False)
        results["other_setting_exit"] = refused.returncode
        results["other_setting_message"] = refused.stderr.strip().splitlines()[-1] if refused.stderr.strip() else ""
        assert refused.returncode != 0 and "1 open terminal (npm run dev)" in refused.stderr, refused.stderr

        kept = api(server, "GET", f"/v1/ptys/{terminal['id']}")
        results["terminal_after_connects"] = kept["status"]
        assert kept["status"] == "running" and kept["pid"] == terminal["pid"], kept
        assert api(server, "GET", "/v1/info")["pid"] == first_pid

        # Closed, the server is idle and the other build replaces it.
        api(server, "DELETE", f"/v1/ptys/{terminal['id']}")
        listed = cli(build_b, "sessions", "list")
        assert listed.returncode == 0, listed.stderr
        replaced = connect(build_b)
        results["replaced_pid_differs"] = api(replaced, "GET", "/v1/info")["pid"] != first_pid
        results["replaced_upgrade_pending"] = replaced["upgradePending"]
        assert results["replaced_pid_differs"] and replaced["upgradePending"] is False
        results["verdict"] = "PASS"
    finally:
        print(json.dumps(results, indent=2))
        for build in (build_b, build_a):
            cli(build, "serve", "--stop")
        time.sleep(1)


if __name__ == "__main__":
    main()
