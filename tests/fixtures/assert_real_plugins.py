#!/usr/bin/env python3
"""Assert real marketplace plugins were isolated into Haarness-owned caches."""

import json
import pathlib
import sys


data = (pathlib.Path(sys.argv[1]) / "data").resolve()
installed = json.loads((data / "plugins" / "installed.json").read_text())
for name in ("commit-commands", "plugin-dev"):
    plugin = installed[name]
    active = pathlib.Path(plugin["path"]).resolve()
    expected = (data / "plugins" / "cache" / "claude-code-plugins" / name).resolve()
    assert active == expected, (name, active, expected)
    assert active.is_dir(), active
    assert not (active / ".git").exists(), active

assert (pathlib.Path(installed["commit-commands"]["path"]) / "commands" / "commit.md").is_file()
assert (pathlib.Path(installed["plugin-dev"]["path"]) / "agents" / "plugin-validator.md").is_file()
assert (pathlib.Path(installed["plugin-dev"]["path"]) / "skills" / "plugin-structure" / "SKILL.md").is_file()
print("real plugin cache isolation passed")
