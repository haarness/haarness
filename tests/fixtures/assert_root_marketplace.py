#!/usr/bin/env python3
"""Assert safe materialization of a marketplace-root plugin."""

import json
import os
import pathlib
import sys


data = (pathlib.Path(sys.argv[1]) / "data").resolve()
source = pathlib.Path(sys.argv[2]).resolve()
installed = json.loads((data / "plugins" / "installed.json").read_text())
plugin = installed["root-source-plugin"]
active = pathlib.Path(plugin["path"]).resolve()
cache = (data / "plugins" / "cache" / "root-source-market" / "root-source-plugin").resolve()

assert active == cache, (active, cache)
assert active != source, (active, source)
assert (active / "skills" / "root-source" / "SKILL.md").is_file()
assert not (active / ".git").exists()
alias = active / "skills" / "root-source" / "ALIAS.md"
assert alias.is_symlink(), alias
assert os.readlink(alias) == "SKILL.md", os.readlink(alias)
print("marketplace-root plugin path materialization passed")
