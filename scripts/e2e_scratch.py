"""Temporary folders of the end-to-end scripts.

Each run holds a whole data folder (databases, logs, project copies), often
several megabytes. A passing run removes its folder; a failing one keeps it as
evidence and prints where. `HAARNESS_E2E_KEEP=1` keeps every run's folder.
Before that, `stop_server` stops the server a run's `haarness` commands started
in the folder, so nothing it started outlives it.
"""

import os

import e2e_env  # noqa: F401 (usage statistics stay off for every Haarness a script starts)
import shutil
import signal
import subprocess
import sys
import time


def stop_server(binary, workspace, env, wait=10.0) -> None:
    """Stop the server that the script's `haarness --workspace <workspace>` commands (`run`, `desktop`, a
    TUI) started and left running, then wait until no process names `workspace` any more. `workspace` is a
    folder the run made under its own temporary folder, so whatever still names it after `wait` seconds is
    the run's own and is killed. Call it in the script's `finally`, before `settle` removes the folder the
    server writes in."""
    try:
        subprocess.run([str(binary), "--workspace", str(workspace), "serve", "--stop"], env=env,
                       capture_output=True, timeout=35)
    except subprocess.TimeoutExpired:
        pass
    if os.name == "nt":
        return

    def naming():
        listing = subprocess.run(["ps", "-A", "-o", "pid=,command="], capture_output=True, text=True).stdout
        return [int(line.split(None, 1)[0]) for line in listing.splitlines()
                if str(workspace) in line and int(line.split(None, 1)[0]) != os.getpid()]

    deadline = time.monotonic() + wait
    while naming() and time.monotonic() < deadline:
        time.sleep(0.1)
    for pid in naming():
        print(f"Stopping process {pid}, which outlived its server", flush=True)
        try:
            os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass


def settle(root) -> None:
    """Call last in the script's `finally`, once its servers have stopped."""
    error = sys.exc_info()[1]
    passed = error is None or (isinstance(error, SystemExit) and error.code in (0, None))
    if not passed or os.environ.get("HAARNESS_E2E_KEEP") == "1":
        print(f"Evidence retained in {root}", flush=True)
        return
    shutil.rmtree(root, ignore_errors=True)
