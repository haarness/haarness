"""Temporary folders of the end-to-end scripts.

Each run holds a whole data folder (databases, logs, project copies), often
several megabytes. A passing run removes its folder; a failing one keeps it as
evidence and prints where. `HAARNESS_E2E_KEEP=1` keeps every run's folder.
"""

import os
import shutil
import sys


def settle(root) -> None:
    """Call last in the script's `finally`, once its servers have stopped."""
    error = sys.exc_info()[1]
    passed = error is None or (isinstance(error, SystemExit) and error.code in (0, None))
    if not passed or os.environ.get("HAARNESS_E2E_KEEP") == "1":
        print(f"Evidence retained in {root}", flush=True)
        return
    shutil.rmtree(root, ignore_errors=True)
