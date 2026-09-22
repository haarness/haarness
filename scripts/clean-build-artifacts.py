#!/usr/bin/env python3
"""Preview or remove Cargo intermediates, preserving runnable build outputs.

macOS/Linux; run without --apply to preview. No session or application data paths
are included. Cargo locks and open-file checks prevent cleaning active artifacts.
"""
import argparse
import fcntl
import os
from pathlib import Path
import shutil
import subprocess
from contextlib import ExitStack

ROOT = Path(__file__).resolve().parents[1]
CACHE_NAMES = ("incremental", "deps", "build", ".fingerprint", "examples")


def allocated(paths):
    if not paths:
        return {}
    # One du invocation counts hardlinked objects across caches only once.
    output = subprocess.check_output(["du", "-sk", *map(str, paths)], text=True)
    return {Path(path): int(size) * 1024 for size, path in
            (line.split("\t", 1) for line in output.splitlines())}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--apply", action="store_true", help="remove the listed intermediates")
    parser.add_argument("--router", action="store_true", help="also include sibling haarness-router")
    parser.add_argument("--incremental-only", action="store_true", help="keep compiled dependency and test caches")
    args = parser.parse_args()
    roots = [ROOT]
    if args.router:
        roots.append(ROOT.parent / "haarness-router")
    candidates, profiles = [], []
    for root in roots:
        target = root / "target"
        if not target.exists():
            continue
        if root.is_symlink() or target.is_symlink():
            parser.error(f"Refusing symlinked build root: {target}")
        tag = target / "CACHEDIR.TAG"
        if not tag.is_file() or not tag.read_text().startswith("Signature: 8a477f597d28d172789f06886806bc55"):
            parser.error(f"Missing Cargo cache marker: {target}")
        for name in ("debug", "debugger"):
            profile = target / name
            if profile.is_symlink():
                parser.error(f"Refusing symlinked profile: {profile}")
            if not profile.is_dir():
                continue
            profiles.append(profile)
            for name in (("incremental",) if args.incremental_only else CACHE_NAMES):
                path = profile / name
                if path.is_symlink():
                    parser.error(f"Refusing symlinked cache: {path}")
                if path.is_dir():
                    candidates.append(path)
    sizes = allocated(candidates)
    total = sum(sizes.values())
    for path in candidates:
        print(f"{sizes[path] / 2**30:7.2f} GiB  {path}")
    print(f"Total: {total / 2**30:.2f} GiB; executable files, release builds, Desktop, and user data are retained.")
    if not args.apply:
        print("Preview only. Add --apply to clean these directories.")
        return
    with ExitStack() as stack:
        for profile in profiles:
            lock_path = profile / ".cargo-lock"
            if lock_path.is_symlink():
                parser.error(f"Refusing symlinked lock: {lock_path}")
            lock = stack.enter_context(lock_path.open("a+"))
            try:
                fcntl.flock(lock.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                parser.error(f"Cargo is using {profile}; wait for the build to finish")
        lsof = shutil.which("lsof")
        if not lsof:
            parser.error("lsof is required to check for running programs before cleaning")
        opened = subprocess.run([lsof, "-nP", "-Fn", "-u", str(os.getuid())], text=True, capture_output=True)
        if opened.returncode not in (0, 1) or not opened.stdout:
            parser.error("Could not inspect open files; nothing was removed")
        for line in opened.stdout.splitlines():
            if line.startswith("n/"):
                for path in candidates:
                    if line[1:].startswith(str(path) + "/"):
                        parser.error(f"Artifact is in use: {line[1:]}; nothing was removed")
        for path in candidates:
            shutil.rmtree(path)
    print(f"Removed intermediates accounting for {total / 2**30:.2f} GiB (retained hardlinks or filesystem snapshots may reduce physical space reclaimed).")


if __name__ == "__main__":
    main()
