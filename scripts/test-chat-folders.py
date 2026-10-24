#!/usr/bin/env python3
"""Chats with a folder of their own, end to end against a real `--chat-home` server.

The chat home is the folder Desktop keeps the folders of chats without a project in. This starts
it the way Desktop does (`haarness --workspace "<folder>" --chat-home desktop`, `connect`), creates
two chats with folders, and shows that each chat's shell runs in its folder, writes land there, a
write into the other chat's folder asks, Undo takes a write back in that folder alone, a terminal
opens there, and the chat home appears in no project list (`desktop projects`, `worktree list`)
and never takes the pre-workspace database. Echo provider, isolated HAARNESS_HOME and
HAARNESS_DATA_DIR; stdlib only.
"""
import json, os, pathlib, sqlite3, subprocess, sys, tempfile, time, urllib.error, urllib.request, uuid
import e2e_scratch

root = pathlib.Path(tempfile.mkdtemp(prefix='haarness-chat-folders-')).resolve()
data = root / 'data'
home = root / 'app' / 'Chat folders'
project = root / 'project'
for folder in (home, project, data / 'config'):
    folder.mkdir(parents=True)
(data / 'config/config.toml').write_text('[defaults]\nprovider="echo"\nmodel="echo-1"\n')
binary = str(pathlib.Path(os.environ.get('HAARNESS_BIN', 'target/debug/haarness')).resolve())
env = {k: v for k, v in os.environ.items() if not k.startswith('HAARNESS_')}
env.update(
    HAARNESS_HOME=str(root / 'home'),
    HAARNESS_DATA_DIR=str(data),
    HAARNESS_TEST_PROVIDER='1',
)
connection = None
started_home = False
checks = 0


def cli(*args, ws, input=None):
    return subprocess.run(
        [binary, '--workspace', str(ws), *args],
        input=input, env=env, text=True, capture_output=True, check=True, timeout=90,
    ).stdout


def api(method, path, body=None, expect=None):
    request = urllib.request.Request(
        connection['url'] + '/v1' + path,
        method=method,
        data=None if body is None else json.dumps(body).encode(),
        headers={'Authorization': 'Bearer ' + connection['token'], 'Content-Type': 'application/json'},
    )
    try:
        with urllib.request.urlopen(request, timeout=15) as reply:
            raw = reply.read()
            status = reply.status
    except urllib.error.HTTPError as error:
        raw, status = error.read(), error.code
    if expect is not None:
        assert status == expect, (method, path, status, raw)
    elif status >= 400:
        raise AssertionError((method, path, status, raw))
    return json.loads(raw) if raw else None


def until(fn, label, timeout=30):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        value = fn()
        if value:
            return value
        time.sleep(0.1)
    raise AssertionError('timed out waiting for ' + label)


def passed(label):
    global checks
    checks += 1
    print('PASS ' + label, flush=True)


def turn(thread, text):
    """Starts a turn of `thread` with an echo directive."""
    return api('POST', f'/threads/{thread["id"]}/turns', {'requestId': str(uuid.uuid4()), 'text': text})


def finish(started):
    done = until(
        lambda: (value if (value := api('GET', '/turns/' + started['id']))['status'] not in ('running', 'queued') else None),
        'the turn to end',
    )
    assert done['status'] == 'completed', done


def tool(thread, name, arguments):
    finish(turn(thread, f'!tool {name} ' + json.dumps(arguments)))


def last_result(thread, name):
    items = api('GET', f'/threads/{thread["id"]}/items')
    results = [item['data'] for item in items if item['data']['type'] == 'tool_result' and item['data']['name'] == name]
    assert results, items
    return results[-1]


def catalog_paths(text):
    listing = json.loads(text)
    return (
        {workspace['path'] for workspace in listing['workspaces']},
        {project['root'] for project in listing['projects']},
        listing,
    )


try:
    # A database from before workspaces; the chat home must never take it.
    legacy = data / 'data' / 'haarness.db'
    legacy.parent.mkdir(parents=True, exist_ok=True)
    with sqlite3.connect(legacy) as database:
        database.execute('CREATE TABLE legacy_marker(value TEXT)')
        database.execute("INSERT INTO legacy_marker VALUES ('kept')")
    database.close()

    # `--chat-home` names its folder.
    refused = subprocess.run([binary, '--chat-home', 'desktop'], input='{"action":"connect"}', env=env,
                             text=True, capture_output=True, timeout=30, cwd=root)
    assert refused.returncode != 0 and '--workspace' in refused.stderr, refused
    passed('--chat-home without --workspace is refused')

    connection = json.loads(cli('--chat-home', 'desktop', ws=home, input='{"action":"connect"}'))
    started_home = True
    info = api('GET', '/info')
    assert 'thread-directory.v1' in info['capabilities'], info['capabilities']
    assert info['projectTrusted'] is False, info
    assert legacy.exists(), 'the chat home took the legacy database'
    passed('the chat home starts like Desktop starts it, without project configuration or the legacy database')

    canonical_home = home.resolve()
    first = api('POST', '/threads', {'title': 'First', 'directory': '2026-10-24/first-chat'})
    second = api('POST', '/threads', {'title': 'Second', 'directory': '2026-10-24/second-chat'})
    again = api('POST', '/threads', {'title': 'Again', 'directory': '2026-10-24/first-chat'})
    assert first['directory'] == str(canonical_home / '2026-10-24/first-chat'), first
    assert second['directory'] == str(canonical_home / '2026-10-24/second-chat'), second
    assert again['directory'] == str(canonical_home / '2026-10-24/first-chat-2'), again
    for thread in (first, second, again):
        assert pathlib.Path(thread['directory']).is_dir(), thread
    refusal = api('POST', '/threads', {'directory': '../outside'}, expect=400)
    assert refusal['code'] == 'invalid_request', refusal
    assert not (root / 'app' / 'outside').exists()
    listed = {thread['id']: thread.get('directory') for thread in api('GET', '/threads')}
    assert listed[first['id']] == first['directory'] and listed[second['id']] == second['directory'], listed
    passed('each chat gets its own folder, a taken one is numbered, one outside the chat home is refused')

    for thread in (first, second):
        tool(thread, 'bash', {'command': 'pwd -P'})
        output = last_result(thread, 'bash')['output']
        assert output == f'exit: 0\n{thread["directory"]}\n', output
    passed('!tool bash pwd runs in each chat\'s own folder')

    tool(first, 'write', {'path': 'hello.txt', 'content': 'from the first chat\n'})
    tool(second, 'write', {'path': 'hello.txt', 'content': 'from the second chat\n'})
    assert (pathlib.Path(first['directory']) / 'hello.txt').read_text() == 'from the first chat\n'
    assert (pathlib.Path(second['directory']) / 'hello.txt').read_text() == 'from the second chat\n'
    assert not (home / 'hello.txt').exists()
    passed('a write lands in the chat\'s folder')

    diff = api('GET', f'/threads/{first["id"]}/diff')
    assert [file['file'] for file in diff] == ['hello.txt'], diff
    reverted = api('POST', f'/threads/{first["id"]}/revert', {})
    assert reverted.get('revert'), reverted
    assert not (pathlib.Path(first['directory']) / 'hello.txt').exists(), 'undo left the write'
    assert (pathlib.Path(second['directory']) / 'hello.txt').read_text() == 'from the second chat\n'
    api('POST', f'/threads/{first["id"]}/unrevert', {})
    assert (pathlib.Path(first['directory']) / 'hello.txt').read_text() == 'from the first chat\n'
    passed('Undo takes the write back in that chat\'s folder alone, and Redo brings it back')

    intrusion = pathlib.Path(second['directory']) / 'intrusion.txt'
    started = turn(first, '!tool write ' + json.dumps({'path': '../second-chat/intrusion.txt', 'content': 'no\n'}))
    pending = until(lambda: [p for p in api('GET', '/permissions') if p['threadId'] == first['id']], 'the ask')
    assert pending[0]['permission'] == 'external_directory', pending
    assert any(second['directory'] in pattern for pattern in pending[0]['patterns']), pending
    api('POST', f'/permissions/{pending[0]["permissionId"]}', {'decision': 'reject', 'feedback': None})
    finish(started)
    assert not intrusion.exists()
    passed('a write into the other chat\'s folder asks, and a no writes nothing')

    terminal = api('POST', '/ptys', {'directory': second['directory'], 'cols': 80, 'rows': 24, 'ifNone': True})
    assert terminal['cwd'] == second['directory'] and terminal['directory'] == second['directory'], terminal
    same = api('POST', '/ptys', {'directory': second['directory'], 'cols': 80, 'rows': 24, 'ifNone': True})
    assert same['id'] == terminal['id'] and same['created'] is False, same
    other = api('POST', '/ptys', {'directory': '2026-10-24/first-chat', 'cols': 80, 'rows': 24, 'ifNone': True})
    assert other['id'] != terminal['id'] and other['cwd'] == first['directory'], other
    for opened in (terminal, other):
        api('DELETE', f'/ptys/{opened["id"]}')
    passed('a chat\'s terminal opens in its folder and is that chat\'s alone')

    # A project beside it: the catalogs list the project, never the chat home.
    for args in (['init', '-q', '-b', 'main'],):
        subprocess.run(['git', '-C', str(project), *args], check=True, capture_output=True)
    for ws in (project, home):
        paths, roots, listing = catalog_paths(cli('desktop', ws=ws, input=json.dumps(
            {'action': 'projects', 'directories': [str(project), str(home)]})))
        assert str(canonical_home) not in paths and str(canonical_home) not in roots, listing
        assert str(project.resolve()) in paths, listing
        assert not any(workspace.get('chatHome') for workspace in listing['workspaces']), listing
    paths, roots, listing = catalog_paths(cli('worktree', 'list', '--json', ws=project))
    assert str(canonical_home) not in paths and str(canonical_home) not in roots, listing
    assert str(project.resolve()) in paths, listing
    text = cli('worktree', 'list', ws=home)
    assert str(canonical_home) not in text and str(project.resolve()) in text, text
    passed('the chat home is in neither desktop projects nor worktree list')

    # Registered again without the flag, it stays the chat home.
    registry = json.loads((data / 'data' / 'workspaces.json').read_text())
    entry = [workspace for workspace in registry['workspaces'] if workspace['path'] == str(canonical_home)]
    assert len(entry) == 1 and entry[0].get('chatHome') is True, registry
    # The first project took the pre-workspace database instead.
    assert not legacy.exists(), 'the first project did not take the legacy database'
    passed('the chat home stays marked, and the first project takes the legacy database')
finally:
    if started_home:
        subprocess.run([binary, '--workspace', str(home), 'serve', '--stop'], env=env,
                       capture_output=True, timeout=35)
    e2e_scratch.settle(root)

print(f'{checks} checks passed')
