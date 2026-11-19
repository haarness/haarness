#!/usr/bin/env python3
"""Real TUI in tmux, echo provider, chats in Review: each chat's own temp folder.

The run's chats keep their folders under the run's own HAARNESS_TEST_CHAT_TEMP (see e2e_env), which this
debug build reads in place of /tmp/haarness-<uid>; nothing goes in that shared one.

1. Its start took away a folder recorded for a chat no store here knows, in which nothing changed for a
   month, and kept one used lately.
2. The model's command finds the chat's own folder in TMPDIR, TMP and TEMP: in `<that folder>/chats`, named by
   the end of the chat's id, for this user alone and recorded for that chat, while the server's own TMPDIR is
   elsewhere. It writes and reads a file there, and Python's multiprocessing makes its socket there, below
   macOS's 104-byte limit, with no approval card.
3. A file tool writes there and the person's own `!` command reads it back, with no card.
4. Another chat's command that reads the first chat's folder shows a card (`external_directory`); refused,
   nothing is read.
5. /archive, and archiving the first chat, keep both folders (an archived chat can come back). Once nothing
   changed in them for a week, the server's next start takes them and their records away.

Usage: HAARNESS_BIN=target/debug/haarness python3 scripts/test-chat-temp-tmux.py
Set HAARNESS_E2E_DIR to keep the run's state under a chosen folder.
"""
import json, os, pathlib, stat, subprocess, tempfile, time, urllib.request, uuid
import e2e_env, e2e_scratch, e2e_tmux

binary = pathlib.Path(os.environ.get('HAARNESS_BIN', 'target/debug/haarness')).resolve()
root = pathlib.Path(tempfile.mkdtemp(prefix='haarness-chat-temp-', dir=os.environ.get('HAARNESS_E2E_DIR'))).resolve()
project = root/'project'; state = root/'data'; home = root/'home'; server_temp = root/'tmp'
for folder in (project, state/'config', home, server_temp):
    folder.mkdir(parents=True, exist_ok=True)
(state/'config'/'haarness.json').write_text(json.dumps({'defaults': {'approval_mode': 'review'}}))
session = 'haarness-chat-temp-' + uuid.uuid4().hex[:8]
checks = []
chats = pathlib.Path(os.path.realpath(e2e_env.CHAT_TEMP))/'chats'
shared = pathlib.Path(os.path.realpath('/tmp'))/f'haarness-{os.geteuid()}'/'chats'
MONTH = time.time() - 30 * 24 * 3600
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
    try:
        pane = capture()
    except subprocess.CalledProcessError:
        pane = '(no pane)'
    raise AssertionError(f'{label}\n{pane}')
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
def name(chat): return chat.replace('-', '')[-8:]
def record(folder): return folder.parent/(folder.name + '.chat')
def recorded(chat):
    """A chat's folder and record, as a chat of a server long gone left them."""
    folder = chats/name(chat)
    folder.mkdir(mode=0o700)
    (folder/'left.txt').write_text('left\n')
    record(folder).write_text(chat)
    return folder
def age(folder):
    """Nothing in `folder`, nor its record, changed for a month."""
    for place, folders, files in os.walk(folder):
        for entry in folders + files:
            os.utime(os.path.join(place, entry), (MONTH, MONTH), follow_symlinks=False)
    for path in (folder, record(folder)):
        os.utime(path, (MONTH, MONTH), follow_symlinks=False)
def gone(folder): return not folder.exists() and not record(folder).exists()
env = {**os.environ, 'HAARNESS_DATA_DIR': str(state), 'HAARNESS_HOME': str(home), 'HOME': str(home),
       'TMPDIR': str(server_temp), 'HAARNESS_TEST_PROVIDER': '1', 'HAARNESS_DEFAULT_PROVIDER': 'echo',
       'HAARNESS_DEFAULT_MODEL': 'echo-1', 'HAARNESS_NO_CONNECT_PROMPT': '1'}
assert env['HAARNESS_TEST_CHAT_TEMP'] == e2e_env.CHAT_TEMP

try:
    chats.mkdir(mode=0o700)
    orphan, lately = recorded(str(uuid.uuid4())), recorded(str(uuid.uuid4()))
    age(orphan)

    command = e2e_tmux.pane_command(env, [binary, '--workspace', project, '--trust-project'])
    run(['tmux', 'new-session', '-d', '-x', '200', '-y', '50', '-c', str(root), '-s', session, command])
    until(lambda: 'echo' in capture().lower(), 'TUI did not open')
    call = until(connect, 'the server did not start')
    until(lambda: gone(orphan), "the server's start left a month-old folder of a chat no store here knows")
    assert read(lately/'left.txt') == 'left\n'
    check("the server's start took a month-old folder of an unknown chat away and kept one used lately")

    line = ('printf %s "$TMPDIR" > tmpdir.txt && printf "%s|%s" "$TMP" "$TEMP" > vars.txt'
            ' && echo made > "$TMPDIR/out.txt" && cat "$TMPDIR/out.txt" > copy.txt && ' + SOCKET + ' > socket.txt')
    paste('!tool bash ' + json.dumps({'command': line}))
    unasked(call, lambda: read(project/'socket.txt'), 'the command did not finish')
    first = root_chats(call)
    assert len(first) == 1, first
    first = first[0]
    folder = pathlib.Path(read(project/'tmpdir.txt'))
    assert folder == chats/name(first), (folder, first)
    assert read(project/'vars.txt') == f'{folder}|{folder}'
    assert stat.S_IMODE(folder.stat().st_mode) == 0o700 and stat.S_IMODE(chats.stat().st_mode) == 0o700
    assert read(record(folder)) == first
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

    other = chats/name(second)
    assert other.is_dir(), other
    typed('/archive')
    until(lambda: any(thread['id'] == second and thread.get('archived') for thread in call('/threads?archived=true')),
          '/archive did not archive')
    call(f'/threads/{first}', {'archived': True}, method='PATCH')
    time.sleep(1)
    assert read(folder/'out.txt') == 'made\n' and other.is_dir() and record(folder).exists() and record(other).exists()
    check("archiving each chat keeps its folder, for when it comes back")

    # A week on, with nothing used: the next start takes both away.
    for archived in (folder, other):
        age(archived)
    subprocess.run(['tmux', 'kill-session', '-t', session], capture_output=True)
    e2e_scratch.stop_server(binary, project, env)
    run([binary, '--workspace', project, '--trust-project', 'serve', '--daemon'], env=env)
    until(lambda: gone(folder) and gone(other), "the next start left the archived chats' folders")
    assert read(lately/'left.txt') == 'left\n'
    check("once unused for a week, the next start takes the archived chats' folders and records away")

    assert not any((shared/(name(chat) + '.chat')).exists() for chat in (first, second)), shared
    check(f'nothing went in the shared {shared}')
    print(f'PASS {len(checks)} chat temp folder scenarios.', flush=True)
finally:
    subprocess.run(['tmux', 'kill-session', '-t', session], capture_output=True)
    e2e_scratch.stop_server(binary, project, env)
    e2e_scratch.settle(root)
