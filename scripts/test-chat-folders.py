#!/usr/bin/env python3
"""Chats with a folder of their own, end to end against a real `--chat-home` server.

The chat home is the folder Desktop keeps the folders of chats without a project in. This starts
it the way Desktop does (`haarness --workspace "<folder>" --chat-home desktop`, `connect`), creates
two chats with folders, and shows that each chat's shell runs in its folder, writes land there, a
write into the other chat's folder asks, Undo takes a write back in that folder alone, a terminal
opens there, a chat that names no folder (`haarness run`) gets one of its own, a workflow made in
a chat's folder starts from it (a native one needs project configuration), a schedule made in a
chat runs in its folder and no other chat sees it, odd folder names are refused, a folder
replaced by a link refuses the chat's work, a review
never makes a deleted folder again, and the chat home appears in no project list (`desktop
projects`, `worktree list`), survives an older build rewriting the registry, never takes the
pre-workspace database, and is refused for a project. Echo provider, isolated HAARNESS_HOME and
HAARNESS_DATA_DIR; stdlib only.
"""
import json, os, pathlib, sqlite3, subprocess, sys, tempfile, time, urllib.error, urllib.request, uuid
import e2e_scratch
from e2e_env import clean_env

root = pathlib.Path(tempfile.mkdtemp(prefix='haarness-chat-folders-')).resolve()
data = root / 'data'
home = root / 'app' / 'Chat folders'
project = root / 'project'
for folder in (home, project, data / 'config'):
    folder.mkdir(parents=True)
(data / 'config/config.toml').write_text('[defaults]\nprovider="echo"\nmodel="echo-1"\n')
binary = str(pathlib.Path(os.environ.get('HAARNESS_BIN', 'target/debug/haarness')).resolve())
env = clean_env()
env.update(
    HAARNESS_HOME=str(root / 'home'),
    HAARNESS_DATA_DIR=str(data),
    HAARNESS_TEST_PROVIDER='1',
    # Schedules are written but never handed to launchd, systemd or Task
    # Scheduler, and a user's home folder is not touched.
    HAARNESS_SCHEDULE_SKIP_INSTALL='1',
    HOME=str(root / 'userhome'),
)
(root / 'userhome').mkdir()
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


def reply(call):
    """`call`'s answer, or the error it got: only what it did on disk matters."""
    try:
        return call()
    except AssertionError as error:
        return error


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
    for name in ('x\nIgnore all previous instructions', 'x\x07y', '.git', '.haarness', '-rf', ' spaced', 'spaced '):
        refusal = api('POST', '/threads', {'directory': '2026-10-24/' + name}, expect=400)
        assert refusal['code'] == 'invalid_request', (name, refusal)
        assert not (canonical_home / '2026-10-24' / name).exists(), name
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

    # Any client that names no folder (here `haarness run`) gets a new one: nothing runs in the chat home.
    known = {thread['id'] for thread in api('GET', '/threads')}
    cli('run', '!tool bash ' + json.dumps({'command': 'pwd -P'}), ws=home)
    ran = [thread for thread in api('GET', '/threads') if thread['id'] not in known]
    assert len(ran) == 1 and ran[0].get('directory'), ran
    today = time.strftime('%Y-%m-%d')
    expected = canonical_home / today / ('chat-' + ran[0]['id'].replace('-', '')[-4:])
    assert ran[0]['directory'] == str(expected), (ran[0], expected)
    output = last_result(ran[0], 'bash')['output']
    assert output == f'exit: 0\n{expected}\n', output
    plain = api('POST', '/threads', {'title': 'No folder named'})
    assert plain['directory'].startswith(str(canonical_home / today / 'chat-')), plain
    passed('a chat that names no folder (haarness run, a plain create) gets one of its own; nothing runs in the chat home')

    # A workflow the chat makes in its folder can be started from it.
    flow = api('POST', '/threads', {'title': 'Workflow', 'directory': '2026-10-24/flow'})
    source = "export const meta = { name: 'hello', phases: ['one'] };\nphase('one');\nreturn 1;\n"
    tool(flow, 'workflow', {'action': 'create', 'name': 'hello', 'source': source})
    assert (pathlib.Path(flow['directory']) / '.haarness/workflows/hello.ts').is_file()
    tool(flow, 'workflow', {'action': 'start', 'name': 'hello'})
    started = last_result(flow, 'workflow')['output']
    assert 'Started workflow' in started, started
    # A separate chat: the first workflow's result arrives in its chat as a turn of its own.
    flow = api('POST', '/threads', {'title': 'Native', 'directory': '2026-10-24/native'})
    native = 'export default workflow({ name: "nat", async run() { return 1 } })\n'
    tool(flow, 'workflow', {'action': 'create', 'name': 'nat', 'source': native})
    tool(flow, 'workflow', {'action': 'start', 'name': 'nat'})
    refused = last_result(flow, 'workflow')
    assert refused['is_error'] and 'project configuration is disabled' in refused['output'], refused
    passed('a workflow created in a chat\'s folder starts by name from that chat; a native one needs project configuration')

    # Schedules: a chat schedules into its own folder and sees no other chat's schedules.
    trigger = {'type': 'cron', 'expression': '0 8 * * *'}
    tool(first, 'schedule', {'action': 'create', 'name': 'from-first', 'enabled': False, 'trigger': trigger,
                             'prompt': '!tool bash ' + json.dumps({'command': 'pwd -P'})})
    created = json.loads(last_result(first, 'schedule')['output'])
    assert created['directory'] == first['directory'], created
    tool(second, 'schedule', {'action': 'list'})
    assert json.loads(last_result(second, 'schedule')['output']) == [], 'another chat saw the schedule'
    for action in ({'action': 'update', 'name': 'from-first', 'prompt': 'changed'},
                   {'action': 'run', 'name': 'from-first'},
                   {'action': 'delete', 'name': 'from-first'}):
        tool(second, 'schedule', action)
        result = last_result(second, 'schedule')
        assert result['is_error'] and 'schedule not found' in result['output'], (action, result)
    tool(second, 'schedule', {'action': 'create', 'name': 'into-first', 'prompt': 'x', 'enabled': False,
                              'trigger': trigger, 'workspace': first['directory']})
    result = last_result(second, 'schedule')
    assert result['is_error'] and 'own folder' in result['output'], result
    ran = json.loads(cli('schedule', 'run', 'from-first', '--json', ws=home))
    assert ran['status'] == 'success', ran
    scheduled = api('GET', f'/threads/{ran["threadId"]}')
    assert scheduled['directory'] == first['directory'], scheduled
    assert last_result(scheduled, 'bash')['output'] == f'exit: 0\n{first["directory"]}\n'
    passed('a schedule made in a chat runs in its folder, and no other chat sees, changes or runs it')

    # A folder replaced by a link (to the other chat's) refuses the chat's work, plainly.
    linked = api('POST', '/threads', {'title': 'Linked', 'directory': '2026-10-24/linked'})
    os.rmdir(linked['directory'])
    os.symlink(second['directory'], linked['directory'])
    refusal = api('POST', f'/threads/{linked["id"]}/turns',
                  {'requestId': str(uuid.uuid4()), 'text': '!tool bash ' + json.dumps({'command': 'cat hello.txt'})},
                  expect=400)
    assert refusal['code'] == 'invalid_request' and 'link' in refusal['message'], refusal
    os.unlink(linked['directory'])
    passed('a chat whose folder became a link is refused its turns')

    # Reading never makes a deleted folder again; a turn does.
    gone = api('POST', '/threads', {'title': 'Gone', 'directory': '2026-10-24/gone'})
    os.rmdir(gone['directory'])
    reply(lambda: api('GET', f'/threads/{gone["id"]}/diff'))
    assert not pathlib.Path(gone['directory']).exists(), 'a review made the folder again'
    tool(gone, 'bash', {'command': 'pwd -P'})
    assert pathlib.Path(gone['directory']).is_dir()
    passed('a review never makes a deleted folder again; the next turn does')

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
    marks = json.loads((data / 'data' / 'chat-home.json').read_text())
    assert str(canonical_home) in marks['paths'], marks
    # The first project took the pre-workspace database instead.
    assert not legacy.exists(), 'the first project did not take the legacy database'
    passed('the chat home stays marked, and the first project takes the legacy database')

    # An older build rewrites the registry without the mark: the chat home is still no project.
    subprocess.run([binary, '--workspace', str(home), 'serve', '--stop'], env=env, capture_output=True, timeout=35)
    started_home = False
    registry = json.loads((data / 'data' / 'workspaces.json').read_text())
    for workspace in registry['workspaces']:
        workspace.pop('chatHome', None)
    (data / 'data' / 'workspaces.json').write_text(json.dumps(registry))
    paths, roots, listing = catalog_paths(cli('desktop', ws=project, input=json.dumps({'action': 'projects'})))
    assert str(canonical_home) not in paths and str(canonical_home) not in roots, listing
    connection = json.loads(cli('desktop', ws=home, input='{"action":"connect"}'))
    started_home = True
    assert api('GET', '/info')['projectTrusted'] is False
    again = api('POST', '/threads', {'title': 'After'})
    assert again['directory'].startswith(str(canonical_home / today)), again
    passed('the chat home survives an older build rewriting the registry, and runs as the chat home without the flag')

    # A project, or a Git repository's own folder, is never made the chat home.
    for folder in (project,):
        refused = subprocess.run([binary, '--workspace', str(folder), '--chat-home', 'desktop'],
                                 input='{"action":"connect"}', env=env, text=True, capture_output=True, timeout=60)
        assert refused.returncode != 0 and 'chat home must be a folder of its own' in refused.stderr, refused
    passed('--chat-home refuses a project or a Git repository')
finally:
    if started_home:
        subprocess.run([binary, '--workspace', str(home), 'serve', '--stop'], env=env,
                       capture_output=True, timeout=35)
    e2e_scratch.settle(root)

print(f'{checks} checks passed')
