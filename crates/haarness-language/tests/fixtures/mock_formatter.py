#!/usr/bin/env python3
import pathlib
import sys


path = pathlib.Path(sys.argv[-1])
# newline="": the file's own line ends, as a formatter keeps them (Windows
# would otherwise write each as \r\n).
with open(path, encoding="utf-8", newline="") as source:
    text = source.read()
with open(path, "w", encoding="utf-8", newline="") as target:
    target.write(text.replace("BADFMT", "GOODFMT"))
