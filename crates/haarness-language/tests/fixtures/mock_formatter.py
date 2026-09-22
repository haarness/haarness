#!/usr/bin/env python3
import pathlib
import sys


path = pathlib.Path(sys.argv[-1])
path.write_text(path.read_text(encoding="utf-8").replace("BADFMT", "GOODFMT"), encoding="utf-8")
