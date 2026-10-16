#!/usr/bin/env python3
"""Real server, echo provider: webfetch stops oversized and stalled pages and honours Stop.

Usage: HAARNESS_BIN=target/debug/haarness python3 scripts/test-webfetch.py
Set HAARNESS_E2E_DIR to keep the run's state under a chosen folder.
"""
import http.server, json, os, pathlib, subprocess, tempfile, threading, time, urllib.request, uuid

binary = pathlib.Path(os.environ.get('HAARNESS_BIN', 'target/debug/haarness')).resolve()
root = pathlib.Path(tempfile.mkdtemp(prefix='haarness-webfetch-', dir=os.environ.get('HAARNESS_E2E_DIR'))).resolve()
workspace = root/'project'; workspace.mkdir()
state = root/'state'; (state/'config').mkdir(parents=True); (state/'data').mkdir()
ENDLESS_CAP = 64 * 1024 * 1024
sent = {}; requests = []; release = threading.Event()

class Web(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'
    def log_message(self, *args): pass
    def chunked(self, content_type='text/plain'):
        self.send_response(200); self.send_header('Content-Type', content_type)
        self.send_header('Transfer-Encoding', 'chunked'); self.send_header('Connection', 'close'); self.end_headers()
    def chunk(self, data): self.wfile.write(b'%x\r\n%s\r\n' % (len(data), data)); self.wfile.flush()
    def do_GET(self):
        requests.append(self.headers.get('Host', '') + self.path)
        if self.path == '/endless':
            # No Content-Length: only the size limit can stop the reader early.
            self.chunked(); block = b'a' * 65536; sent['endless'] = 0
            try:
                while sent['endless'] < ENDLESS_CAP:
                    self.chunk(block); sent['endless'] += len(block)
                self.wfile.write(b'0\r\n\r\n')
            except (BrokenPipeError, ConnectionResetError): pass
        elif self.path == '/stall':
            self.chunked(); self.chunk(b'first part of a page that never ends')
            release.wait(120)
        else:
            body = b'ordinary-page-proof'
            self.send_response(200); self.send_header('Content-Type', 'text/plain')
            self.send_header('Content-Length', str(len(body))); self.end_headers(); self.wfile.write(body)

web = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Web)
threading.Thread(target=web.serve_forever, daemon=True).start()
port = web.server_port
(state/'config/config.toml').write_text('[defaults]\nprovider="echo"\nmodel="echo-1"\n')
env = {k: v for k, v in os.environ.items() if not k.startswith('HAARNESS_')}
env.update(HAARNESS_DATA_DIR=str(state), HAARNESS_HOME=str(root/'home'), HAARNESS_TEST_PROVIDER='1',
           HAARNESS_DEFAULT_PROVIDER='echo', HAARNESS_DEFAULT_MODEL='echo-1')
url = None; token = None; checks = []

def run(args, **kwargs): return subprocess.run(args, check=True, text=True, capture_output=True, timeout=60, **kwargs).stdout
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
def results(thread): return [item['data'] for item in api(f'/threads/{thread}/items') if item['data']['type'] == 'tool_result']
def fetch(thread, args):
    """Starts an echo turn that calls webfetch; returns the turn and the count of earlier results."""
    before = len(results(thread))
    turn = api(f'/threads/{thread}/turns', {'requestId': str(uuid.uuid4()), 'text': '!tool webfetch ' + json.dumps(args)})
    return turn, before
def finish(thread, before, seconds=30):
    result = until(lambda: (values[before] if len(values := results(thread)) > before else None), 'tool result missing', seconds)
    until(lambda: not api('/turns/active'), 'turn still active', seconds)
    return result
def check(label):
    checks.append(label); print('PASS ' + label, flush=True)
def rss_kb(pid): return int(subprocess.run(['ps', '-o', 'rss=', '-p', str(pid)], text=True, capture_output=True).stdout.strip() or 0)

try:
    run([str(binary), '--workspace', str(workspace), 'serve', '--daemon', '--port', '0'], env=env)
    registry = json.loads((state/'data/workspaces.json').read_text())
    identity = next(item['id'] for item in registry['workspaces'] if item['path'] == str(workspace))
    runtime = json.loads((state/'cache/workspaces'/identity/'runtime.json').read_text())
    url, token, pid = runtime['url'], runtime.get('token'), runtime.get('pid')
    thread = api('/threads', {'title': 'Webfetch limits', 'provider': 'echo', 'model': 'echo-1', 'agent': 'build'})['id']

    _, before = fetch(thread, {'url': f'http://127.0.0.1:{port}/ordinary'})
    result = finish(thread, before)
    assert not result['is_error'] and 'ordinary-page-proof' in result['output'], result
    check('an ordinary page is fetched')

    rss_before = rss_kb(pid) if pid else 0
    _, before = fetch(thread, {'url': f'http://127.0.0.1:{port}/endless'})
    result = finish(thread, before, 60)
    assert result['is_error'] and 'Response too large' in result['output'], result
    until(lambda: 'endless' in sent and sent['endless'] < ENDLESS_CAP, 'server kept sending')
    time.sleep(.5)
    assert sent['endless'] < 24 * 1024 * 1024, sent
    growth = (rss_kb(pid) - rss_before) // 1024 if pid else 0
    check(f'a body with no length stops at the limit (server sent {sent["endless"] // 1048576} MiB of 64, daemon grew {growth} MiB)')

    turn, before = fetch(thread, {'url': f'http://127.0.0.1:{port}/stall'})
    until(lambda: any(path.endswith('/stall') for path in requests), 'stalled page was never requested')
    time.sleep(1)
    assert api('/turns/active'), 'turn ended before Stop'
    started = time.monotonic()
    api(f'/turns/{turn["id"]}/interrupt', {})
    until(lambda: not api('/turns/active'), 'Stop did not end a fetch that never finishes', 10)
    elapsed = time.monotonic() - started
    result = until(lambda: (values[before] if len(values := results(thread)) > before else None), 'cancelled call has no result')
    assert result['is_error'], result
    check(f'Stop ends a fetch whose body never finishes ({elapsed:.1f} s)')

    (root/'result.json').write_text(json.dumps({'passed': True, 'checks': checks}, indent=2))
    print(f'PASS {len(checks)} webfetch scenarios. Evidence: {root}', flush=True)
finally:
    release.set()
    if url: subprocess.run([str(binary), '--workspace', str(workspace), 'serve', '--stop'], env=env, capture_output=True, timeout=35)
    web.shutdown(); web.server_close()
