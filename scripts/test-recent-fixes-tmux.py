#!/usr/bin/env python3
"""Real TUI/server shell approvals, quoted writes, undo and request accounting."""
import json
import os
from pathlib import Path
import shlex
import subprocess
import tempfile
import time
import urllib.request
import urllib.error


def run(args, **kwargs):
    return subprocess.run(args, check=True, text=True, capture_output=True, **kwargs).stdout


def until(check, label):
    end = time.monotonic() + 30
    while time.monotonic() < end:
        result = check()
        if result:
            return result
        time.sleep(.1)
    raise AssertionError(label)


binary = str(Path(os.environ.get('HAARNESS_BIN', 'target/debug/haarness')).resolve())
evidence = Path(os.environ.get('HAARNESS_FIXES_EVIDENCE', '/tmp/haarness-recent-fixes-tmux.txt'))
session = 'haarness-recent-fixes-' + str(os.getpid())
with tempfile.TemporaryDirectory(prefix='haarness-recent-fixes-') as directory:
    root = Path(directory).resolve()
    workspace = root / 'workspace'
    (workspace / '.haarness').mkdir(parents=True)
    # Chats start in Auto; the outside-folder asks wait for a person, so ask for Review.
    (workspace / '.haarness/config.toml').write_text('[defaults]\nprovider="echo"\nmodel="echo-1"\napproval_mode="review"\n')
    outside = root / 'external space'
    outside.mkdir()
    (outside / 'input').write_text('external-read-proof')
    (workspace / 'input').write_text('b\na\n')
    run(['git', 'init', '-q', str(workspace)])
    env = dict(os.environ, HAARNESS_DATA_DIR=str(root / 'data'), HAARNESS_TEST_PROVIDER='1')
    args = [binary, '--workspace', str(workspace), '--trust-project']
    endpoint = json.loads(run(args + ['desktop'], env=env, input='{"action":"connect"}', timeout=120))

    def api(path, body=None):
        request = urllib.request.Request(endpoint['url'] + '/v1' + path,
            data=None if body is None else json.dumps(body).encode(),
            headers={'Authorization': 'Bearer ' + endpoint['token'], 'Content-Type': 'application/json'})
        with urllib.request.urlopen(request, timeout=10) as response:
            body = response.read()
            return json.loads(body) if body else None

    thread = api('/threads', {'title': 'Recent fixes verification', 'provider': 'echo', 'model': 'echo-1'})['id']
    captures = []

    def capture():
        return run(['tmux', 'capture-pane', '-p', '-t', session])

    def key(value):
        run(['tmux', 'send-keys', '-t', session, value])

    def send(command):
        text = ' !tool bash ' + json.dumps({'command': command})
        run(['tmux', 'send-keys', '-l', '-t', session, text])
        key('Enter')
        until(lambda: any(item['data'].get('text', '').strip() == text.strip() for item in api('/threads/' + thread + '/items')), 'prompt was not submitted')

    def idle():
        until(lambda: not api('/turns/active'), 'turn remained active')
        until(lambda: '←→ select · enter choose · esc reject' not in capture(), 'resolved approval remained visible')

    def pending():
        value = until(lambda: api('/permissions'), 'external approval missing')
        assert value[0]['permission'] == 'external_directory', value
        until(lambda: 'Access outside the project' in capture() and '←→ select · enter choose · esc reject' in capture(), 'approval not rendered in TUI')
        captures.append(capture())

    try:
        command = shlex.join(['env', 'HAARNESS_DATA_DIR=' + env['HAARNESS_DATA_DIR'], 'HAARNESS_TEST_PROVIDER=1', *args, '--thread', thread])
        run(['tmux', 'new-session', '-d', '-x', '160', '-y', '45', '-s', session, command])
        until(lambda: 'review' in capture().lower(), 'TUI did not open')
        send('printf denied > ' + shlex.quote(str(outside / 'denied output')))
        pending()
        assert not (outside / 'denied output').exists()
        key('n')
        idle()
        assert not (outside / 'denied output').exists()
        send('printf approved > ' + shlex.quote(str(outside / 'approved output')))
        pending()
        key('y')
        until(lambda: (outside / 'approved output').exists(), 'approved redirect did not execute')
        idle()
        assert (outside / 'approved output').read_text() == 'approved'
        send('cat < ' + shlex.quote(str(outside / 'input')))
        pending()
        key('y')
        idle()
        until(lambda: 'external-read-proof' in capture(), 'input redirect result absent')
        send("printf '%s' 'literal > /tmp/unused-fix-path'")
        idle()
        assert not api('/permissions'), 'literal text created a false permission'
        send("sort input '-o' sorted")
        until(lambda: (workspace / 'sorted').exists(), 'quoted output flag did not execute')
        idle()
        assert (workspace / 'sorted').read_text() == 'a\nb\n'
        items = api('/threads/' + thread + '/items')
        usage = [item['data'] for item in items if item['data']['type'] == 'usage'][-1]
        assert usage['prompt']['system_tokens'] > 0 and usage['prompt']['tool_tokens'] > 0, usage
        api('/threads/' + thread + '/revert', {})
        assert not (workspace / 'sorted').exists(), 'quoted write lacked an undo checkpoint'
        api('/threads/' + thread + '/unrevert', {})
        assert (workspace / 'sorted').read_text() == 'a\nb\n'
        # Exercise recovery through the real server's MCP API and then call
        # the recovered tool through the provider/runtime from the TUI.
        fixture = Path(__file__).resolve().parents[1] / 'crates/haarness-mcp/tests/fixtures/recovery.py'
        log = root / 'mcp-calls'
        api('/mcp/recovery', {'transport':'stdio','command':'python3','args':[str(fixture)],'env':{'CALL_LOG':str(log)},'enabled':True,'timeout_ms':2000})
        try:
            api('/mcp/recovery/tools/crash', {'arguments':{}})
            raise AssertionError('crashing tool unexpectedly succeeded')
        except urllib.error.HTTPError:
            pass
        until(lambda: any(item['status'] == 'failed' for item in api('/mcp')), 'closed transport remained connected')
        assert not api('/mcp/recovery/tools')
        until(lambda: any(item['status'] == 'connected' for item in api('/mcp')), 'MCP did not reconnect')
        assert log.read_text() == 'called\n', 'interrupted tool was replayed'
        run(['tmux','send-keys','-l','-t',session,' !tool mcp__recovery__ping {}'])
        key('Enter')
        until(lambda: 'pong' in capture(), 'recovered MCP tool did not complete through TUI')
        idle()
        api('/mcp/recovery/disconnect', {})
        captures.append(capture())
        evidence.write_text('\n\n'.join(captures))
        print('PASS: quoted external allow/deny, input redirects, literal text, quoted mutation undo/redo, prepared usage, MCP transport recovery; TUI capture:', evidence)
    except BaseException:
        evidence.write_text('\n\n'.join([*captures, capture(), json.dumps(api('/permissions'), indent=2)]))
        raise
    finally:
        subprocess.run(['tmux', 'kill-session', '-t', session], capture_output=True)
        subprocess.run(args + ['serve', '--stop'], env=env, capture_output=True, timeout=30)
