#!/usr/bin/env python3
"""Real CLI and server, echo provider: MCP settings and connection errors never show the credentials
in a server's URL or command arguments, while the rest stays readable.

Usage: HAARNESS_BIN=target/debug/haarness python3 scripts/test-mcp-redaction.py
Set HAARNESS_E2E_DIR to keep the run's state under a chosen folder.
"""
import json, os, pathlib, subprocess, tempfile, time, urllib.request, uuid
import e2e_scratch

binary = pathlib.Path(os.environ.get('HAARNESS_BIN', 'target/debug/haarness')).resolve()
root = pathlib.Path(tempfile.mkdtemp(prefix='haarness-mcp-redaction-', dir=os.environ.get('HAARNESS_E2E_DIR'))).resolve()
workspace = root/'project'; workspace.mkdir()
state = root/'state'; (state/'config').mkdir(parents=True); (state/'data').mkdir()
SECRETS = ['hunter2', 'fixture-url-secret', 'fixture-arg-secret', 'fixture-flag-secret']
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
args = ["--api-key", "fixture-arg-secret", "--token=fixture-flag-secret", "--region", "eu"]
enabled = false
''')
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
    assert local['config']['args'] == ['--api-key', '<redacted>', '--token=<redacted>', '--region', 'eu'], local
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
    assert kept == ['--api-key', 'fixture-arg-secret', '--token=fixture-flag-secret', '--region', 'eu'], kept
    check('the settings view and edit form hide them, and saving the form unchanged keeps the real values')

    thread = api('/threads', {'title': 'MCP settings', 'provider': 'echo', 'model': 'echo-1', 'agent': 'build'})['id']
    api(f'/threads/{thread}/turns', {'requestId': str(uuid.uuid4()), 'text': '!tool mcp_manage {"action":"list"}'})
    until(lambda: not api('/turns/active'), 'turn still active')
    items = api(f'/threads/{thread}/items')
    clean(json.dumps(items), 'transcript')
    results = [item['data'] for item in items if item['data']['type'] == 'tool_result']
    assert results and '<redacted>' in results[-1]['output'], results
    check('the model reading MCP settings with mcp_manage sees no credentials')

    (root/'result.json').write_text(json.dumps({'passed': True, 'checks': checks}, indent=2))
    print(f'PASS {len(checks)} MCP redaction scenarios.', flush=True)
finally:
    if url: subprocess.run([str(binary), '--workspace', str(workspace), 'serve', '--stop'], env=env, capture_output=True, timeout=35)
    e2e_scratch.settle(root)
