"""The environment an end-to-end script starts Haarness with.

A script drops the person's own HAARNESS_* settings (and whatever else it names) so a run cannot
reach their data, then sets its own. Dropping them also dropped what keeps a test run away from
Haarness Cloud, so `clean_env` puts that back: usage statistics off, and Cloud on a closed loopback
port. A script that talks to a Cloud stand-in sets its own HAARNESS_CLOUD_API_URL afterwards.
Schedules are written into the run's own folders and never handed to launchd, systemd or Task
Scheduler, which an isolated data folder does not move.
"""

import os

# What every Haarness a test starts gets, whatever the caller's environment holds.
ISOLATED = {
    "HAARNESS_TELEMETRY": "0",
    "HAARNESS_CLOUD_API_URL": "http://127.0.0.1:9",
    "HAARNESS_SCHEDULE_SKIP_INSTALL": "1",
}


def clean_env(*drop, base=None):
    """`base` (the process environment by default) without each variable whose name starts with
    HAARNESS_ or one of `drop`, plus `ISOLATED`."""
    prefixes = ("HAARNESS_", *drop)
    source = os.environ if base is None else base
    env = {name: value for name, value in source.items() if not name.startswith(prefixes)}
    env.update(ISOLATED)
    return env
