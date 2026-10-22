#!/usr/bin/env python3
"""Real-binary proof that opening Review runs none of the programs a repository's own config names.

October 22. A served workspace whose `.git/config` names a clean filter, a process filter, a filter
whose name holds `=`, a file system monitor and a `post-index-change` hook, with a changed submodule
whose own config names a filter: `GET /v1/vcs/diff` lists the changes and no program leaves its mark.
A partial clone whose remote names an upload-pack command: the branch review fetches nothing and says
to run `git backfill` instead of quoting Git.

Usage: python3 scripts/test-repository-config.py   (HAARNESS_BIN picks the binary, HAARNESS_E2E_ROOT a
folder to keep the fixture in). The echo provider is configured; no model is called.
"""
import json
import os
import re
import secrets
import shutil
import subprocess
import tempfile
import threading
import queue
import time
import urllib.error
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get('HAARNESS_BIN', REPO / 'target/debug/haarness')).resolve()
keep = os.environ.get('HAARNESS_E2E_ROOT')
holder = None if keep else tempfile.TemporaryDirectory(prefix='haarness-repository-config-')
root = Path(keep or holder.name).resolve()
if keep and root.exists():
    shutil.rmtree(root)
root.mkdir(parents=True)
home, data, marks = root / 'home', root / 'data', root / 'marks'
for folder in (home, data, marks):
    folder.mkdir()
token = secrets.token_hex(24)
env = {'PATH': os.environ['PATH'], 'HOME': str(home), 'HAARNESS_HOME': str(home), 'HAARNESS_DATA_DIR': str(data),
       'USER': os.environ.get('USER', 'fixture'), 'LANG': 'en_US.UTF-8', 'HAARNESS_TEST_PROVIDER': '1',
       'HAARNESS_SERVER_TOKEN': token, 'GIT_CONFIG_NOSYSTEM': '1'}
servers = []
checks = []


def git(folder, *args):
    result = subprocess.run(['git', '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.test',
                             '-c', 'protocol.file.allow=always', *args],
                            cwd=folder, env=env, capture_output=True, text=True)
    assert result.returncode == 0, f'git {args}: {result.stderr}'
    return result.stdout


def mark(name, then):
    """A shell command that leaves the mark `name`, then runs `then`."""
    return f"touch '{marks / name}'; {then}"


def left():
    return sorted(path.name for path in marks.iterdir())


def script(path, body):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text('#!/bin/sh\n' + body + '\n')
    path.chmod(0o755)


def repository(name):
    folder = root / name
    folder.mkdir()
    git(folder, 'init', '-q', '-b', 'main')
    (folder / '.haarness').mkdir()
    (folder / '.haarness' / 'config.toml').write_text('[defaults]\nprovider="echo"\nmodel="echo-1"\n')
    return folder


def serve(folder):
    ready = queue.Queue()
    log = (root / f'{folder.name}-server.log').open('w')
    process = subprocess.Popen([str(BINARY), '--workspace', str(folder), '--trust-project', 'serve', '--port', '0'],
                               env=env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=log, text=True)
    servers.append((process, log))

    def read():
        for line in process.stdout:
            found = re.search(r'Haarness server listening on (http://\S+)', line)
            if found:
                ready.put(found[1])
        ready.put(None)

    threading.Thread(target=read, daemon=True).start()
    url = ready.get(timeout=60)
    assert url, (root / f'{folder.name}-server.log').read_text()[-2000:]
    return url


def review(url, mode):
    request = urllib.request.Request(f'{url}/v1/vcs/diff?mode={mode}', headers={'Authorization': 'Bearer ' + token})
    try:
        with urllib.request.urlopen(request, timeout=60) as response:
            return response.status, json.loads(response.read())
    except urllib.error.HTTPError as error:
        return error.code, json.loads(error.read() or b'null')


def passed(label):
    checks.append(label)
    print('PASS', label, flush=True)


def case_working_tree():
    module = repository('module')
    (module / '.gitattributes').write_text('*.txt filter=inner\n')
    (module / 'inner.txt').write_text('inner\n')
    git(module, 'add', '--all')
    git(module, 'commit', '-q', '-m', 'module')

    work = repository('review')
    (work / '.gitattributes').write_text('*.txt filter=evil\n*.dat filter=proc\n*.eq filter=a=b\n')
    for name, text in [('a.txt', 'one\n'), ('b.dat', 'bbb\n'), ('c.eq', 'ccc\n'), ('same.md', 'same\n')]:
        (work / name).write_text(text)
    git(work, 'add', '--all')
    git(work, 'commit', '-q', '-m', 'baseline')
    git(work, 'submodule', '-q', 'add', str(module), 'module')
    git(work, 'commit', '-q', '-m', 'module')
    # The programs, named after the commits so committing ran none of them.
    git(work, 'config', 'filter.evil.clean', mark('clean', 'cat'))
    git(work, 'config', 'filter.evil.required', 'true')
    git(work, 'config', 'filter.proc.process', mark('process', 'exit 1'))
    git(work, 'config', 'filter.proc.required', 'true')
    git(work, 'config', 'filter.a=b.clean', mark('equals', 'cat'))
    script(work / '.git' / 'monitor.sh', mark('fsmonitor', 'exit 1'))
    git(work, 'config', 'core.fsmonitor', str(work / '.git' / 'monitor.sh'))
    script(work / '.git' / 'hooks' / 'post-index-change', mark('hook', 'true'))
    git(work / 'module', 'config', 'filter.inner.clean', mark('submodule', 'cat'))
    # Same-size changes, so Git reads each file through its filter to see whether it changed, and a
    # new modification time on an unchanged file, so the diff refreshes (and writes) the index.
    (work / 'a.txt').write_text('two\n')
    (work / 'b.dat').write_text('BBB\n')
    (work / 'c.eq').write_text('CCC\n')
    (work / 'module' / 'inner.txt').write_text('INNER\n')
    later = time.time() + 3600
    os.utime(work / 'same.md', (later, later))

    url = serve(work)
    status, files = review(url, 'working')
    assert status == 200, files
    listed = sorted(file['file'] for file in files)
    assert listed == ['a.txt', 'b.dat', 'c.eq'], listed
    patch = next(file for file in files if file['file'] == 'a.txt')['patch']
    assert '-one' in patch and '+two' in patch, patch
    assert left() == [], f'programs the repository names ran: {left()}'
    passed('Review lists the changes and runs no filter, process filter, monitor, hook or submodule Git')


def case_partial_clone():
    origin = repository('origin')
    git(origin, 'config', 'uploadpack.allowFilter', 'true')
    (origin / 'file.txt').write_text('one\n')
    git(origin, 'add', '--all')
    git(origin, 'commit', '-q', '-m', 'one')
    git(origin, 'checkout', '-q', '-b', 'feature')
    (origin / 'file.txt').write_text('two\n')
    git(origin, 'commit', '-q', '-am', 'two')
    git(origin, 'checkout', '-q', 'main')
    clone = root / 'partial'
    git(root, 'clone', '-q', '--filter=blob:none', '--branch', 'feature', f'file://{origin}', str(clone))
    git(clone, 'config', 'remote.origin.uploadpack', mark('fetch', 'git-upload-pack'))

    url = serve(clone)
    status, files = review(url, 'working')
    assert status == 200 and files == [], files
    status, failure = review(url, 'branch')
    assert left() == [], f'the review fetched with the remote\'s command: {left()} {failure}'
    assert status >= 400, (status, failure)
    assert failure['message'].endswith(
        'this clone has not downloaded the files the review needs; run git backfill, then try again'), failure
    assert 'promisor' not in failure['message'], failure
    passed('a partial clone\'s branch review fetches nothing and says to run git backfill')


try:
    case_working_tree()
    case_partial_clone()
    (root / 'results.json').write_text(json.dumps(checks, indent=2))
finally:
    for process, log in servers:
        process.terminate()
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=10)
        log.close()
    if holder:
        holder.cleanup()
