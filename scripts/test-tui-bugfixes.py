#!/usr/bin/env python3
"""Real tmux regressions for TUI drafts, input integrity, shortcuts and selectors."""
import concurrent.futures
import json
import os
from pathlib import Path
import shlex
import subprocess
import tempfile
import time
import urllib.request

repo = Path(__file__).resolve().parents[1]
binary = str(Path(os.environ.get('HAARNESS_BIN', repo / 'target/debug/haarness')).resolve())
root = Path(tempfile.mkdtemp(prefix='haarness-tui-regressions-')).resolve()
project = root / 'project'
project.mkdir()
data = root / 'state'
(data / 'config').mkdir(parents=True)
(data / 'config/config.toml').write_text('[defaults]\nprovider="echo"\nmodel="echo-1"\n[permissions]\nbash="ask"\n')
env = {**os.environ, 'HAARNESS_DATA_DIR': str(data), 'HAARNESS_TEST_PROVIDER': '1'}
args = [binary, '--workspace', str(project)]
socket = 'haarness-regressions-' + str(os.getpid())
session = 'main'
checks = []
endpoint = None


def run(*args, **kwargs):
    return subprocess.check_output(args, text=True, stderr=subprocess.PIPE, **kwargs)


def tmux(*args, **kwargs):
    return run('tmux', '-L', socket, *args, **kwargs)


def api(path, body=None):
    request = urllib.request.Request(endpoint['url'] + '/v1' + path,
        data=None if body is None else json.dumps(body).encode(),
        headers={'Authorization': 'Bearer ' + endpoint['token'], 'Content-Type': 'application/json'})
    with urllib.request.urlopen(request, timeout=10) as response:
        content = response.read()
        return json.loads(content) if content else None


def until(check, label):
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(.08)
    raise AssertionError(label)


def pane():
    return tmux('capture-pane', '-p', '-t', session)


def keys(*values):
    tmux('send-keys', '-t', session, *values)
    time.sleep(.12)


def text(value):
    tmux('send-keys', '-l', '-t', session, '--', value)
    time.sleep(.12)


def paste(value):
    tmux('load-buffer', '-b', 'fixture', '-', input=value)
    tmux('paste-buffer', '-r', '-p', '-b', 'fixture', '-t', session)
    time.sleep(.2)


def launch(thread):
    subprocess.run(['tmux', '-L', socket, 'kill-session', '-t', session], capture_output=True)
    command = shlex.join(['env', 'HAARNESS_DATA_DIR=' + str(data), 'HAARNESS_TEST_PROVIDER=1', *args, '--thread', thread])
    tmux('new-session', '-d', '-x', '120', '-y', '36', '-s', session, command)
    until(lambda: 'h a a r n e s s' in pane(), 'TUI did not launch')


def new(title):
    thread = api('/threads', {'title': title, 'provider': 'echo', 'model': 'echo-1'})['id']
    launch(thread)
    return thread


def submit(thread):
    before = len(api('/threads/' + thread + '/items'))
    keys('Enter')
    records = until(lambda: [i for i in api('/threads/' + thread + '/items')[before:] if i['data']['type'] == 'user_message'], 'message was not submitted')
    until(lambda: not api('/turns/active'), 'turn remained active')
    return records[0]['data']['text']


def check(name, success):
    (root / (name + '.txt')).write_text(pane())
    checks.append({'name': name, 'passed': bool(success)})
    (root / 'results.json').write_text(json.dumps(checks, indent=2))
    assert success, name
    print('PASS', name, flush=True)


def quit_client(target):
    tmux('send-keys', '-t', target, 'C-p')
    time.sleep(.15)
    tmux('send-keys', '-l', '-t', target, 'quit')
    tmux('send-keys', '-t', target, 'Enter')
    until(lambda: subprocess.run(['tmux', '-L', socket, 'has-session', '-t', target], capture_output=True).returncode != 0, 'client did not quit')


print('Evidence:', root, flush=True)
try:
    run('git', 'init', '-q', str(project))
    endpoint = json.loads(run(*args, 'desktop', env=env, input='{"action":"connect"}', timeout=90))
    payload = 'line\n' * 12
    for name, edits in [('paste-backspace', ['BSpace']), ('paste-delete', ['Home', 'Delete']), ('paste-word-delete', ['C-w'])]:
        thread = new(name)
        paste(payload)
        keys(*edits)
        text('after')
        check(name, submit(thread) == 'after')
    thread = new('literal-paste-markers')
    first = 'FIRST [#2 pasted 1 lines] ' + 'a' * 1100
    second = 'b' * 1100
    paste(first)
    text(' | ')
    paste(second)
    check('literal-paste-markers', submit(thread) == first + ' | ' + second)
    for index, value in enumerate(['e\u0301', '👍🏽', '🇯🇵', '👨‍👩‍👧‍👦']):
        thread = new('unicode-' + str(index))
        paste('A' + value)
        keys('BSpace')
        check('unicode-' + str(index), submit(thread) == 'A')
    thread = new('model-shortcut')
    paste('DRAFT-F2')
    before = len(api('/threads/' + thread + '/items'))
    keys('F2')
    until(lambda: 'Select model' in pane(), 'F2 did not open models')
    check('f2-preserves-unsent-draft', len(api('/threads/' + thread + '/items')) == before and 'DRAFT-F2' in pane())
    keys('Escape')
    check('enter-still-submits', submit(thread) == 'DRAFT-F2')
    thread = new('normal-draft-mode')
    text('printf THIS-IS-A-CHAT-MESSAGE')
    keys('M-n')
    text('!')
    text('printf SHELL-DRAFT')
    other = next(t for t in api('/threads') if t['id'] != thread and t['title'] == 'New thread')['id']
    keys('C-l')
    text('normal-draft-mode')
    keys('Enter')
    until(lambda: 'THIS-IS-A-CHAT-MESSAGE' in pane(), 'normal draft did not restore')
    check('normal-draft-mode', submit(thread) == 'printf THIS-IS-A-CHAT-MESSAGE')
    keys('C-l')
    text(other[-8:])
    keys('Enter')
    until(lambda: 'SHELL-DRAFT' in pane(), 'shell draft did not restore')
    check('shell-draft-mode-restores', ' shell ' in pane().splitlines()[-1])
    keys('M-n')
    check('fresh-session-chat-mode', ' shell ' not in pane().splitlines()[-1])
    keys('C-l')
    text('FILTER-' + 'x' * 150 + '-TAIL')
    until(lambda: '-TAIL▌' in pane(), 'search suffix and caret')
    check('search-cursor-visible', '-TAIL▌' in pane())
    keys('Escape')
    # Both clients load before either saves. Exit concurrently to exercise the lock.
    session = 'a'
    a = new('parallel-a')
    paste('KEEP-A')
    session = 'b'
    b = new('parallel-b')
    paste('KEEP-B')
    with concurrent.futures.ThreadPoolExecutor(2) as pool:
        list(pool.map(quit_client, ['a', 'b']))
    session = 'a'
    launch(a)
    check('concurrent-save-retains-a', 'KEEP-A' in pane())
    quit_client('a')
    session = 'b'
    launch(b)
    check('concurrent-save-retains-b', 'KEEP-B' in pane())
    quit_client('b')
    old_url = endpoint['url']
    run(*args, 'serve', '--stop', env=env, timeout=30)
    endpoint = json.loads(run(*args, 'desktop', env=env, input='{"action":"connect"}', timeout=90))
    session = 'a'
    launch(a)
    check('daemon-restart-retains-draft', 'KEEP-A' in pane())
    quit_client('a')
    # Exercise migration from the legacy tuple format at a previous daemon URL.
    draft_file = data / 'config/tui-server-drafts.json'
    draft_file.write_text(json.dumps({old_url: {'thread': a, 'drafts': {a: ['LEGACY-NORMAL', []], b: ['LEGACY-SHELL', []]}, 'shell': False}}))
    launch(a)
    check('legacy-port-and-format-migration', 'LEGACY-NORMAL' in pane() and ' shell ' not in pane().splitlines()[-1])
    quit_client('a')
    (data / 'config/tui-state.toml').write_text('[keybindings]\nmodels="ctrl+m"\nsessions="F2"\n')
    launch(a)
    keys('F3')
    check('legacy-shortcut-migration-respects-custom-f2', 'Select model' in pane() and 'LEGACY-NORMAL' in pane())
    print('PASS:', len(checks), 'TUI bugfix regressions', flush=True)
finally:
    subprocess.run(['tmux', '-L', socket, 'kill-server'], capture_output=True)
    if endpoint:
        subprocess.run(args + ['serve', '--stop'], env=env, capture_output=True, timeout=30)
