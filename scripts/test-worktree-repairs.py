#!/usr/bin/env python3
"""Worktrees Git keeps in unusual ways, end to end with the real CLI, server and TUI.

1. A project that is a worktree of a bare repository (`git clone --bare`, then `git worktree add`):
   a worktree Haarness creates from it opens, resets and is removed, through `haarness worktree` and
   through the server's `/v1/workspaces` routes Desktop uses. A checkout of another repository is still
   refused as a different project.
2. A worktree whose link to its repository broke (its `.git` names a Git folder that is gone): `haarness
   worktree remove` forgets it and says in one line that its folder was left on disk, with the work in
   it; the TUI's /workspace Remove says the same in its notice. A worktree whose link works is still
   removed whole, and one whose repository is out of reach (moved) is kept, naming `git worktree repair`.

Usage: HAARNESS_BIN=target/debug/haarness python3 scripts/test-worktree-repairs.py
Set HAARNESS_E2E_DIR to keep the run's state under a chosen folder.
"""
import json
import os
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
import uuid
from pathlib import Path

import e2e_scratch
import e2e_tmux
from e2e_env import clean_env

repo = Path(__file__).resolve().parents[1]
binary = Path(os.environ.get("HAARNESS_BIN", repo / "target/debug/haarness")).resolve()
root = Path(tempfile.mkdtemp(prefix="haarness-worktree-repairs-", dir=os.environ.get("HAARNESS_E2E_DIR"))).resolve()
home = root / "home"
home.mkdir()
env = clean_env()
env.update(HOME=str(home), HAARNESS_HOME=str(home), HAARNESS_DATA_DIR=str(root / "data"), HAARNESS_TEST_PROVIDER="1",
           GIT_AUTHOR_NAME="Fixture", GIT_AUTHOR_EMAIL="fixture@example.test",
           GIT_COMMITTER_NAME="Fixture", GIT_COMMITTER_EMAIL="fixture@example.test")
checks = []
servers = []
sessions = []


def git(*args, cwd=root):
    subprocess.run(["git", *map(str, args)], cwd=cwd, env=env, check=True, capture_output=True)


def cli(workspace, *args, ok=True):
    result = subprocess.run([str(binary), "--workspace", str(workspace), *map(str, args)], env=env, text=True,
                            capture_output=True, timeout=120, stdin=subprocess.DEVNULL)
    if ok:
        assert result.returncode == 0, f"{args}: {result.stdout}\n{result.stderr}"
    return result


def check(label, condition=True):
    assert condition, label
    checks.append(label)
    print("PASS", label, flush=True)


def listed(workspace):
    return json.loads(cli(workspace, "worktree", "list", "--json").stdout)


def repository(name):
    folder = root / name
    folder.mkdir()
    git("init", "-q", "-b", "main", folder)
    (folder / "tracked.txt").write_text("base\n")
    git("add", "tracked.txt", cwd=folder)
    git("commit", "-q", "-m", "base", cwd=folder)
    return folder


def api(connection, method, path, body=None):
    request = urllib.request.Request(connection["url"] + "/v1" + path, method=method,
                                     data=None if body is None else json.dumps(body).encode(),
                                     headers={"Authorization": "Bearer " + connection["token"],
                                              "Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(request, timeout=60) as response:
            raw = response.read()
            return json.loads(raw) if raw else None
    except urllib.error.HTTPError as error:
        raise AssertionError(f"{method} {path}: {error.code} {error.read().decode()}") from error


def pane(session):
    return subprocess.run(["tmux", "capture-pane", "-p", "-t", session], capture_output=True, text=True).stdout


def until(predicate, label, timeout=45):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(.15)
    raise AssertionError(label)


try:
    # 1. A worktree of a bare repository.
    origin = repository("origin")
    bare = root / "bare.git"
    git("clone", "-q", "--bare", origin, bare)
    main = root / "main"
    git("worktree", "add", "-q", main, "main", cwd=bare)
    created = cli(main, "worktree", "create", "bare feature")
    path = Path(cli(main, "worktree", "open", "bare feature", "--path-only").stdout.strip())
    check("haarness worktree create and open work in a project that is a bare repository's worktree",
          "created worktree" in created.stdout and path.is_dir())
    (path / "tracked.txt").write_text("changed\n")
    cli(main, "worktree", "reset", "bare feature", "--confirm", "--discard-changes")
    check("haarness worktree reset resets it", (path / "tracked.txt").read_text() == "base\n")
    cli(main, "worktree", "remove", "bare feature", "--confirm")
    check("haarness worktree remove removes it", not path.exists()
          and all(item["name"] != "bare feature" for item in listed(main)["workspaces"]))

    connection = json.loads(subprocess.run([str(binary), "--workspace", str(main), "--trust-project", "desktop"],
                                           input='{"action":"connect"}', env=env, text=True, capture_output=True,
                                           timeout=120, check=True).stdout)
    servers.append(main)
    current = next(item for item in api(connection, "GET", "/workspaces")["workspaces"] if item["path"] == str(main.resolve()))
    made = api(connection, "POST", "/workspaces", {"name": "api feature"})["workspace"]
    opened = api(connection, "POST", f'/workspaces/{made["id"]}/open', {})
    reset = api(connection, "POST", f'/workspaces/{made["id"]}/reset', {"confirm": True})
    removed = api(connection, "DELETE", f'/workspaces/{made["id"]}', {"confirm": True})
    check("The server's create, open, reset and remove routes all work for it",
          made["projectId"] == current["projectId"] and (opened.get("workspace") or opened)["id"] == made["id"] and "reset worktree" in reset["message"]
          and "removed worktree" in removed["message"] and not Path(made["path"]).exists())

    # 2. A worktree whose link to its repository broke.
    project = repository("project")
    cli(project, "worktree", "create", "unlinked")
    unlinked = Path(cli(project, "worktree", "open", "unlinked", "--path-only").stdout.strip())
    (unlinked / "notes.txt").write_text("keep me\n")
    (unlinked / ".git").write_text("gitdir: /nowhere/at/all/.git/worktrees/unlinked\n")
    refused = cli(project, "worktree", "open", "unlinked", ok=False)
    check("Opening it says its link is broken and how to forget it",
          refused.returncode != 0 and "no longer links to its repository" in refused.stderr)
    forgot = cli(project, "worktree", "remove", "unlinked", "--confirm")
    lines = forgot.stdout.strip().splitlines()
    check("haarness worktree remove forgets it in one line and leaves its folder with the work in it",
          len(lines) == 1 and lines[0].startswith("worktree forgotten: ") and "its folder was left on disk" in lines[0]
          and (unlinked / "notes.txt").read_text() == "keep me\n"
          and all(item["name"] != "unlinked" for item in listed(project)["workspaces"]))
    cli(project, "worktree", "create", "linked")
    linked = Path(cli(project, "worktree", "open", "linked", "--path-only").stdout.strip())
    removed = cli(project, "worktree", "remove", "linked", "--confirm")
    check("A worktree whose link works is still removed whole", "removed worktree" in removed.stdout and not linked.exists())

    # A repository out of reach (moved, or on a disk not connected) keeps its worktree.
    elsewhere = repository("elsewhere")
    cli(elsewhere, "worktree", "create", "away")
    away = next(item for item in listed(elsewhere)["workspaces"] if item["name"] == "away")
    moved = root / "elsewhere-moved"
    elsewhere.rename(moved)
    kept = cli(main, "worktree", "remove", away["id"], "--confirm", ok=False)
    still = any(item["id"] == away["id"] for item in listed(main)["workspaces"])
    moved.rename(elsewhere)
    check("A worktree whose repository is out of reach is kept, saying how to repair it",
          kept.returncode != 0 and "not reachable" in kept.stderr and "git worktree repair" in kept.stderr and still)

    # The TUI's /workspace Remove.
    cli(project, "worktree", "create", "tui unlinked")
    shown = Path(cli(project, "worktree", "open", "tui unlinked", "--path-only").stdout.strip())
    (shown / ".git").write_text("gitdir: /nowhere/at/all/.git/worktrees/tui\n")
    session = "haarness-worktree-" + uuid.uuid4().hex[:8]
    sessions.append(session)
    servers.append(project)
    launch = e2e_tmux.pane_command(env, [binary, "--workspace", project, "--trust-project"])
    subprocess.run(["tmux", "new-session", "-d", "-s", session, "-x", "160", "-y", "40", launch], check=True)
    until(lambda: "/ commands" in pane(session), "the TUI did not start", 60)
    for keys in (["-l", "/workspace"], ["Enter"]):
        subprocess.run(["tmux", "send-keys", "-t", session, *keys], check=True)
    until(lambda: "Projects and worktrees" in pane(session), "the workspace dashboard did not open")
    subprocess.run(["tmux", "send-keys", "-t", session, "-l", "tui unlinked"], check=True)
    until(lambda: "Search  tui unlinked" in pane(session), "the dashboard did not take the filter")
    subprocess.run(["tmux", "send-keys", "-t", session, "Enter"], check=True)
    until(lambda: "Remove worktree…" in pane(session), "the worktree's actions did not open")
    # From Open, past Reset, to Remove.
    subprocess.run(["tmux", "send-keys", "-t", session, "Down", "Down", "Enter"], check=True)
    until(lambda: "Remove clean worktree" in pane(session), "the removal choices did not open")
    subprocess.run(["tmux", "send-keys", "-t", session, "Down"], check=True)
    time.sleep(.3)
    subprocess.run(["tmux", "send-keys", "-t", session, "Enter"], check=True)
    notice = until(lambda: next((line for line in pane(session).splitlines() if "forgotten · it no longer links" in line), None),
                   "the TUI did not say the worktree was forgotten")
    (root / "tui-notice.txt").write_text(pane(session))
    check("The TUI says it forgot the worktree and left its folder on disk",
          "its folder was left on disk" in notice and shown.is_dir())
    print(f"{len(checks)} checks passed", flush=True)
finally:
    for session in sessions:
        subprocess.run(["tmux", "kill-session", "-t", session], capture_output=True)
    for workspace in servers:
        subprocess.run([str(binary), "--workspace", str(workspace), "serve", "--stop"], env=env, capture_output=True,
                       timeout=60)
    e2e_scratch.settle(root)
