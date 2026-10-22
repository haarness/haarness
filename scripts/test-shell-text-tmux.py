#!/usr/bin/env python3
"""Real TUI in tmux, echo provider: folder names stay plain text in shell commands.

A project whose folder name holds a space and `$(…)` runs a slash command whose !`…` snippet names
the folder in double quotes, in single quotes and inside a nested `$(basename "…")`: the reply shows
the name as written each time and nothing runs. `/editor` with a temporary folder named the same way hands `$EDITOR` the
composer file as one word.

Usage: HAARNESS_BIN=target/debug/haarness python3 scripts/test-shell-text-tmux.py
Set HAARNESS_E2E_DIR to keep the run's state under a chosen folder.
"""
import json, os, pathlib, shlex, subprocess, tempfile, time, urllib.request, uuid
import e2e_scratch

binary = pathlib.Path(os.environ.get('HAARNESS_BIN', 'target/debug/haarness')).resolve()
root = pathlib.Path(tempfile.mkdtemp(prefix='haarness-shell-text-', dir=os.environ.get('HAARNESS_E2E_DIR'))).resolve()
project = root/'odd $(touch project-ran) folder'
temporary = root/'tmp a $(touch editor-ran) b'
state = root/'state'; home = root/'home'
for folder in (project/'.haarness/commands', temporary, state, home):
    folder.mkdir(parents=True, exist_ok=True)
(project/'.haarness/commands/where.md').write_text(
    "---\ndescription: Name the project folder\n---\n"
    "WHERE !`printf '[%s]' \"${CLAUDE_PROJECT_DIR}\" '${CLAUDE_PROJECT_DIR}' \"$(basename \"${CLAUDE_PROJECT_DIR}\")\"`\n")
session = 'haarness-shell-text-' + uuid.uuid4().hex[:8]
checks = []

def run(args): return subprocess.run(args, check=True, text=True, capture_output=True, timeout=30).stdout
def capture(): return run(['tmux', 'capture-pane', '-p', '-J', '-t', session])
def keys(*values): run(['tmux', 'send-keys', '-t', session, *values])
def type_text(text): run(['tmux', 'send-keys', '-l', '-t', session, text])
def until(predicate, label, seconds=45):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if predicate(): return
        time.sleep(.2)
    raise AssertionError(f'{label}\n{capture()}')
def replies():
    """The echo replies of the project's chats, read from its server as the TUI shows them."""
    registry = json.loads((state/'data/workspaces.json').read_text())
    identity = next(item['id'] for item in registry['workspaces'] if item['path'] == str(project))
    runtime = json.loads((state/'cache/workspaces'/identity/'runtime.json').read_text())
    def get(path):
        request = urllib.request.Request(runtime['url'] + '/v1' + path, headers={'Authorization': 'Bearer ' + runtime['token']})
        with urllib.request.urlopen(request, timeout=15) as reply: return json.loads(reply.read())
    return [item['data'].get('text', '') for thread in get('/threads') for item in get(f"/threads/{thread['id']}/items")
            if item['data']['type'] == 'assistant_message']
def check(label):
    checks.append(label); print('PASS ' + label, flush=True)

try:
    command = shlex.join(['env', '-u', 'VISUAL', f'HAARNESS_DATA_DIR={state}', f'HAARNESS_HOME={home}', f'HOME={home}',
                          'HAARNESS_TEST_PROVIDER=1', 'HAARNESS_DEFAULT_PROVIDER=echo', 'HAARNESS_DEFAULT_MODEL=echo-1',
                          'HAARNESS_NO_CONNECT_PROMPT=1', f'TMPDIR={temporary}/',
                          "EDITOR=printf 'from the editor' >", str(binary), '--workspace', str(project), '--trust-project'])
    run(['tmux', 'new-session', '-d', '-x', '200', '-y', '50', '-c', str(root), '-s', session, command])
    until(lambda: 'echo' in capture().lower(), 'TUI did not open')
    type_text('/where'); keys('Enter')
    until(lambda: 'Echo: WHERE' in capture(), 'the command reply did not show')
    expected = f'Echo: WHERE [{project}][{project}][{project.name}]'
    until(lambda: any(reply.strip() == expected for reply in replies()), f'the snippet did not name the folder as written: {replies()}')
    assert not (root/'project-ran').exists() and not (project/'project-ran').exists(), 'the folder name ran as a command'
    check('a slash command snippet names a folder holding a space and $(…) as written, quoted, in single quotes and in a nested $(…)')
    type_text('/editor'); keys('Enter')
    until(lambda: 'from the editor' in capture(), 'the editor text did not reach the composer')
    assert not (root/'editor-ran').exists(), 'the temporary folder name ran as a command'
    check('/editor hands $EDITOR the composer file as one word in a temporary folder holding a space and $(…)')
    (root/'pane.txt').write_text(capture())
    print(f'PASS {len(checks)} shell text scenarios in the TUI.', flush=True)
finally:
    subprocess.run(['tmux', 'kill-session', '-t', session], capture_output=True)
    subprocess.run([str(binary), '--workspace', str(project), 'serve', '--stop'], capture_output=True, timeout=35,
                   env={**os.environ, 'HAARNESS_DATA_DIR': str(state), 'HAARNESS_HOME': str(home), 'HOME': str(home)})
    e2e_scratch.settle(root)
