import os
import stat

import pytest

from haarness_sdk import LocalServer


def test_a_local_server_that_cannot_start_says_why(tmp_path):
    executable = tmp_path / "haarness"
    executable.write_text(
        "#!/bin/sh\necho 'warming up' >&2\necho 'Error: a Haarness server already runs in this folder' >&2\nexit 1\n"
    )
    executable.chmod(executable.stat().st_mode | stat.S_IXUSR)
    with pytest.raises(RuntimeError, match="^Haarness failed to start: Error: a Haarness server already runs in this folder$"):
        LocalServer.start(str(tmp_path), executable=str(executable), startup_timeout=5)
