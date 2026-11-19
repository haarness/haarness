#!/usr/bin/env python3
"""Real TUI in tmux, echo provider, chats in Review: each chat's own temp folder.

1. The model's command finds the chat's own folder in TMPDIR, TMP and TEMP: in /tmp/haarness-<uid>/chats,
   as the system spells /tmp, named by the end of the chat's id, for this user alone and recorded for
   that chat, while the server's own TMPDIR is elsewhere. It writes and reads a file there, and Python's
   multiprocessing makes its socket there, below macOS's 104-byte limit, with no approval card.
2. A file tool writes there and the person's own `!` command reads it back, with no card.
3. Another chat's command that reads the first chat's folder shows a card (`external_directory`);
   refused, nothing is read.
4. /archive takes the open chat's folder away, and archiving the first chat its folder and record.
5. Before all that, the server's start took away a folder recorded for a chat no store here knows, in
   which nothing changed for a month.

Usage: HAARNESS_BIN=target/debug/haarness python3 scripts/test-chat-temp-tmux.py
Set HAARNESS_E2E_DIR to keep the run's state under a chosen folder.
"""
import json, os, pathlib, stat, subprocess, tempfile, time, urllib.request, uuid
import e2e_scratch, e2e_tmux

binary = pathlib.Path(os.environ.get('HAARNESS_BIN', 'target/debug/haarness')).resolve()
root = pathlib.Path(tempfile.mkdtemp(prefix='haarness-chat-temp-', dir=os.environ.get('HAARNESS_E2E_DIR'))).resolve()
project = root/'project'; state = root/'data'; home = root/'home'; server_temp = root/'tmp'
for folder in (project, state/'config', home, server_temp):
    folder.mkdir(parents=True, exist_ok=True)
(state/'config'/'haarness.json').write_text(json.dumps({'defaults': {'approval_mode': 'review'}}))
session = 'haarness-chat-temp-' + uuid.uuid4().hex[:8]
checks = []
chats = pathlib.Path(os.path.realpath('/tmp'))/f'haarness-{os.geteuid()}'/'chats'
SOCKET = ("python3 -c \"from multiprocessing.connection import Listener; "
          "listener = Listener(family='AF_UNIX'); print(listener.address); listener.close()\"")

def run(args, **kwargs): return subprocess.run(args, check=True, text=True, capture_output=True, timeout=60, **kwargs).stdout
def capture(): return run(['tmux', 'capture-pane', '-p', '-J', '-t', session])
def keys(*values): run(['tmux', 'send-keys', '-t', session, *values])
def typed(text):
    run(['tmux', 'send-keys', '-l', '-t', session, text])
    until(lambda: text in capture(), 'typed text not shown: ' + text)
    keys('Enter')
def paste(text):
    """A prompt as a paste, so its leading `!` does not open shell mode."""
    run(['tmux', 'load-buffer', '-'], input=' ' + text)
    run(['tmux', 'paste-buffer', '-p', '-t', session]); keys('Enter')
def until(predicate, label, seconds=45):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        value = predicate()
        if value: return value
        time.sleep(.2)
    raise AssertionError(f'{label}\n{capture()}')
def read(path): return path.read_text() if path.exists() else None
def api():
    registry = json.loads((state/'data/workspaces.json').read_text())
    identity = next(item['id'] for item in registry['workspaces'] if item['path'] == str(project))
    runtime = json.loads((state/'cache/workspaces'/identity/'runtime.json').read_text())
    def call(path, body=None, method=None):
        request = urllib.request.Request(runtime['url'] + '/v1' + path, method=method,
            data=None if body is None else json.dumps(body).encode(),
            headers={'Authorization': 'Bearer ' + runtime['token'], 'Content-Type': 'application/json'})
        with urllib.request.urlopen(request, timeout=15) as reply: return json.loads(reply.read() or b'null')
    return call
def connect():
    try:
        call = api(); call('/threads'); return call
    except (OSError, ValueError, StopIteration, KeyError):
        return None
def check(label):
    checks.append(label); print('PASS ' + label, flush=True)
def unasked(call, done, label):
    """`done` comes true with no approval card shown or asked for on the way."""
    def finished():
        assert not call('/permissions'), (label, call('/permissions'))
        assert '⚠ ' not in capture(), (label, capture())
        return done()
    return until(finished, label)
def root_chats(call): return [thread['id'] for thread in call('/threads') if not thread.get('parentId')]
env = {**os.environ, 'HAARNESS_DATA_DIR': str(state), 'HAARNESS_HOME': str(home), 'HOME': str(home),
       'TMPDIR': str(server_temp), 'HAARNESS_TEST_PROVIDER': '1', 'HAARNESS_DEFAULT_PROVIDER': 'echo',
       'HAARNESS_DEFAULT_MODEL': 'echo-1', 'HAARNESS_NO_CONNECT_PROMPT': '1'}

try:
    # What a chat of a server long gone left: its folder and record, untouched for a month.
    for folder in (chats.parent, chats):
        folder.mkdir(mode=0o700, exist_ok=True)
    left = str(uuid.uuid4())
    orphan = chats/left.replace('-', '')[-8:]
    orphan.mkdir(mode=0o700)
    (orphan/'left.txt').write_text('left\n')
    (chats/(orphan.name + '.chat')).write_text(left)
    month = time.time() - 30 * 24 * 3600
    for path in (orphan/'left.txt', orphan, chats/(orphan.name + '.chat')):
        os.utime(path, (month, month))

    command = e2e_tmux.pane_command(env, [binary, '--workspace', project, '--trust-project'])
    run(['tmux', 'new-session', '-d', '-x', '200', '-y', '50', '-c', str(root), '-s', session, command])
    until(lambda: 'echo' in capture().lower(), 'TUI did not open')
    call = until(connect, 'the server did not start')

    line = ('printf %s "$TMPDIR" > tmpdir.txt && printf "%s|%s" "$TMP" "$TEMP" > vars.txt'
            ' && echo made > "$TMPDIR/out.txt" && cat "$TMPDIR/out.txt" > copy.txt && ' + SOCKET + ' > socket.txt')
    paste('!tool bash ' + json.dumps({'command': line}))
    unasked(call, lambda: read(project/'socket.txt'), 'the command did not finish')
    first = root_chats(call)
    assert len(first) == 1, first
    first = first[0]
    folder = pathlib.Path(read(project/'tmpdir.txt'))
    assert folder == chats/first.replace('-', '')[-8:], (folder, first)
    assert read(project/'vars.txt') == f'{folder}|{folder}'
    assert stat.S_IMODE(folder.stat().st_mode) == 0o700 and stat.S_IMODE(chats.stat().st_mode) == 0o700
    assert read(folder.parent/(folder.name + '.chat')) == first
    assert read(project/'copy.txt') == 'made\n' and read(folder/'out.txt') == 'made\n'
    assert not str(folder).startswith(str(server_temp))
    address = read(project/'socket.txt').strip()
    assert address.startswith(f'{folder}/pymp-') and len(address.encode()) < 104, address
    check(f'the command gets the chat\'s own folder ({folder}) as TMPDIR, TMP and TEMP, writes and reads there '
          f'and makes a {len(address.encode())}-byte socket there, with no card')

    notes = folder/'notes.md'
    paste('!tool write ' + json.dumps({'path': str(notes), 'content': 'temp notes\n'}))
    unasked(call, lambda: read(notes), 'the write did not finish')
    keys('!'); until(lambda: 'shell' in capture(), 'shell mode did not open')
    typed('cat "$TMPDIR/notes.md" > person.txt')
    unasked(call, lambda: read(project/'person.txt'), "the person's command did not run")
    assert read(project/'person.txt') == 'temp notes\n'
    check("a file tool writes in the folder and the person's own ! command reads it there, with no card")

    typed('/new')
    until(lambda: 'tmpdir.txt' not in capture(), 'the new chat did not open')
    paste('!tool bash ' + json.dumps({'command': f'cat {folder}/out.txt > stolen.txt'}))
    asked = until(lambda: call('/permissions'), "reading another chat's folder did not ask")
    assert asked[0]['permission'] == 'external_directory', asked
    until(lambda: '⚠ ' in capture(), 'no card was shown')
    second = asked[0]['threadId']
    assert second != first
    call(f"/permissions/{asked[0]['permissionId']}", {'decision': 'reject'})
    until(lambda: not call('/permissions'), 'the refusal did not close the card')
    assert not (project/'stolen.txt').exists()
    check("another chat's command that reads the first chat's folder asks, and refused reads nothing")

    other = chats/second.replace('-', '')[-8:]
    assert other.is_dir(), other
    typed('/archive')
    until(lambda: not other.exists() and not (chats/(other.name + '.chat')).exists(),
          "/archive left the chat's folder")
    call(f'/threads/{first}', {'archived': True}, method='PATCH')
    until(lambda: not folder.exists() and not (chats/(folder.name + '.chat')).exists(),
          "archiving left the first chat's folder")
    check("archiving each chat takes its own folder and record away")
    assert not orphan.exists() and not (chats/(orphan.name + '.chat')).exists(), orphan
    check("the server's start took away a month-old folder of a chat no store here knows")
    print(f'PASS {len(checks)} chat temp folder scenarios.', flush=True)
finally:
    subprocess.run(['tmux', 'kill-session', '-t', session], capture_output=True)
    e2e_scratch.stop_server(binary, project, env)
    e2e_scratch.settle(root)
