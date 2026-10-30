#!/usr/bin/env python3
"""Offline bundled rg proof through a copied CLI, authenticated server and PTY.

Uses the private Echo provider; no external AI service or user server is touched.
Run with HAARNESS_BIN pointing to a freshly built CLI or packaged backend.
"""
import concurrent.futures
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time
import urllib.request
from uuid import uuid4

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "sdks/tests"))
import server


def until(check, timeout=20):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = check()
        if result:
            return result
        time.sleep(.05)
    raise AssertionError("Timed out waiting for ripgrep evidence")


def main():
    fixture = server.Fixture()
    original = server.BINARY
    try:
        standalone = fixture.root / "standalone application"
        standalone.mkdir()
        binary = standalone / original.name
        shutil.copy2(original, binary)
        server.BINARY = binary
        # Only the copied executable is shipped. No adjacent rg, vendor folder,
        # package manager, home configuration, or network download is available.
        fixture.env["HAARNESS_HOME"] = str(fixture.root / "private home with spaces")
        # HAARNESS_DATA_DIR outranks HAARNESS_HOME for the cache; drop it so the
        # private home is the only root the copied CLI can extract into.
        fixture.env.pop("HAARNESS_DATA_DIR", None)
        fixture.env["PATH"] = (str(Path(os.environ["SystemRoot"]) / "System32")
                               if os.name == "nt" else "/usr/bin:/bin")
        fixture.env["SHELL"] = "cmd.exe" if os.name == "nt" else "/bin/sh"
        fixture.env.pop("RIPGREP_CONFIG_PATH", None)
        if os.name != "nt":
            (fixture.home / ".profile").write_text("PATH=/usr/bin:/bin; export PATH\n")
        fixture.env.update({"HTTPS_PROXY": "http://127.0.0.1:1", "HTTP_PROXY": "http://127.0.0.1:1", "NO_PROXY": "localhost,127.0.0.1"})
        (fixture.workspace / ".gitignore").write_text("ignored/\n")
        (fixture.workspace / "ignored").mkdir()
        (fixture.workspace / "ignored/private.txt").write_text("needle hidden from search\n")
        (fixture.workspace / ".hidden.txt").write_text("needle hidden dotfile\n")
        (fixture.workspace / "source space").mkdir()
        (fixture.workspace / "source space/café.txt").write_text("first\nneedle café\nlast\n", encoding="utf-8")
        (fixture.workspace / "source space/data.json").write_text('{"needle":true}\n')
        args = [str(binary), "--workspace", str(fixture.workspace), "--trust-project"]
        def doctor():
            return subprocess.check_output([*args, "doctor"], env=fixture.env, text=True, encoding="utf-8", timeout=60)
        with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
            reports = list(pool.map(lambda _: doctor(), range(4)))
        rg = Path(next(line[4:] for line in reports[0].splitlines() if line.startswith("rg: ")))
        manifest = json.loads((ROOT / "vendor/ripgrep/manifest.json").read_text(encoding="utf-8"))
        version = manifest["version"]
        assert all(f"ripgrep: {version}" in report and str(rg) in report for report in reports)
        expected = next(line.removeprefix("rg sha256: ") for line in reports[0].splitlines() if line.startswith("rg sha256: "))
        assert hashlib.sha256(rg.read_bytes()).hexdigest() == expected
        assert rg.is_relative_to(Path(fixture.env["HAARNESS_HOME"]))
        print("PASS copied CLI: offline extraction, concurrent first use, version and hash")

        fixture.start()
        def api(path, body=None, method=None):
            request = urllib.request.Request(fixture.url + "/v1" + path,
                data=None if body is None else json.dumps(body).encode(),
                headers={"Authorization": "Bearer " + fixture.token, "Content-Type": "application/json"},
                method=method)
            with urllib.request.urlopen(request, timeout=30) as response:
                content = response.read()
                return json.loads(content) if content else None
        def approve():
            for pending in api("/permissions"):
                api("/permissions/" + pending["permissionId"], {"decision": "once"})
        def run(command, expected_exit=0):
            thread = api("/threads", {"title": "Bundled ripgrep", "provider": "echo", "model": "echo-1"})
            turn = api(f"/threads/{thread['id']}/turns", {
                "requestId": str(uuid4()), "text": "!tool bash " + json.dumps({"command": command})})
            def done():
                approve()
                state = api("/turns/" + turn["id"])
                return state if state["status"] in ("completed", "failed", "interrupted") else None
            state = until(done)
            assert state["status"] == "completed", state
            items = api(f"/threads/{thread['id']}/items")
            results = [item["data"] for item in items if item["data"]["type"] == "tool_result"]
            result = results[-1]
            assert not result["is_error"], result
            assert result["metadata"]["exit"] == expected_exit, result
            return result["output"]
        locate = "where rg" if os.name == "nt" else "command -v rg"
        assert str(rg) in run(locate)
        assert f"ripgrep {version}" in run("rg --version")
        found = run('rg -n --glob "*.txt" needle "source space"')
        assert "café.txt:2:needle café" in found, found
        files = run('rg --files .')
        assert "café.txt" in files and "private.txt" not in files and ".hidden.txt" not in files, files
        assert ".hidden.txt" in run('rg --files --hidden --glob "*.txt" .')
        run('rg no_such_marker "source space"', 1)
        run('rg "[" "source space"', 2)
        run('rg needle missing-file.txt', 2)
        assert "café" in run('rg needle "source space" | rg café')
        print("PASS server agent turns: bundled resolution, Unicode/spaces, ignores, glob flags, pipeline, exit 0/1/2")
        rg.write_bytes(b"broken cache")
        assert f"ripgrep {version}" in run("rg --version")
        rg.unlink()
        assert f"ripgrep {version}" in run("rg --version")
        print("PASS running server repairs a corrupt or deleted executable")

        command = 'rg -n needle "source space/café.txt" > pty-rg.txt 2> pty-rg-error.txt'
        pty = api("/ptys", {"command": "cmd.exe" if os.name == "nt" else "/bin/sh",
                             "args": ["/C" if os.name == "nt" else "-c", command]})
        try:
            until(lambda: (fixture.workspace / "pty-rg.txt").exists() and (fixture.workspace / "pty-rg.txt").read_text(encoding="utf-8"))
        except AssertionError:
            # What the terminal's command said instead, when it ran at all.
            error = fixture.workspace / "pty-rg-error.txt"
            raise AssertionError("the terminal's rg wrote nothing: "
                                 + (error.read_text(encoding="utf-8", errors="replace") if error.exists() else "it did not run"))
        assert "2:needle café" in (fixture.workspace / "pty-rg.txt").read_text(encoding="utf-8")
        api("/ptys/" + pty["id"], method="DELETE")
        print("PASS real server PTY executes bundled rg")

        output_file = fixture.root / "cli-output.txt"
        with output_file.open("w", encoding="utf-8") as output:
            # `run` stops rather than wait on an approval it cannot give, so it approves its own call.
            process = subprocess.Popen([*args, "run", "--yolo", "--attach", fixture.url, "--provider", "echo", "--model", "echo-1", "--format", "json",
                '!tool bash {"command":"rg --version"}'], env=fixture.env, stdout=output, stderr=subprocess.STDOUT)
            try:
                def cli_done():
                    approve()
                    return process.poll() is not None
                until(cli_done, 60)
                assert process.returncode == 0, output_file.read_text(encoding="utf-8")[-3000:]
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait()
        assert f"ripgrep {version}" in output_file.read_text(encoding="utf-8"), output_file.read_text(encoding="utf-8")[-3000:]
        print("PASS CLI run over the authenticated server returns real ripgrep output")
    finally:
        server.BINARY = original
        fixture.close()


if __name__ == "__main__":
    main()
