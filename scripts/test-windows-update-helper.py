#!/usr/bin/env python3
"""Desktop's Windows install helper (WINDOWS_HELPER in apps/desktop/src/main/update-install.ts) under PowerShell.

It runs the helper's own text, as Desktop hands it to Windows PowerShell, with stand-ins: a Haarness that exits, a
server of Desktop's (its program in the bin folder) and a server elsewhere, each with the record a real server
leaves, and a setup that notes how it was run. Checked: the helper waits for Haarness, asks only Desktop's server to
stop (with its token, at the address its record names), waits for it, then runs the setup with
`--updated /S --force-run`, and reports success; a setup that fails is reported and Haarness opens again.

Needs `pwsh` (PowerShell 7, on any OS) or HAARNESS_POWERSHELL, of the machine's own architecture: an emulated one
(Docker running an amd64 image on Apple silicon) reports the emulator as every process's program. Without one:
  docker build -q --target pwsh -t haarness-linux-update-test:pwsh scripts/linux-update
  docker run --rm --init -u root -v "$PWD":/src -w /src haarness-linux-update-test:pwsh \
    python3 scripts/test-windows-update-helper.py
Stand-ins are started apart from this process (and `--init` reaps them in a container), as a process that has
exited must be gone: here, as on Windows, not a zombie the helper would still see.
"""
import http.server
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SHELL = os.environ.get("HAARNESS_POWERSHELL") or shutil.which("pwsh") or shutil.which("powershell")
if not SHELL:
    sys.exit("No PowerShell: run in haarness-release-pwsh (see this file's docstring).")

source = (ROOT / "apps/desktop/src/main/update-install.ts").read_text()
helper = re.search(r"export const WINDOWS_HELPER = `(.*?)\n`;", source, re.S).group(1)
helper = helper.replace("\\\\", "\\").replace("\\`", "`").replace("\\$", "$")

# A server as Desktop's: answers /v1/shutdown with its token, notes it, and stops.
SERVER = r'''
import http.server, sys, time
port, token, note = int(sys.argv[1]), sys.argv[2], sys.argv[3]
class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_): pass
    def do_POST(self):
        ok = self.path == "/v1/shutdown" and self.headers.get("Authorization") == "Bearer " + token
        self.send_response(204 if ok else 401); self.end_headers()
        if ok:
            open(note, "a").write("shutdown %f\n" % time.time())
            # Drains a moment, as a server finishing its writes would.
            time.sleep(0.5)
            raise SystemExit(0)
http.server.HTTPServer(("127.0.0.1", port), Handler).serve_forever()
'''


def detached(command, env=None):
    """Starts `command` apart from this process; returns its pid."""
    line = " ".join(f"'{part}'" for part in command) + " >/dev/null 2>&1 & echo $!"
    return int(subprocess.run(["sh", "-c", line], env=env, capture_output=True, text=True, check=True).stdout)


def alive(pid):
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False


def free_port():
    import socket
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def scenario(setup_exit: int):
    root = Path(tempfile.mkdtemp(prefix="haarness-windows-helper-"))
    try:
        bin_dir = root / "Haarness/resources/bin"
        bin_dir.mkdir(parents=True)
        records = root / "cache/workspaces"
        server_py = root / "server.py"
        server_py.write_text(SERVER)
        # Desktop's server runs a program in the bin folder; the other one, a program elsewhere.
        ours_exe = bin_dir / "haarness"
        shutil.copy2(sys.executable, ours_exe)
        servers = {}
        for name, exe in (("ours", ours_exe), ("other", Path(sys.executable))):
            port, token, note = free_port(), "token-" + name, root / (name + ".note")
            env = {**os.environ, "PYTHONHOME": sys.prefix}
            pid = detached([str(exe), str(server_py), str(port), token, str(note)], env)
            record = records / name
            record.mkdir(parents=True)
            # Desktop's server names the any-address, as a server may; the helper still asks on loopback.
            url = f"http://{'0.0.0.0' if name == 'ours' else '127.0.0.1'}:{port}"
            (record / "runtime.json").write_text(json.dumps({"pid": pid, "url": url, "token": token}))
            servers[name] = (pid, note)
        time.sleep(0.5)
        setup = root / "setup.sh"
        setup.write_text(
            "#!/bin/sh\n"
            f"printf '%s\\n' \"$@\" > '{root}/setup.args'\n"
            f"[ -e '{root}/ours.note' ] && echo stopped-first > '{root}/setup.order'\n"
            f"kill -0 {servers['ours'][0]} 2>/dev/null && echo server-still-running >> '{root}/setup.order'\n"
            f"exit {setup_exit}\n"
        )
        setup.chmod(0o755)
        app = root / "Haarness.sh"
        app.write_text(f"#!/bin/sh\ntouch '{root}/reopened'\n")
        app.chmod(0o755)
        haarness = detached(["sleep", "1"])
        result = root / "install-result.json"
        env = {
            **os.environ,
            "HAARNESS_UPDATE_PID": str(haarness),
            "HAARNESS_UPDATE_SETUP": str(setup),
            "HAARNESS_UPDATE_RESULT": str(result),
            "HAARNESS_UPDATE_VERSION": "9.9.9",
            "HAARNESS_UPDATE_BIN": str(bin_dir),
            "HAARNESS_UPDATE_RECORDS": str(records),
            "HAARNESS_UPDATE_APP": str(app),
        }
        started = time.time()
        run = subprocess.run([SHELL, "-NoProfile", "-NonInteractive", "-Command", helper], env=env,
                             capture_output=True, text=True, timeout=120)
        assert run.returncode == 0, run.stdout + run.stderr
        assert time.time() - started >= 0.5, "the helper did not wait for Haarness"
        report = json.loads(result.read_text())
        assert (root / "setup.args").read_text().split() == ["--updated", "/S", "--force-run"]
        # Desktop's server stopped on its own, before the setup ran; the other was not asked.
        assert servers["ours"][1].exists(), "Desktop's server was not asked to stop"
        assert not alive(servers["ours"][0])
        assert (root / "setup.order").read_text().split() == ["stopped-first"]
        assert not servers["other"][1].exists(), "a server outside Desktop's folder was asked to stop"
        assert alive(servers["other"][0])
        return report, (root / "reopened").exists()
    finally:
        for pid, _ in locals().get("servers", {}).values():
            if alive(pid):
                os.kill(pid, 9)
        shutil.rmtree(root, ignore_errors=True)


report, reopened = scenario(0)
assert report == {"version": "9.9.9", "ok": True, "reason": ""}, report
assert not reopened, "Haarness was opened again although the setup opens it"
report, reopened = scenario(2)
assert report == {"version": "9.9.9", "ok": False, "reason": "the setup ended with 2"}, report
assert reopened, "a failed setup did not open Haarness again"
print("PASS: waits for Haarness, stops only Desktop's own servers gracefully and first, runs the setup with "
      "--updated /S --force-run, reports success; a failed setup is reported and Haarness opens again")
