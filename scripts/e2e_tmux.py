"""Commands for tmux panes that run the way an end-to-end script's own processes do.

Once a tmux server is running, `tmux new-session` and `new-window` start their command with that
server's environment, not the caller's. What a script set for its processes (an isolated
HAARNESS_HOME, the echo provider, HAARNESS_SCHEDULE_SKIP_INSTALL) is then missing in the pane, and
what it took out (a person's HAARNESS_* settings, provider keys) can come back from the server's
environment. `pane_command` names both, so the pane matches the script.
"""

import os
import re
import shlex

# Kept from the script's environment as they are, whatever the tmux server has.
BASE = ("HOME", "PATH", "USER", "TMPDIR", "LANG", "LC_ALL", "LC_CTYPE")
# Credentials never go on a command line; in the pane they are cleared, as a TUI needs none (the
# fixture's server, started by the script, holds them). A fixture server's own token is not one.
CREDENTIAL = re.compile(r"(_API_KEY|_SECRET|_PASSWORD|_TOKEN)$")
FIXTURE = ("HAARNESS_SERVER_TOKEN", "HAARNESS_SERVER_PASSWORD", "HAARNESS_SERVER_USERNAME")


def pane_command(env, args):
    """A tmux pane command that runs `args` with `env`: every HAARNESS_* variable and credential the
    pane may have from the tmux server is cleared first, then the script's own HAARNESS_* values, the
    base variables and every value the script changed are set. Usage statistics stay off unless `env`
    says otherwise: a server a pane starts must not report a test run to Haarness Cloud."""
    env = {"HAARNESS_TELEMETRY": "0", **env}
    credentials = sorted(
        name for name in set(os.environ) | set(env) if CREDENTIAL.search(name) and name not in FIXTURE
    )
    values = [
        f"{name}={value}"
        for name, value in env.items()
        if name not in credentials
        and (name.startswith("HAARNESS_") or name in BASE or os.environ.get(name) != value)
    ]
    clear = (
        'for name in $(env | sed -n "s/^\\(HAARNESS_[A-Za-z0-9_]*\\)=.*/\\1/p"); do unset "$name"; done; '
        + "".join(f"unset {name}; " for name in credentials)
        + 'exec "$@"'
    )
    return shlex.join(["sh", "-c", clear, "sh", "env", *values, *[str(arg) for arg in args]])
