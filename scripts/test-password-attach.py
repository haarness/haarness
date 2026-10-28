#!/usr/bin/env python3
"""Real server + CLI + tmux verification for optional password authentication."""
import base64, json, os, pathlib, shlex, subprocess, tempfile, time, urllib.request, urllib.error, urllib.parse
import e2e_scratch

binary = pathlib.Path(os.environ.get('HAARNESS_BIN', 'target/debug/haarness')).resolve()
root = pathlib.Path(tempfile.mkdtemp(prefix='haarness-password-attach-')).resolve()
evidence = pathlib.Path(os.environ.get('HAARNESS_EVIDENCE_DIR', root / 'evidence'))
evidence.mkdir(parents=True, exist_ok=True)

def run(args, **kwargs):
    return subprocess.run(args, text=True, capture_output=True, check=True, timeout=40, **kwargs).stdout

def until(predicate, label, seconds=30):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        try:
            result = predicate()
            if result: return result
        except (urllib.error.URLError, ConnectionError): pass
        time.sleep(.1)
    raise AssertionError(label)

for mode, password in [('anonymous', ''), ('password', '日本:attach-password')]:
    state = root / mode
    workspace = state / 'server-project'
    local = state / 'client-project'
    workspace.mkdir(parents=True)
    local.mkdir()
    env = {**os.environ, 'HAARNESS_DATA_DIR': str(state / 'state'), 'HAARNESS_HOME': str(state / 'home'),
           'HAARNESS_SERVER_TOKEN': '', 'HAARNESS_SERVER_PASSWORD': password, 'HAARNESS_SERVER_USERNAME': 'alex',
           'HAARNESS_TEST_PROVIDER': '1', 'HAARNESS_DEFAULT_PROVIDER': 'echo', 'HAARNESS_DEFAULT_MODEL': 'echo-1'}
    session = 'haarness-password-' + mode + '-' + str(os.getpid())
    url = None
    def api(path):
        headers = {} if not password else {'Authorization': 'Basic ' + base64.b64encode(('alex:' + password).encode()).decode()}
        with urllib.request.urlopen(urllib.request.Request(url + '/v1' + path, headers=headers), timeout=10) as response:
            return json.load(response)
    def tmux(*args): return run(['tmux', *args])
    def capture():
        result = subprocess.run(['tmux', 'capture-pane', '-p', '-t', session + ':0.0'], text=True, capture_output=True)
        return result.stdout
    def send(text):
        tmux('send-keys', '-l', '-t', session, text)
        tmux('send-keys', '-t', session, 'Enter')
    def start(port):
        run([str(binary), '--workspace', str(workspace), '--yolo', 'serve', '--daemon', '--host', '127.0.0.1', '--port', str(port)], env=env)
    try:
        help_text = run([str(binary), "serve", "--help"], env={**env, "HAARNESS_SERVER_PASSWORD": "help-secret-proof", "HAARNESS_SERVER_TOKEN": "help-token-proof"})
        assert "help-secret-proof" not in help_text and "help-token-proof" not in help_text
        start(0)
        registry = json.loads((state / 'state/data/workspaces.json').read_text())
        identity = next(item['id'] for item in registry['workspaces'] if item['path'] == str(workspace))
        discovery = json.loads((state / 'state/cache/workspaces' / identity / 'runtime.json').read_text())
        url = discovery['url']
        try:
            with urllib.request.urlopen(url + '/v1/info', timeout=10) as response: status = response.status
        except urllib.error.HTTPError as error: status = error.code
        assert status == (401 if password else 200), status
        assert api('/info')['workspace'] == str(workspace)
        # A second explicit serve may not silently change the running access policy.
        changed = subprocess.run([str(binary), '--workspace', str(workspace), 'serve', '--daemon', '--port', '0'],
                                 env={**env, 'HAARNESS_SERVER_PASSWORD': 'different-password'}, text=True, capture_output=True, timeout=30)
        assert changed.returncode != 0 and 'different authentication settings' in changed.stderr, changed.stderr
        assert api('/info')['workspace'] == str(workspace)
        output = run([str(binary), '--workspace', str(local), 'run', '--attach', url, '--provider', 'echo', '--model', 'echo-1', '--title', mode + '-auth', mode + '-cli-proof'], env=env)
        assert 'Echo: ' + mode + '-cli-proof' in output, output
        thread = next(item['id'] for item in api('/threads') if item['title'] == mode + '-auth')
        if password:
            failed = subprocess.run([str(binary), '--workspace', str(local), 'run', '--attach', url, '--provider', 'echo', '--model', 'echo-1', 'must-not-run'],
                                    env={**env, 'HAARNESS_SERVER_PASSWORD': 'wrong-password'}, capture_output=True, text=True, timeout=20)
            assert failed.returncode != 0 and 'wrong-password' not in failed.stderr, failed.stderr
            assert 'The server refused the token or password.' in failed.stderr, failed.stderr
        args = ['env', *[key + '=' + value for key, value in env.items() if key.startswith('HAARNESS_')], str(binary), '--workspace', str(local), '--session', thread, 'tui', '--attach', url]
        def launch(): tmux('new-session', '-d', '-x', '150', '-y', '42', '-s', session, shlex.join(args))
        launch()
        until(lambda: 'Echo: ' + mode + '-cli-proof' in capture(), 'TUI did not restore CLI history')
        send(mode + '-tui-proof')
        until(lambda: 'Echo: ' + mode + '-tui-proof' in capture(), 'TUI chat failed')
        send(' !tool write {"path":"auth-proof.txt","content":"server-owned"}')
        until(lambda: (workspace / 'auth-proof.txt').exists(), 'TUI tool write failed')
        assert (workspace / 'auth-proof.txt').read_text() == 'server-owned'
        assert not (local / 'auth-proof.txt').exists()
        until(lambda: not api('/turns/active'), 'Tool did not finish')
        (evidence / (mode + '-chat.txt')).write_text(capture())
        original = api('/info')['serverInstanceId']
        run([str(binary), '--workspace', str(workspace), 'serve', '--stop'], env=env)
        start(urllib.parse.urlparse(url).port)
        until(lambda: api('/info')['serverInstanceId'] != original, 'Server did not restart')
        until(lambda: 'Reconnected' in capture(), 'TUI did not reconnect after server restart')
        send(mode + '-restart-proof')
        until(lambda: 'Echo: ' + mode + '-restart-proof' in capture(), 'TUI chat failed after restart')
        (evidence / (mode + '-restart.txt')).write_text(capture())
        tmux('kill-session', '-t', session)
        launch()
        until(lambda: 'Echo: ' + mode + '-restart-proof' in capture(), 'TUI reattach did not restore history')
        (evidence / (mode + '-reattach.txt')).write_text(capture())
        (evidence / (mode + '-items.json')).write_text(json.dumps(api('/threads/' + thread + '/items'), indent=2))
        print('PASS ' + mode + ': CLI, attached TUI, server-side tools, restart, reattach, access-policy guard', flush=True)
    finally:
        subprocess.run(['tmux', 'kill-session', '-t', session], capture_output=True)
        subprocess.run([str(binary), '--workspace', str(workspace), 'serve', '--stop'], env=env, capture_output=True, timeout=40)
if os.environ.get('HAARNESS_EVIDENCE_DIR'):
    print('Evidence: ' + str(evidence), flush=True)
e2e_scratch.settle(root)
