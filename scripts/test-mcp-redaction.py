#!/usr/bin/env python3
"""Real CLI and server, echo provider: MCP settings and connection errors never show the credentials
in a server's URL or command arguments, while the rest stays readable. Nor do a failed marketplace
clone, a remote instruction URL that cannot be reached, or the log line of a request Auto approved.
An MCP entry's `${TOKEN}` reference reaches its server resolved once and stays in the file through
`haarness mcp disable/enable`, which write only the flag, in the entry's own (opencode) shape. A
headersHelper's `${VAR}` holding `$(…)` reaches the server as text and never runs. `haarness mcp auth
--no-browser` against a server whose sign-in link is `file:` stops before printing it, and its debug log
(rmcp writes addresses as `Url { … password: Some(…) }`) and `haarness logs export` hide a raw-`@`
password.

Usage: HAARNESS_BIN=target/debug/haarness python3 scripts/test-mcp-redaction.py
Set HAARNESS_E2E_DIR to keep the run's state under a chosen folder.
"""
import http.server, json, os, pathlib, subprocess, tempfile, threading, time, urllib.error, urllib.request, uuid, zipfile
import e2e_scratch
from e2e_env import clean_env

binary = pathlib.Path(os.environ.get('HAARNESS_BIN', 'target/debug/haarness')).resolve()
root = pathlib.Path(tempfile.mkdtemp(prefix='haarness-mcp-redaction-', dir=os.environ.get('HAARNESS_E2E_DIR'))).resolve()
workspace = root/'project'; workspace.mkdir()
state = root/'state'; (state/'config').mkdir(parents=True); (state/'data').mkdir()
SECRETS = ['hunter2', 'fixture-url-secret', 'fixture-arg-secret', 'fixture-flag-secret',
           'fixture-clone-secret', 'fixture-instruction-secret', 'fixture-command-secret',
           'fixture-review-secret', 'fixture-inner-secret', 'fixture-at', 'fixture-proxy-secret']

# A remote MCP server that wants OAuth and offers a `file:` sign-in link, and one that writes down the
# headers its client sent.
seen_headers = []
issuer = {}
class Fixture(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args): pass
    def reply(self, status, body=None, headers=()):
        data = b'' if body is None else json.dumps(body).encode()
        self.send_response(status)
        for name, value in headers: self.send_header(name, value)
        if body is not None: self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(data))); self.end_headers(); self.wfile.write(data)
    def do_GET(self):
        base = f'http://127.0.0.1:{self.server.server_port}'
        if self.path.startswith('/.well-known/oauth-authorization-server'):
            return self.reply(200, {'issuer': issuer.get('value', base), 'authorization_endpoint': 'file:///nonexistent/haarness-fixture.app',
                                    'token_endpoint': base + '/token', 'response_types_supported': ['code'],
                                    'code_challenge_methods_supported': ['S256']})
        self.reply(404 if self.path.startswith('/.well-known') else 405)
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get('Content-Length') or 0)) or b'{}')
        if self.path == '/oauth/mcp':
            return self.reply(401, headers=[('WWW-Authenticate', 'Bearer')])
        seen_headers.append({name.lower(): value for name, value in self.headers.items()})
        if 'id' not in body: return self.reply(202)
        result = {'protocolVersion': '2025-06-18', 'capabilities': {'tools': {}}, 'serverInfo': {'name': 'helper', 'version': '1'}} \
            if body.get('method') == 'initialize' else {'tools': []}
        self.reply(200, {'jsonrpc': '2.0', 'id': body['id'], 'result': result})
fixture = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Fixture)
threading.Thread(target=fixture.serve_forever, daemon=True).start()
port = fixture.server_port
marker = root/'helper-ran'

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
args = ["--api-key", "fixture-arg-secret", "--token", "fixture-flag-secret", "--region", "eu", "--proxy", "fixture:fixture-proxy-secret@127.0.0.1:3128"]
enabled = false

[permissions]
bash = "ask"
''')
# A stdio MCP server that writes down the environment value it was started with.
received = root/'received-token'
(root/'refs_server.py').write_text('''import json, os, sys
open(os.environ["FIXTURE_OUT"], "w").write(os.environ["FIXTURE_TOKEN"])
for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request:
        continue
    result = {"protocolVersion": "2024-11-05", "capabilities": {"tools": {}}, "serverInfo": {"name": "refs", "version": "1"}} \\
        if request.get("method") == "initialize" else {"tools": []}
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)
''')
refs_entry = {'type': 'local', 'command': ['python3', str(root/'refs_server.py')],
              'environment': {'FIXTURE_TOKEN': '${FIXTURE_OUTER}', 'FIXTURE_OUT': str(received)}, 'timeout': 9000}
(state/'config/haarness.json').write_text(json.dumps({'instructions': [
    'http://fixture:fixture-instruction-secret@127.0.0.1:9/rules.md?token=fixture-instruction-secret'],
    'mcp': {'refs': refs_entry,
            'oauthfile': {'type': 'remote', 'url': f'http://someone:fixture-at@secret@127.0.0.1:{port}/oauth/mcp',
                          'oauthClientId': 'fixture-client', 'enabled': False},
            'helper': {'type': 'remote', 'url': f'http://127.0.0.1:{port}/helper/mcp', 'oauth': False,
                       'headersHelper': 'printf \'{"X-Fixture":"%s","X-Quoted":"${FIXTURE_HELPER_VALUE}"}\' "${FIXTURE_HELPER_VALUE}"'}}}))
env = clean_env()
env.update(HAARNESS_DATA_DIR=str(state), HAARNESS_HOME=str(root/'home'), HAARNESS_TEST_PROVIDER='1',
           HAARNESS_DEFAULT_PROVIDER='echo', HAARNESS_DEFAULT_MODEL='echo-1',
           # The variable's value names another; it must reach the server as written.
           FIXTURE_OUTER='outer-${FIXTURE_INNER}', FIXTURE_INNER='fixture-inner-secret',
           # Shell syntax in a value a headersHelper reads: it must reach the server as text.
           FIXTURE_HELPER_VALUE=f'v $(touch {marker}) `touch {marker}` end')
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

    def received_token(): return received.exists() and received.read_text()
    assert until(received_token, 'the MCP server was not started') == 'outer-${FIXTURE_INNER}', received.read_text()
    for action, enabled in (('disable', False), ('enable', True)):
        received.unlink(missing_ok=True)
        cli('mcp', action, 'refs')
        saved = (state/'config/haarness.json').read_text()
        # Only the flag is written: the entry keeps opencode's shape (`type`, a `command` list,
        # `environment`, `timeout`) and its reference as written.
        assert json.loads(saved)['mcp']['refs'] == {**refs_entry, 'enabled': enabled}, saved
        assert 'outer-' not in saved and 'fixture-inner-secret' not in saved, saved
    assert until(received_token, 'enabling did not start the MCP server') == 'outer-${FIXTURE_INNER}', received.read_text()
    check("an MCP environment reference is resolved once for the server and stays in the file through disable/enable, which write only the flag")
    # opencode writes `timeout` in milliseconds.
    assert json.loads(cli('mcp', 'get', 'refs'))['config']['timeoutMs'] == 9000
    check("an opencode entry's timeout of 9000 is 9 seconds, not 9000")

    sent = until(lambda: next((headers for headers in seen_headers if 'x-fixture' in headers), None),
                 'the helper server got no headers from its headersHelper')
    value = env['FIXTURE_HELPER_VALUE']
    assert sent['x-fixture'] == value and sent['x-quoted'] == value, sent
    assert not marker.exists(), 'a headersHelper value ran as a command'
    check('a headersHelper value holding $(…) and backticks reaches the server as text, quoted or in single quotes, and never runs')

    debug_env = {**env, 'HAARNESS_LOG': 'debug,rmcp=debug'}
    def sign_in(): return subprocess.run([str(binary), '--workspace', str(workspace), 'mcp', 'auth', 'oauthfile', '--no-browser'],
                                         env=debug_env, text=True, capture_output=True, timeout=60)
    # rmcp's own error quotes the server's address, password included.
    auth = sign_in()
    assert auth.returncode != 0 and 'issuer mismatch' in auth.stderr, auth.stderr
    clean(auth.stdout + auth.stderr, 'mcp auth error')
    assert f'expected http://<redacted>@127.0.0.1:{port}/oauth/mcp' in auth.stderr, auth.stderr
    check('an OAuth error that quotes the MCP server address hides its user name and password')
    # The issuer rmcp expects for a server without resource metadata: the server's own address.
    issuer['value'] = f'http://someone:fixture-at%40secret@127.0.0.1:{port}/oauth/mcp'
    auth = sign_in()
    assert auth.returncode != 0, auth.stdout
    assert 'MCP server "oauthfile" sent a file: sign-in link; only http and https links open' in auth.stderr, auth.stderr
    assert 'file:' not in auth.stdout and 'Authorize MCP server' not in auth.stdout, auth.stdout
    check('haarness mcp auth --no-browser stops at a file: sign-in link before printing or opening it')
    debug_log = logs()
    assert 'discovery url: Url {' in debug_log, 'rmcp logged no discovery address at debug level'
    assert 'username: "<redacted>", password: Some("<redacted>")' in debug_log, 'the debug discovery address was not hidden'
    clean(debug_log, 'debug log')
    exported = root/'logs.zip'
    cli('logs', 'export', '--output', str(exported))
    with zipfile.ZipFile(exported) as archive:
        clean('\n'.join(archive.read(name).decode(errors='replace') for name in archive.namelist()), 'log export')
    check('the debug log and its export hide the raw-@ password of a URL rmcp writes as debug output')

    keyed = json.loads(cli('mcp', 'get', 'keyed'))
    clean(json.dumps(keyed), 'mcp get')
    assert keyed['config']['url'] == 'http://<redacted>@127.0.0.1:9/mcp?api_key=<redacted>&region=eu', keyed
    local = json.loads(cli('mcp', 'get', 'local'))
    clean(json.dumps(local), 'mcp get')
    assert local['config']['args'] == ['--api-key', '<redacted>', '--token', '<redacted>', '--region', 'eu',
                                       '--proxy', '<redacted>@127.0.0.1:3128'], local
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
    assert kept == ['--api-key', 'fixture-arg-secret', '--token', 'fixture-flag-secret', '--region', 'eu',
                    '--proxy', 'fixture:fixture-proxy-secret@127.0.0.1:3128'], kept
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
    until(lambda: 'approved once (Auto' in logs(), 'no Auto approval line in the log')
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
