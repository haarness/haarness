#!/usr/bin/env python3
"""Real CLI and server, echo provider: MCP settings and connection errors never show the credentials
in a server's URL or command arguments, while the rest stays readable. Nor do a failed marketplace
clone, a remote instruction URL that cannot be reached, or the log line of a request Auto approved.

Usage: HAARNESS_BIN=target/debug/haarness python3 scripts/test-mcp-redaction.py
Set HAARNESS_E2E_DIR to keep the run's state under a chosen folder.
"""
import json, os, pathlib, subprocess, tempfile, time, urllib.error, urllib.request, uuid
import e2e_scratch

binary = pathlib.Path(os.environ.get('HAARNESS_BIN', 'target/debug/haarness')).resolve()
root = pathlib.Path(tempfile.mkdtemp(prefix='haarness-mcp-redaction-', dir=os.environ.get('HAARNESS_E2E_DIR'))).resolve()
workspace = root/'project'; workspace.mkdir()
state = root/'state'; (state/'config').mkdir(parents=True); (state/'data').mkdir()
SECRETS = ['hunter2', 'fixture-url-secret', 'fixture-arg-secret', 'fixture-flag-secret',
           'fixture-clone-secret', 'fixture-instruction-secret', 'fixture-command-secret',
           'fixture-review-secret']
# Port 9 (discard) refuses the connection, so the server reports why it could not connect.
(state/'config/config.toml').write_text('''[defaults]
provider = "echo"
model = "echo-1"

[mcp.keyed]
transport = "streamable_http"
url = "http://someone:hunter2@127.0.0.1:9/mcp?api_key=fixture-url-secret&region=eu"
timeoutMs = 2000

[mcp.local]
transport = "stdio"
command = "/nonexistent/fixture-mcp"
args = ["--api-key", "fixture-arg-secret", "--token", "fixture-flag-secret", "--region", "eu"]
enabled = false

[permissions]
bash = "ask"
''')
(state/'config/haarness.json').write_text(json.dumps({'instructions': [
    'http://fixture:fixture-instruction-secret@127.0.0.1:9/rules.md?token=fixture-instruction-secret']}))
env = {k: v for k, v in os.environ.items() if not k.startswith('HAARNESS_')}
env.update(HAARNESS_DATA_DIR=str(state), HAARNESS_HOME=str(root/'home'), HAARNESS_TEST_PROVIDER='1',
           HAARNESS_DEFAULT_PROVIDER='echo', HAARNESS_DEFAULT_MODEL='echo-1')
url = None; token = None; checks = []

def run(args, **kwargs): return subprocess.run(args, check=True, text=True, capture_output=True, timeout=60, **kwargs).stdout
def cli(*args): return run([str(binary), '--workspace', str(workspace), *args], env=env)
def api(path, body=None):
    headers = {'Content-Type': 'application/json'}
    if token: headers['Authorization'] = 'Bearer ' + token
    request = urllib.request.Request(url + '/v1' + path, data=None if body is None else json.dumps(body).encode(), headers=headers)
    with urllib.request.urlopen(request, timeout=15) as reply:
        data = reply.read(); return json.loads(data) if data else None
def until(predicate, label, seconds=30):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if result := predicate(): return result
        time.sleep(.1)
    raise AssertionError(label)
def clean(text, label):
    for secret in SECRETS:
        assert secret not in text, f'{label} shows {secret}: {text}'
def check(label):
    checks.append(label); print('PASS ' + label, flush=True)
def logs(): return '\n'.join(path.read_text(errors='replace') for path in state.rglob('*.log*') if path.is_file())

try:
    run([str(binary), '--workspace', str(workspace), 'serve', '--daemon', '--port', '0'], env=env)
    registry = json.loads((state/'data/workspaces.json').read_text())
    identity = next(item['id'] for item in registry['workspaces'] if item['path'] == str(workspace))
    runtime = json.loads((state/'cache/workspaces'/identity/'runtime.json').read_text())
    url, token = runtime['url'], runtime.get('token')

    keyed = json.loads(cli('mcp', 'get', 'keyed'))
    clean(json.dumps(keyed), 'mcp get')
    assert keyed['config']['url'] == 'http://<redacted>@127.0.0.1:9/mcp?api_key=<redacted>&region=eu', keyed
    local = json.loads(cli('mcp', 'get', 'local'))
    clean(json.dumps(local), 'mcp get')
    assert local['config']['args'] == ['--api-key', '<redacted>', '--token', '<redacted>', '--region', 'eu'], local
    shown = cli('mcp', 'get', 'keyed', '--show-secrets')
    assert 'fixture-url-secret' in shown and 'hunter2' in shown, shown
    check('haarness mcp get hides URL and argument credentials and shows them with --show-secrets')

    failed = until(lambda: next((item for item in api('/mcp') if item['name'] == 'keyed' and item['status'] == 'failed'), None),
                   'unreachable server was not reported as failed')
    clean(json.dumps(failed), 'MCP status')
    assert '127.0.0.1:9' in (failed.get('detail') or ''), failed
    check(f'a failed connection names the server without its credentials: {failed["detail"]}')

    # The settings view behind the TUI and Desktop menus: details and the edit form.
    views = [api('/manage/mcp', {'action': action, 'name': name, 'scope': 'global'})
             for action in ('inspect', 'edit_form') for name in ('keyed', 'local')]
    clean(json.dumps(views), 'settings view')
    assert all('<redacted>' in json.dumps(view) for view in views), views
    # Submitting a form sends its request with each field's value, as the clients do.
    for form in (views[2], views[3]):
        api('/manage/mcp', {**form['submit'], **{field['key']: field['value'] for field in form['fields']}})
    kept = json.loads(cli('mcp', 'get', 'keyed', '--show-secrets'))['config']['url']
    assert kept == 'http://someone:hunter2@127.0.0.1:9/mcp?api_key=fixture-url-secret&region=eu', kept
    kept = json.loads(cli('mcp', 'get', 'local', '--show-secrets'))['config']['args']
    assert kept == ['--api-key', 'fixture-arg-secret', '--token', 'fixture-flag-secret', '--region', 'eu'], kept
    check('the settings view and edit form hide them, and saving the form unchanged keeps the real values')

    # Edited arguments (as the model's mcp_manage or a person sends them): each hidden value stays
    # with its own flag, and one that cannot be matched is refused rather than guessed.
    def edit_args(values): return api('/manage/mcp', {'action': 'edit', 'name': 'local', 'scope': 'global', 'args': values})
    def saved_args(): return json.loads(cli('mcp', 'get', 'local', '--show-secrets'))['config']['args']
    edit_args(['--token', '<redacted>', '--api-key', '<redacted>', '--region', 'eu'])
    assert saved_args() == ['--token', 'fixture-flag-secret', '--api-key', 'fixture-arg-secret', '--region', 'eu'], saved_args()
    edit_args(['--token', '<redacted>'])
    assert saved_args() == ['--token', 'fixture-flag-secret'], saved_args()
    try:
        edit_args(['--api-key', '<redacted>'])
        raise AssertionError('an unmatched hidden value was saved')
    except urllib.error.HTTPError as error:
        message = error.read().decode()
        assert 'in full' in message, message
    assert saved_args() == ['--token', 'fixture-flag-secret'], saved_args()
    check('reordered and shortened arguments keep each hidden value with its flag; an unmatched one is refused')

    thread = api('/threads', {'title': 'MCP settings', 'provider': 'echo', 'model': 'echo-1', 'agent': 'build'})['id']
    api(f'/threads/{thread}/turns', {'requestId': str(uuid.uuid4()), 'text': '!tool mcp_manage {"action":"list"}'})
    until(lambda: not api('/turns/active'), 'turn still active')
    items = api(f'/threads/{thread}/items')
    clean(json.dumps(items), 'transcript')
    results = [item['data'] for item in items if item['data']['type'] == 'tool_result']
    assert results and '<redacted>' in results[-1]['output'], results
    check('the model reading MCP settings with mcp_manage sees no credentials')

    # A marketplace whose URL carries a token: git cannot reach it, and the error names it without the token.
    failed_clone = subprocess.run([str(binary), '--workspace', str(workspace), 'plugin', 'marketplace', 'add',
                                   'https://oauth2:fixture-clone-secret@127.0.0.1:9/team/market.git'],
                                  env=env, text=True, capture_output=True, timeout=60)
    assert failed_clone.returncode != 0, failed_clone.stdout
    clean(failed_clone.stdout + failed_clone.stderr, 'marketplace add')
    assert 'git clone failed for https://<redacted>@127.0.0.1:9/team/market.git' in failed_clone.stderr, failed_clone.stderr
    check('a failed marketplace clone names the source without its token')

    # A request Auto answers is logged with the command's credentials hidden.
    chat = api('/threads', {'title': 'Auto log', 'provider': 'echo', 'model': 'echo-1', 'agent': 'build'})['id']
    request = urllib.request.Request(f'{url}/v1/threads/{chat}/approval-mode', method='PUT',
                                     data=json.dumps({'mode': 'auto'}).encode(),
                                     headers={'Content-Type': 'application/json', 'Authorization': 'Bearer ' + token})
    urllib.request.urlopen(request, timeout=15).read()
    command = json.dumps({'command': 'echo --token fixture-command-secret > /dev/null'})
    api(f'/threads/{chat}/turns', {'requestId': str(uuid.uuid4()), 'text': f'!tool bash {command}'})
    until(lambda: not api('/turns/active'), 'turn still active')
    until(lambda: 'approved once (Auto)' in logs(), 'no Auto approval line in the log')
    clean(logs(), 'daemon log')
    check("the Auto approval log line hides the command's token")

    # A request waiting for a person is logged the same way.
    review = api('/threads', {'title': 'Review log', 'provider': 'echo', 'model': 'echo-1', 'agent': 'build'})['id']
    command = json.dumps({'command': 'echo --token fixture-review-secret > /dev/null'})
    api(f'/threads/{review}/turns', {'requestId': str(uuid.uuid4()), 'text': f'!tool bash {command}'})
    waiting = until(lambda: [item for item in api('/permissions') if item['threadId'] == review], 'no request waited')
    api(f"/permissions/{waiting[0]['permissionId']}", {'decision': 'reject'})
    until(lambda: not api('/turns/active'), 'turn still active')
    until(lambda: 'permission requested; waiting for a reply' in logs(), 'no waiting line in the log')
    clean(logs(), 'daemon log')
    check("the waiting request's log line hides the command's token")
    assert 'skipped remote instructions http://<redacted>@127.0.0.1:9/rules.md?token=<redacted>' in logs(), \
        'no redacted notice for the unreachable instruction URL'
    check('an unreachable instruction URL is logged without its credentials')

    (root/'result.json').write_text(json.dumps({'passed': True, 'checks': checks}, indent=2))
    print(f'PASS {len(checks)} MCP redaction scenarios.', flush=True)
finally:
    if url: subprocess.run([str(binary), '--workspace', str(workspace), 'serve', '--stop'], env=env, capture_output=True, timeout=35)
    e2e_scratch.settle(root)
