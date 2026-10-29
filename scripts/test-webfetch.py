#!/usr/bin/env python3
"""Real server, echo provider: webfetch reads pages as text, meets rules for every spelling of an address,
reports redirects off the address by reason, follows one on it only where the rules allow its new address
(in a turn and in a workflow's own call), stops oversized and stalled pages, and honours Stop; media
generation still follows its backend's redirects.

Usage: HAARNESS_BIN=target/debug/haarness python3 scripts/test-webfetch.py
Set HAARNESS_E2E_DIR to keep the run's state under a chosen folder.
"""
import http.server, json, os, pathlib, shutil, subprocess, tempfile, threading, time, urllib.request, uuid
import e2e_scratch
from e2e_env import clean_env

binary = pathlib.Path(os.environ.get('HAARNESS_BIN', 'target/debug/haarness')).resolve()
root = pathlib.Path(tempfile.mkdtemp(prefix='haarness-webfetch-', dir=os.environ.get('HAARNESS_E2E_DIR'))).resolve()
workspace = root/'project'; workspace.mkdir()
state = root/'state'; (state/'config').mkdir(parents=True); (state/'data').mkdir()
ENDLESS_CAP = 64 * 1024 * 1024
sent = {}; requests = []; release = threading.Event()
PAGES = {
    '/page': ('text/html; charset=utf-8', b'<!doctype html><html><head><title>Fixture</title>'
              b'<style>.hidden-style-proof { color: red }</style><script>hiddenHeadScriptProof()</script></head>'
              b'<body><h1>visible-content-proof</h1><script>hiddenBodyScriptProof()</script>'
              b'<p>after-script-proof</p></body></html>'),
    '/bare': ('text/html; charset=utf-8', b'bare-nonce-314159\n'),
    '/landing': ('text/plain', b'same-host-landing-proof'),
    '/private': ('text/plain', b'private-page-proof'),
}

class Web(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'
    def log_message(self, *args): pass
    def chunked(self, content_type='text/plain'):
        self.send_response(200); self.send_header('Content-Type', content_type)
        self.send_header('Transfer-Encoding', 'chunked'); self.send_header('Connection', 'close'); self.end_headers()
    def chunk(self, data): self.wfile.write(b'%x\r\n%s\r\n' % (len(data), data)); self.wfile.flush()
    def do_POST(self):
        requests.append('POST ' + self.headers.get('Host', '') + self.path)
        self.rfile.read(int(self.headers.get('Content-Length', '0')))
        if self.path == '/moved':
            # A media backend that moved to another host name: generation follows it, as it always did.
            self.send_response(307); self.send_header('Location', f'http://localhost:{self.server.server_port}/generate')
            self.send_header('Content-Length', '0'); self.end_headers()
        else:
            body = json.dumps({'data': [{'b64_json': 'iVBORw0KGgo='}], 'mime_type': 'image/png'}).encode()
            self.send_response(200); self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(body))); self.end_headers(); self.wfile.write(body)
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
        elif self.path.startswith('/login?next='):
            # An open redirect on the host: it sends the visitor wherever `next` says.
            self.send_response(302); self.send_header('Location', self.path.split('=', 1)[1])
            self.send_header('Content-Length', '0'); self.end_headers()
        elif self.path in ('/hop', '/away', '/port'):
            # /hop stays on this host; /away leads to another host name for the same server; /port to another port.
            target = {'/hop': '/landing', '/away': f'http://localhost:{self.server.server_port}/landing',
                      '/port': 'http://127.0.0.1:1/landing'}[self.path]
            self.send_response(302); self.send_header('Location', target)
            self.send_header('Content-Length', '0'); self.end_headers()
        elif self.path == '/stall':
            self.chunked(); self.chunk(b'first part of a page that never ends')
            release.wait(120)
        else:
            content_type, body = PAGES.get(self.path, ('text/plain', b'ordinary-page-proof'))
            self.send_response(200); self.send_header('Content-Type', content_type)
            self.send_header('Content-Length', str(len(body))); self.end_headers(); self.wfile.write(body)

web = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Web)
threading.Thread(target=web.serve_forever, daemon=True).start()
port = web.server_port
(state/'config/config.toml').write_text('[defaults]\nprovider="echo"\nmodel="echo-1"\n[permissions]\n'
    f'webfetch = {{ "*" = "allow", "http://127.0.0.1:{port}/private*" = "deny", "http://localhost:{port}/*" = "deny" }}\n'
    'media_generate = "allow"\n')
env = clean_env()
# A workflow's own calls run on Bun; keep the one this machine has on PATH.
bun = shutil.which('bun') or str(pathlib.Path.home()/'.bun/bin/bun')
env['PATH'] = f'{pathlib.Path(bun).parent}:{env.get("PATH", "/usr/bin:/bin")}'
env.update(HAARNESS_DATA_DIR=str(state), HAARNESS_HOME=str(root/'home'), HAARNESS_TEST_PROVIDER='1',
           HAARNESS_DEFAULT_PROVIDER='echo', HAARNESS_DEFAULT_MODEL='echo-1',
           HAARNESS_IMAGE_GENERATE_URL=f'http://127.0.0.1:{port}/moved')
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

    _, before = fetch(thread, {'url': f'http://127.0.0.1:{port}/page'})
    output = finish(thread, before)['output']
    assert 'visible-content-proof' in output and 'after-script-proof' in output, output
    for hidden in ['hiddenHeadScriptProof', 'hiddenBodyScriptProof', 'hidden-style-proof']:
        assert hidden not in output, (hidden, output)
    _, before = fetch(thread, {'url': f'http://127.0.0.1:{port}/bare'})
    assert finish(thread, before)['output'].strip() == 'bare-nonce-314159'
    check('HTML reads as its text, without inline scripts and styles, and bare text is kept')

    for address in [f'HTTP://127.0.0.1:{port}/private', f'http://someone@127.0.0.1:{port}/private',
                    f'http://127.0.0.1:{port}/public/../private', f'http://LOCALHOST.:{port}/landing']:
        _, before = fetch(thread, {'url': address})
        result = finish(thread, before)
        assert result['is_error'] and 'denied by permission rule' in result['output'], (address, result)
    assert not any(path.endswith('/private') or path.startswith('localhost') for path in requests), requests
    check('a deny rule also meets other spellings of the address (case, user name, .., trailing dot)')

    _, before = fetch(thread, {'url': f'http://127.0.0.1:{port}/hop'})
    result = finish(thread, before)
    assert not result['is_error'] and 'same-host-landing-proof' in result['output'], result
    _, before = fetch(thread, {'url': f'http://127.0.0.1:{port}/away'})
    result = finish(thread, before)
    target = f'http://localhost:{port}/landing'
    assert not result['is_error'] and 'another host' in result['output'] and target in result['output'], result
    assert not any(path.startswith('localhost') for path in requests), requests
    _, before = fetch(thread, {'url': target})
    result = finish(thread, before)
    assert result['is_error'] and 'denied by permission rule' in result['output'], result
    check('a same-host redirect is followed; one to another host is reported, and fetching it meets the deny')

    for target in ['/private', '/%70rivate', '/private#part']:
        _, before = fetch(thread, {'url': f'http://127.0.0.1:{port}/login?next={target}'})
        result = finish(thread, before)
        assert not result['is_error'], result
        assert 'which the permission rules do not let this fetch follow' in result['output'], (target, result)
        assert result['metadata']['redirect'].startswith(f'http://127.0.0.1:{port}/'), result
    _, before = fetch(thread, {'url': f'http://127.0.0.1:{port}/login?next=/landing'})
    result = finish(thread, before)
    assert not result['is_error'] and 'same-host-landing-proof' in result['output'], result
    assert not any(path.split('#')[0] in (f'127.0.0.1:{port}/private', f'127.0.0.1:{port}/%70rivate')
                   for path in requests), requests
    check('a redirect on the host into a denied path, in any spelling, is reported and never requested')

    (workspace/'.haarness/workflows').mkdir(parents=True)
    (workspace/'.haarness/workflows/fetcher.ts').write_text(
        'import { workflow } from "@haarness-ai/plugin"\n'
        'export default workflow({ name: "fetcher", async run(args, ctx) {\n'
        f'  const stopped = await ctx.tool("webfetch", {{ url: "http://127.0.0.1:{port}/login?next=/private" }})\n'
        f'  const followed = await ctx.tool("webfetch", {{ url: "http://127.0.0.1:{port}/login?next=/landing" }})\n'
        '  return { stopped, followed }\n} })\n')
    before_requests = len(requests)
    output = run([str(binary), '--workspace', str(workspace), 'workflow', 'run', 'fetcher'], env=env)
    assert 'which the permission rules do not let this fetch follow' in output, output
    assert 'same-host-landing-proof' in output, output
    assert not any(path == f'127.0.0.1:{port}/private' for path in requests[before_requests:]), requests
    check("a workflow's own fetch meets the same rules on a redirect")

    _, before = fetch(thread, {'url': f'http://127.0.0.1:{port}/port'})
    result = finish(thread, before)
    assert 'which is another port of the same host' in result['output'], result
    check('a redirect to another port of the same host says so')

    before = len(results(thread))
    api(f'/threads/{thread}/turns', {'requestId': str(uuid.uuid4()), 'text': '!tool image_generate {"prompt":"Quiet red fox"}'})
    result = until(lambda: (values[before] if len(values := results(thread)) > before else None), 'image result missing')
    until(lambda: not api('/turns/active'), 'turn still active')
    assert not result['is_error'] and result['metadata'].get('path'), result
    assert f'POST localhost:{port}/generate' in requests, requests
    check('image generation still follows its backend to another host')

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
    print(f'PASS {len(checks)} webfetch scenarios.', flush=True)
finally:
    release.set()
    if url: subprocess.run([str(binary), '--workspace', str(workspace), 'serve', '--stop'], env=env, capture_output=True, timeout=35)
    web.shutdown(); web.server_close()
    e2e_scratch.settle(root)
