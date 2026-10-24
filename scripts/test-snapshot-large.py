#!/usr/bin/env python3
"""A large project's snapshots never lock other chats out of the session database.

Drives a real, isolated workspace daemon on a Git project of about 30,000 files with the echo
provider. Chat A edits a file and then adds one (`!tool write`), so its turns capture the whole
tree before and after each write, the first time storing every file. Meanwhile chat B runs one
short echo turn after another, each writing its items to the same database. Then chat A undoes
both writes.

Checks that no turn of chat B fails or waits on chat A's captures (the slowest one is reported),
and that undo puts the tree back byte for byte. Before captures were written in short batches,
one capture held the database's write lock for as long as it took to read and compress every
file: B's writes waited the five-second busy timeout and failed with "database is locked".

Prints a `TIMINGS {...}` line to compare binaries (`HAARNESS_BIN`). `HAARNESS_SNAPSHOT_FILES`
changes the number of files. Echo provider, isolated HOME, HAARNESS_HOME and HAARNESS_DATA_DIR;
stdlib only.
"""
import hashlib
import json
import os
from pathlib import Path
import random
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request
import uuid

import e2e_scratch

REPO = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get('HAARNESS_BIN', REPO / 'target/debug/haarness')).resolve()
FILES = int(os.environ.get('HAARNESS_SNAPSHOT_FILES', '30000'))

root = Path(tempfile.mkdtemp(prefix='haarness-snapshot-large-')).resolve()
home, project = root / 'home', root / 'project'
home.mkdir()
(project / '.haarness').mkdir(parents=True)
(project / '.haarness/config.toml').write_text(
    '[defaults]\nprovider="echo"\nmodel="echo-1"\n'
    '[permissions]\nread="allow"\nwrite="allow"\nedit="allow"\n[lsp]\nenabled=false\n')
env = {'PATH': '/usr/bin:/bin:/usr/sbin:/sbin', 'HOME': str(home), 'HAARNESS_HOME': str(home),
       'HAARNESS_DATA_DIR': str(root / 'data'), 'USER': os.environ.get('USER', 'fixture'),
       'LANG': 'en_US.UTF-8', 'HAARNESS_TEST_PROVIDER': '1'}
args = [str(BINARY), '--workspace', str(project), '--trust-project']
connection = {}
started = False
checks = 0


def passed(label):
    global checks
    checks += 1
    print('PASS ' + label, flush=True)


def api(method, path, body=None, timeout=60):
    request = urllib.request.Request(
        connection['url'] + '/v1' + path, method=method,
        data=None if body is None else json.dumps(body).encode(),
        headers={'Authorization': 'Bearer ' + connection['token'], 'Content-Type': 'application/json'})
    try:
        with urllib.request.urlopen(request, timeout=timeout) as reply:
            raw = reply.read()
    except urllib.error.HTTPError as error:
        raise AssertionError((method, path, error.code, error.read().decode(errors='replace'))) from None
    return json.loads(raw) if raw else None


def turn(thread, text, timeout=300):
    """Runs one turn of `thread` to its end: its final state and how long it took."""
    start = time.monotonic()
    begun = api('POST', f'/threads/{thread["id"]}/turns', {'requestId': str(uuid.uuid4()), 'text': text})
    while True:
        done = api('GET', '/turns/' + begun['id'])
        if done['status'] not in ('running', 'queued'):
            return done, time.monotonic() - start
        if time.monotonic() - start > timeout:
            raise AssertionError(f'turn {begun["id"]} still {done["status"]} after {timeout}s')
        time.sleep(0.05)


def tree():
    """Every file of the project but Git's, with its mode and content."""
    digest = hashlib.sha256()
    count = 0
    for directory, names, files in os.walk(project):
        names[:] = sorted(name for name in names if name != '.git')
        for name in sorted(files):
            path = Path(directory) / name
            digest.update(str(path.relative_to(project)).encode() + b'\0')
            digest.update(oct(path.lstat().st_mode).encode() + b'\0')
            digest.update(path.read_bytes() + b'\0')
            count += 1
    return digest.hexdigest(), count


def write_project():
    """About 6 KB of source-like text per file, a hundred files per folder."""
    rng = random.Random(1024)
    words = [''.join(rng.choice('abcdefghijklmnopqrstuvwxyz_') for _ in range(rng.randint(2, 12)))
             for _ in range(600)]
    for index in range(FILES):
        folder = project / 'src' / f'd{index // 100:03d}'
        if index % 100 == 0:
            folder.mkdir(parents=True)
        lines = (' '.join(rng.choices(words, k=rng.randint(3, 14))) for _ in range(rng.randint(20, 160)))
        (folder / f'f{index:05d}.txt').write_text(f'// file {index}\n' + ';\n'.join(lines) + '\n')
    git = ['git', '-C', str(project), '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid']
    for command in (['init', '-q', '-b', 'main'], ['add', '-A'], ['commit', '-q', '-m', 'fixture']):
        subprocess.run(git + command, check=True, capture_output=True, env=env)


class Chatter(threading.Thread):
    """Chat B: one echo turn after another until stopped."""

    def __init__(self, thread):
        super().__init__(daemon=True)
        self.thread = thread
        self.stopping = threading.Event()
        self.turns = 0
        self.slowest = 0.0
        self.errors = []

    def run(self):
        while not self.stopping.is_set():
            try:
                done, took = turn(self.thread, f'ping {self.turns}', timeout=60)
                if done['status'] != 'completed':
                    self.errors.append(f'turn {self.turns}: {done["status"]} {done.get("error")}')
                self.slowest = max(self.slowest, took)
            except Exception as error:  # every failure is a finding
                self.errors.append(f'turn {self.turns}: {error}')
            self.turns += 1
            time.sleep(0.05)


timings = {'binary': str(BINARY), 'files': FILES}
try:
    began = time.monotonic()
    write_project()
    original, count = tree()
    timings['fixture_s'] = round(time.monotonic() - began, 1)
    print(f'fixture: {count} files in {timings["fixture_s"]}s', flush=True)

    connection.update(json.loads(subprocess.run(
        args + ['desktop'], input='{"action":"connect"}', env=env, check=True, capture_output=True,
        text=True, timeout=120).stdout))
    started = True
    first = api('POST', '/threads', {'title': 'Edits'})
    second = api('POST', '/threads', {'title': 'Chatter'})
    done, _ = turn(second, 'hello')
    assert done['status'] == 'completed', done

    chatter = Chatter(second)
    chatter.start()
    time.sleep(1)
    target = project / 'src/d000/f00000.txt'
    for label, arguments in (
        ('edit', {'path': 'src/d000/f00000.txt', 'content': 'edited by chat A\n'}),
        ('add', {'path': 'notes/new.txt', 'content': 'added by chat A\n'}),
    ):
        done, took = turn(first, '!tool write ' + json.dumps(arguments))
        assert done['status'] == 'completed', (label, done)
        timings[f'{label}_turn_s'] = round(took, 2)
        print(f'chat A {label}: {took:.2f}s while chat B ran {chatter.turns} turns', flush=True)
    assert target.read_text() == 'edited by chat A\n'
    assert (project / 'notes/new.txt').read_text() == 'added by chat A\n'
    passed('chat A writes into a large project while chat B keeps running turns')

    for step in ('undo_add', 'undo_edit'):
        start = time.monotonic()
        reverted = api('POST', f'/threads/{first["id"]}/revert', {}, timeout=300)
        assert reverted.get('revert'), reverted
        timings[f'{step}_s'] = round(time.monotonic() - start, 2)
    chatter.stopping.set()
    chatter.join(timeout=120)
    timings.update(chatter_turns=chatter.turns, chatter_slowest_s=round(chatter.slowest, 2),
                   chatter_errors=len(chatter.errors))
    print('TIMINGS ' + json.dumps(timings), flush=True)

    assert not (project / 'notes/new.txt').exists(), 'undo left the added file'
    assert tree() == (original, count), 'undo did not restore the project byte for byte'
    passed('undo restores the whole project byte for byte')

    assert chatter.turns >= 5, chatter.turns
    assert not chatter.errors, chatter.errors[:5]
    passed(f'chat B ran {chatter.turns} turns without one failing (slowest {chatter.slowest:.2f}s)')
    print(f'{checks} checks passed', flush=True)
finally:
    if started:
        subprocess.run(args + ['serve', '--stop'], env=env, capture_output=True, timeout=60)
    e2e_scratch.settle(root)
