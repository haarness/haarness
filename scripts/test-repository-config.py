#!/usr/bin/env python3
"""Real-binary proof of what opening Review runs from a repository's own config, trusted or not.

October 22. A workspace whose `.git/config` names a clean filter, a process filter, a filter whose
name holds `=`, the empty-named filter, a file system monitor and a `post-index-change` hook, with a
changed submodule whose own config names a filter. With project configuration not trusted
(`--disable-project-config`), `GET /v1/vcs/diff` lists the changes and no program leaves its mark.
Trusted (the default), Review reads as the person's own `git diff`: the filters and the submodule's Git
run, the monitor and the hook never do. A partial clone whose remote names an upload-pack command:
untrusted, the branch review fetches nothing and says to run `git backfill`, in English words the
clients recognise even under a German locale; trusted, it fetches the base as `git diff` would.

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
from e2e_env import ISOLATED

REPO = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get('HAARNESS_BIN', REPO / 'target/debug/haarness')).resolve()
keep = os.environ.get('HAARNESS_E2E_ROOT')
holder = None if keep else tempfile.TemporaryDirectory(prefix='haarness-repository-config-')
root = Path(keep or holder.name).resolve()
if keep and root.exists():
    shutil.rmtree(root)
root.mkdir(parents=True, exist_ok=True)
home, data, marks = root / 'home', root / 'data', root / 'marks'
for folder in (home, data, marks):
    folder.mkdir()
token = secrets.token_hex(24)
env = {**ISOLATED, 'PATH': os.environ['PATH'], 'HOME': str(home), 'HAARNESS_HOME': str(home), 'HAARNESS_DATA_DIR': str(data),
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


def serve(folder, trusted, **extra):
    ready = queue.Queue()
    log = (root / f'{folder.name}-{"trusted" if trusted else "untrusted"}-server.log').open('w')
    trust = [] if trusted else ['--disable-project-config']
    process = subprocess.Popen([str(BINARY), '--workspace', str(folder), *trust, 'serve', '--port', '0'],
                               env={**env, **extra}, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=log,
                               text=True)
    servers.append((process, log))

    def read():
        for line in process.stdout:
            found = re.search(r'Haarness server listening on (http://\S+)', line)
            if found:
                ready.put(found[1])
        ready.put(None)

    threading.Thread(target=read, daemon=True).start()
    url = ready.get(timeout=60)
    assert url, log.name
    return url, process


def stop(process):
    process.terminate()
    process.wait(timeout=10)


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


def submodule(work, name):
    module = repository(f'{name}-module')
    (module / '.gitattributes').write_text('*.txt filter=inner\n')
    (module / 'inner.txt').write_text('inner\n')
    git(module, 'add', '--all')
    git(module, 'commit', '-q', '-m', 'module')
    git(work, 'submodule', '-q', 'add', str(module), 'module')
    git(work, 'commit', '-q', '-m', 'module')
    git(work / 'module', 'config', 'filter.inner.clean', mark(f'{name}-submodule', 'cat'))
    (work / 'module' / 'inner.txt').write_text('INNER\n')


def programs(work, name, filters):
    """The repository's programs, named after the commits so committing ran none of them."""
    for key, value in filters:
        git(work, 'config', key, value)
    with (work / '.git' / 'config').open('a') as config:
        config.write(f'[filter ""]\n\tclean = "{mark(name + "-empty", "cat")}"\n')
    script(work / '.git' / 'monitor.sh', mark(f'{name}-fsmonitor', 'exit 1'))
    git(work, 'config', 'core.fsmonitor', str(work / '.git' / 'monitor.sh'))
    script(work / '.git' / 'hooks' / 'post-index-change', mark(f'{name}-hook', 'true'))


def changed(work, files):
    """Same-size changes, so Git reads each file through its filter to see whether it changed, and a new
    modification time on an unchanged file, so the diff refreshes (and writes) the index."""
    for name, text in files:
        (work / name).write_text(text)
    later = time.time() + 3600
    os.utime(work / 'same.md', (later, later))


def case_untrusted():
    work = repository('untrusted')
    (work / '.gitattributes').write_text('*.txt filter=evil\n*.dat filter=proc\n*.eq filter=a=b\n*.none filter=\n')
    for name, text in [('a.txt', 'one\n'), ('b.dat', 'bbb\n'), ('c.eq', 'ccc\n'), ('d.none', 'ddd\n'),
                       ('same.md', 'same\n')]:
        (work / name).write_text(text)
    git(work, 'add', '--all')
    git(work, 'commit', '-q', '-m', 'baseline')
    submodule(work, 'untrusted')
    programs(work, 'untrusted', [
        ('filter.evil.clean', mark('untrusted-clean', 'cat')), ('filter.evil.required', 'true'),
        ('filter.proc.process', mark('untrusted-process', 'exit 1')), ('filter.proc.required', 'true'),
        ('filter.a=b.clean', mark('untrusted-equals', 'cat'))])
    changed(work, [('a.txt', 'two\n'), ('b.dat', 'BBB\n'), ('c.eq', 'CCC\n'), ('d.none', 'DDD\n')])

    url, _ = serve(work, trusted=False)
    status, files = review(url, 'working')
    assert status == 200, files
    listed = sorted(file['file'] for file in files)
    assert listed == ['a.txt', 'b.dat', 'c.eq', 'd.none'], listed
    patch = next(file for file in files if file['file'] == 'a.txt')['patch']
    assert '-one' in patch and '+two' in patch, patch
    assert left() == [], f'programs the repository names ran: {left()}'
    passed('untrusted: Review lists the changes and runs no filter, process filter, monitor, hook or submodule Git')


def case_trusted():
    work = repository('trusted')
    (work / '.gitattributes').write_text('*.txt filter=evil\n*.none filter=\n')
    for name, text in [('a.txt', 'one\n'), ('d.none', 'ddd\n'), ('same.md', 'same\n')]:
        (work / name).write_text(text)
    git(work, 'add', '--all')
    git(work, 'commit', '-q', '-m', 'baseline')
    submodule(work, 'trusted')
    programs(work, 'trusted', [('filter.evil.clean', mark('trusted-clean', 'cat')), ('filter.evil.required', 'true')])
    changed(work, [('a.txt', 'two\n'), ('d.none', 'DDD\n')])

    url, _ = serve(work, trusted=True)
    status, files = review(url, 'working')
    assert status == 200, files
    assert sorted(file['file'] for file in files) == ['a.txt', 'd.none', 'module'], files
    ran = [name for name in left() if name.startswith('trusted-')]
    assert ran == ['trusted-clean', 'trusted-empty', 'trusted-submodule'], ran
    passed('trusted: Review reads as git diff does (filters and the submodule\'s Git run), never the monitor or a hook')


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

    # A German locale: the message is still recognised, since Git speaks English to the review.
    url, process = serve(clone, trusted=False, LC_ALL='de_DE.UTF-8', LANGUAGE='de')
    status, files = review(url, 'working')
    assert status == 200 and files == [], files
    status, failure = review(url, 'branch')
    assert 'fetch' not in left(), f'the review fetched with the remote\'s command: {left()} {failure}'
    assert status >= 400, (status, failure)
    assert failure['message'].endswith(
        'this clone has not downloaded the files the review needs, and the review does not download them while '
        'project configuration is not trusted; run git backfill (Git 2.49 or later), then try again'), failure
    assert 'promisor' not in failure['message'], failure
    passed('untrusted: a partial clone\'s branch review fetches nothing and says to run git backfill, in any locale')
    stop(process)
    url, _ = serve(clone, trusted=True)
    status, files = review(url, 'branch')
    assert status == 200 and [file['file'] for file in files] == ['file.txt'], (status, files)
    assert 'fetch' in left(), left()
    passed('trusted: a partial clone\'s branch review fetches its base as git diff does')


try:
    case_untrusted()
    case_trusted()
    case_partial_clone()
    (root / 'results.json').write_text(json.dumps(checks, indent=2))
finally:
    for process, log in servers:
        if process.poll() is None:
            process.terminate()
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=10)
        log.close()
    if holder:
        holder.cleanup()
