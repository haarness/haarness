#!/usr/bin/env python3
"""Verify vendored ripgrep payloads; --fetch recreates them from pinned upstream releases."""
import gzip
import hashlib
import io
import json
from pathlib import Path
import sys
import tarfile
import urllib.request
import zipfile

root = Path(__file__).resolve().parent
manifest = json.loads((root / "manifest.json").read_text())
for target, asset in manifest["assets"].items():
    path = root / "artifacts" / asset["file"]
    if "--fetch" in sys.argv:
        data = urllib.request.urlopen(asset["source"], timeout=60).read()
        assert hashlib.sha256(data).hexdigest() == asset["source_sha256"], target
        if asset["source"].endswith(".zip"):
            archive = zipfile.ZipFile(io.BytesIO(data))
            names = [n for n in archive.namelist() if Path(n).name == asset["binary"]]
            assert len(names) == 1
            binary = archive.read(names[0])
        else:
            archive = tarfile.open(fileobj=io.BytesIO(data))
            names = [n for n in archive.getmembers() if n.isfile() and Path(n.name).name == asset["binary"]]
            assert len(names) == 1
            binary = archive.extractfile(names[0]).read()
        assert hashlib.sha256(binary).hexdigest() == asset["binary_sha256"], target
        payload = bytearray(gzip.compress(binary, mtime=0))
        payload[9] = 3  # Fixed gzip OS header for reproducible cross-platform vendoring.
        assert hashlib.sha256(payload).hexdigest() == asset["archive_sha256"], target
        path.write_bytes(payload)
    data = path.read_bytes()
    assert hashlib.sha256(data).hexdigest() == asset["archive_sha256"], target
    assert hashlib.sha256(gzip.decompress(data)).hexdigest() == asset["binary_sha256"], target
    print(f"verified ripgrep {manifest['version']}: {target}")
