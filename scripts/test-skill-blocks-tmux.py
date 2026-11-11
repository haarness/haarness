#!/usr/bin/env python3
"""Real TUI in tmux, echo provider: a slash command's fenced ```! block asks once per command, and a
skill the model loads never asks.

The project asks before every shell command (`bash = "ask"`, Review). `/fenced`, typed in the TUI,
holds a fenced block of two commands: a card names the first, and once it is allowed a second card
names the second; the prompt the model gets holds both outputs, one per line. Then the model loads
the `quiet` skill, whose inline command and fenced block would each ask: no card appears, the turn
ends, and the skill's text names each command in a note instead of its output, so nothing ran.

Usage: HAARNESS_BIN=target/debug/haarness python3 scripts/test-skill-blocks-tmux.py
Set HAARNESS_E2E_DIR to keep the run's state under a chosen folder.
"""
import json, os, pathlib, subprocess, tempfile, time, urllib.request, uuid
import e2e_scratch, e2e_tmux

binary = pathlib.Path(os.environ.get('HAARNESS_BIN', 'target/debug/haarness')).resolve()
root = pathlib.Path(tempfile.mkdtemp(prefix='haarness-skill-blocks-', dir=os.environ.get('HAARNESS_E2E_DIR'))).resolve()
project = root/'project'; state = root/'state'; home = root/'home'
for folder in (project/'.haarness/commands', project/'.haarness/skills/quiet', state, home):
    folder.mkdir(parents=True, exist_ok=True)
(project/'.haarness/config.toml').write_text(
    '[defaults]\napproval_mode="review"\n[permissions]\nbash="ask"\n[lsp]\nenabled=false\n')
(project/'.haarness/commands/fenced.md').write_text(
    '---\ndescription: Two commands in a fenced block\n---\nContext:\n```!\nprintf FENCE_ONE\nprintf FENCE_TWO\n```\nEnd of context\n')
(project/'.haarness/skills/quiet/SKILL.md').write_text(
    '---\nname: quiet\ndescription: Commands that would ask\n---\nQUIET !`touch inline-ran.txt`\n```!\ntouch fenced-ran.txt\n```\nQUIET_END\n')
session = 'haarness-skill-blocks-' + uuid.uuid4().hex[:8]
checks = []
evidence = root/'evidence'; evidence.mkdir()

def run(args, **kwargs): return subprocess.run(args, check=True, text=True, capture_output=True, timeout=60, **kwargs).stdout
def capture(): return run(['tmux', 'capture-pane', '-p', '-J', '-t', session])
def keys(*values): run(['tmux', 'send-keys', '-t', session, *values])
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
def api():
    registry = json.loads((state/'data/workspaces.json').read_text())
    identity = next(item['id'] for item in registry['workspaces'] if item['path'] == str(project))
    runtime = json.loads((state/'cache/workspaces'/identity/'runtime.json').read_text())
    def call(path, body=None):
        request = urllib.request.Request(runtime['url'] + '/v1' + path,
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
    checks.append(label); (evidence/f'{len(checks)}.txt').write_text(capture()); print('PASS ' + label, flush=True)

env = {**os.environ, 'HAARNESS_DATA_DIR': str(state), 'HAARNESS_HOME': str(home), 'HOME': str(home),
       'HAARNESS_TEST_PROVIDER': '1', 'HAARNESS_DEFAULT_PROVIDER': 'echo', 'HAARNESS_DEFAULT_MODEL': 'echo-1',
       'HAARNESS_NO_CONNECT_PROMPT': '1'}
started = False
try:
    command = e2e_tmux.pane_command(env, [binary, '--workspace', project, '--trust-project'])
    run(['tmux', 'new-session', '-d', '-x', '180', '-y', '50', '-c', str(root), '-s', session, command])
    until(lambda: 'echo' in capture().lower(), 'TUI did not open')
    call = until(connect, 'the server did not start')
    started = True

    def asked(text):
        pending = call('/permissions')
        return pending if len(pending) == 1 and text in json.dumps(pending) else None
    run(['tmux', 'send-keys', '-l', '-t', session, '/fenced']); keys('Enter')
    until(lambda: asked('printf FENCE_ONE'), 'no card for the first command')
    until(lambda: 'Allow once' in capture() and 'printf FENCE_ONE' in capture(), 'the first card is not shown')
    assert 'FENCE_TWO' not in json.dumps(call('/permissions'))
    check('a fenced block asks about its first command on a card of its own')
    keys('y')
    until(lambda: asked('printf FENCE_TWO'), 'no card for the second command')
    until(lambda: 'Allow once' in capture() and 'printf FENCE_TWO' in capture(), 'the second card is not shown')
    check('once it is allowed, a second card asks about the second command')
    keys('y')
    chat = next(thread['id'] for thread in call('/threads') if not thread.get('parentId'))
    def prompts():
        return [item['data']['text'] for item in call(f'/threads/{chat}/items') if item['data']['type'] == 'user_message']
    until(lambda: any('Context:\nFENCE_ONE\nFENCE_TWO\nEnd of context' in text for text in prompts()), 'the block outputs are not in the prompt')
    until(lambda: 'FENCE_TWO' in capture() and not call('/turns/active'), 'the command turn did not end')
    check("the model gets both commands' outputs, one per line")

    cards_before = sum(1 for item in call(f'/threads/{chat}/items') if item['data']['type'] == 'permission_request')
    paste('!tool skill ' + json.dumps({'name': 'quiet'}))
    def skill_result():
        results = [item['data'] for item in call(f'/threads/{chat}/items')
                   if item['data']['type'] == 'tool_result' and item['data']['name'] == 'skill']
        return results[-1] if results else None
    result = until(skill_result, 'the skill did not load')
    until(lambda: not call('/turns/active'), 'the skill turn did not end')
    cards = sum(1 for item in call(f'/threads/{chat}/items') if item['data']['type'] == 'permission_request')
    assert cards == cards_before and not call('/permissions'), (cards_before, cards)
    assert not result['is_error'], result
    output = result['output']
    assert 'QUIET (skipped: `touch inline-ran.txt` asks for approval, which loading a skill never does; call bash with it to run it)' in output, output
    assert '(skipped: `touch fenced-ran.txt` asks for approval' in output and 'QUIET_END' in output, output
    assert not (project/'inline-ran.txt').exists() and not (project/'fenced-ran.txt').exists()
    until(lambda: 'Tool skill =>' in capture(), 'the TUI did not show the skill result')
    assert 'Allow once' not in capture()
    check('a skill the model loads raises no card and names the commands it did not run')
    (root/'results.json').write_text(json.dumps(checks, indent=2))
    print('PASS', len(checks), 'skill block TUI checks.', flush=True)
finally:
    subprocess.run(['tmux', 'kill-session', '-t', session], capture_output=True)
    if started:
        subprocess.run([str(binary), '--workspace', str(project), '--trust-project', 'serve', '--stop'],
                       env=env, capture_output=True, timeout=60)
    e2e_scratch.settle(root)
