#!/usr/bin/env python3
"""Saving configuration files end to end, with the real CLI and the settings requests Desktop sends.

1. A global `haarness.json` that is a link into a dotfiles folder stays a link: `haarness mcp add`
   and an edit saved from Desktop's MCP settings change the file it leads to, keeping its comments.
2. A project's save must land inside the project, Git or not: a `haarness.json` or a `.haarness` folder
   that links out (to the global file, say) is not saved through, and `haarness mcp add --project` says
   so in one line with what to do. A link to a file inside the repository is saved through, and a
   global file in a folder inside a Git work tree still follows its link to dotfiles.
3. Several `haarness mcp add --project` at once against a project's `config.toml` all land, the file keeps its
   permissions, and a reader polling it meanwhile never meets a file it cannot parse.

Usage: HAARNESS_BIN=target/debug/haarness python3 scripts/test-config-saves.py
Set HAARNESS_E2E_DIR to keep the run's state under a chosen folder.
"""
import json
import os
import subprocess
import sys
import tempfile
import threading
import tomllib
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import e2e_scratch
from e2e_env import clean_env

repo = Path(__file__).resolve().parents[1]
binary = Path(os.environ.get("HAARNESS_BIN", repo / "target/debug/haarness")).resolve()
root = Path(tempfile.mkdtemp(prefix="haarness-config-saves-", dir=os.environ.get("HAARNESS_E2E_DIR"))).resolve()
checks = []
servers = []


def environment(name):
    home = root / name / "home"
    state = root / name / "state"
    home.mkdir(parents=True)
    env = clean_env()
    env.update(HOME=str(home), HAARNESS_HOME=str(home), HAARNESS_DATA_DIR=str(state), HAARNESS_TEST_PROVIDER="1")
    return env, state / "config"


def run(*args, env, workspace, input=None, ok=True):
    if (env, workspace) not in servers:
        servers.append((env, workspace))
    result = subprocess.run([str(binary), "--workspace", str(workspace), *map(str, args)], env=env, input=input,
                            text=True, capture_output=True, timeout=90)
    if ok:
        assert result.returncode == 0, f"{args}: {result.stdout}\n{result.stderr}"
    return result


def check(label, condition=True):
    assert condition, label
    checks.append(label)
    print("PASS", label, flush=True)


def add(name, env, workspace, *extra, ok=True):
    return run("mcp", "add", name, "--command", sys.executable, "--arg", "-c", "--arg", "pass", "--disabled",
               *extra, env=env, workspace=workspace, ok=ok)


try:
    # 1. The global file kept in a dotfiles folder.
    env, config = environment("linked")
    project = root / "linked/project"
    project.mkdir(parents=True)
    dotfiles = root / "linked/home/dotfiles"
    dotfiles.mkdir(parents=True)
    kept = dotfiles / "haarness.json"
    kept.write_text('{\n  // kept in my dotfiles\n  "model": "echo/echo-1"\n}\n')
    run("doctor", env=env, workspace=project)
    config.mkdir(parents=True, exist_ok=True)
    (config / "haarness.json").symlink_to(kept)
    add("docs", env, project)
    text = kept.read_text()
    check("haarness mcp add saves through the global file's link, keeping it a link and its comments",
          (config / "haarness.json").is_symlink() and '"docs"' in text and "// kept in my dotfiles" in text)
    request = {"action": "asset", "kind": "mcp", "input": {"action": "edit", "name": "docs", "env": {"MODE": "desktop"}}}
    run("desktop", env=env, workspace=project, input=json.dumps(request))
    text = kept.read_text()
    check("An edit saved from Desktop's MCP settings goes through the link as well",
          (config / "haarness.json").is_symlink() and '"MODE": "desktop"' in text and "// kept in my dotfiles" in text)
    check("Nothing is left beside either file", sorted(path.name for path in dotfiles.iterdir()) == ["haarness.json"])

    # 2. A project's save stays inside the project, Git or not, through a linked file or folder.
    project = root / "linked/repository"
    (project / "shared").mkdir(parents=True)
    subprocess.run(["git", "init", "-q", str(project)], check=True)
    outside = root / "linked/elsewhere.json"
    outside.write_text("{}\n")
    (project / "haarness.json").symlink_to(outside)
    refused = add("project-docs", env, project, "--project", ok=False)
    message = refused.stderr.strip().splitlines()
    check("A project file linked out of its repository is not saved through, in one line that says what to do",
          refused.returncode != 0 and len(message) == 1 and "outside the project" in message[0]
          and "Save the setting in your global configuration instead" in message[0] and outside.read_text() == "{}\n")
    (project / "haarness.json").unlink()
    (project / "shared/haarness.json").write_text("{}\n")
    (project / "haarness.json").symlink_to("shared/haarness.json")
    add("project-docs", env, project, "--project")
    check("A project file linked to a file in its repository is saved through",
          (project / "haarness.json").is_symlink() and '"project-docs"' in (project / "shared/haarness.json").read_text())
    download = root / "linked/download"
    (download / ".haarness").mkdir(parents=True)
    (download / ".haarness/haarness.json").symlink_to(kept)
    before = kept.read_text()
    refused = add("download-docs", env, download, "--project", "--env", "TOKEN=project-only", ok=False)
    check("A project with no Git whose file links to the global one keeps its server out of the global file",
          refused.returncode != 0 and "outside the project" in refused.stderr and kept.read_text() == before)
    folder_linked = root / "linked/folder-linked"
    folder_linked.mkdir()
    subprocess.run(["git", "init", "-q", str(folder_linked)], check=True)
    (folder_linked / ".haarness").symlink_to(config)
    refused = add("folder-docs", env, folder_linked, "--project", ok=False)
    check("A project whose .haarness folder links to the global folder is not saved through either",
          refused.returncode != 0 and "outside the project" in refused.stderr and kept.read_text() == before)
    subprocess.run(["git", "init", "-q", str(config.parent)], check=True)
    add("in-a-work-tree", env, project)
    check("A global file in a Git work tree still saves through its link to dotfiles",
          '"in-a-work-tree"' in kept.read_text() and (config / "haarness.json").is_symlink())

    # 3. Saves at once against a TOML file (a project's: a global one becomes JSON when it loads).
    env, _ = environment("toml")
    project = root / "toml/project"
    (project / ".haarness").mkdir(parents=True)
    toml = project / ".haarness/config.toml"
    toml.write_text('[defaults]\nmodel = "echo/echo-1"\n')
    toml.chmod(0o640)
    unreadable = []
    done = threading.Event()

    def read():
        while not done.is_set():
            try:
                tomllib.loads(toml.read_text())
            except (tomllib.TOMLDecodeError, FileNotFoundError) as error:
                unreadable.append(repr(error))

    reader = threading.Thread(target=read)
    reader.start()
    try:
        with ThreadPoolExecutor(8) as pool:
            for result in pool.map(lambda index: add(f"server{index}", env, project, "--project"), range(16)):
                assert result.returncode == 0
    finally:
        done.set()
        reader.join()
    saved = tomllib.loads(toml.read_text())
    check("16 haarness mcp add at once against config.toml all land",
          sorted(saved["mcp"]) == sorted(f"server{index}" for index in range(16))
          and saved["defaults"]["model"] == "echo/echo-1")
    check("config.toml keeps its permissions", toml.stat().st_mode & 0o777 == 0o640)
    check("A reader never met a half-saved config.toml", not unreadable)
    print(f"{len(checks)} checks passed", flush=True)
finally:
    for env, workspace in servers:
        subprocess.run([str(binary), "--workspace", str(workspace), "serve", "--stop"], env=env, capture_output=True,
                       timeout=60)
    e2e_scratch.settle(root)
