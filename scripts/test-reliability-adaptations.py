#!/usr/bin/env python3
"""Real daemon, provider HTTP, MCP descendants, and tmux regression coverage."""
import datetime
import http.server
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request
import uuid

REPO = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get('HAARNESS_BIN', REPO / 'target/debug/haarness')).resolve()
OUT = Path(os.environ.get('HAARNESS_ADAPTATIONS_EVIDENCE', REPO / 'target/reliability-adaptations/tmux'))
OUT.mkdir(parents=True, exist_ok=True)
state = {'catalog': 'ok', 'response': 'ok', 'requests': []}


class Provider(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def reply(self, value, status=200):
        body = json.dumps(value).encode()
        self.send_response(status)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if state['catalog'] == 'failed':
            return self.reply({'error': 'fixture outage'}, 503)
        self.reply({'data': [] if state['catalog'] == 'empty' else [{'id': 'gpt-5.4-mini'}, {'id': 'gpt-4.1-mini'}]})

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        if self.path.endswith('/router-session'):
            return self.reply({'token': 'fixture-router', 'routerUrl': URL,
                               'expiresAt': (datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(hours=1)).isoformat()})
        state['requests'].append(body)
        blocked = state['response'] == 'blocked'
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.end_headers()
        if self.path.endswith('/responses'):
            events = [{'type': 'response.output_text.delta', 'delta': 'PARTIAL_FILTER_PROOF' if blocked else 'HANDOFF_READY'}]
            if blocked:
                events.append({'type': 'response.output_item.done', 'item': {'type': 'function_call', 'id': 'item', 'call_id': 'filtered-write',
                    'name': 'write', 'arguments': json.dumps({'path': 'must-not-exist', 'content': 'blocked'})}})
            events.append({'type': 'response.incomplete' if blocked else 'response.completed',
                'response': {'usage': {'input_tokens': 12, 'output_tokens': 4}, 'incomplete_details': {'reason': 'content_filter'} if blocked else None}})
        else:
            events = [{'choices': [{'delta': {'content': 'PARTIAL_FILTER_PROOF' if blocked else 'HANDOFF_READY'}, 'finish_reason': None}]},
                      {'choices': [{'delta': {}, 'finish_reason': 'content_filter' if blocked else 'stop'}], 'usage': {'prompt_tokens': 12, 'completion_tokens': 4}}]
        for event in events:
            self.wfile.write(('data: ' + json.dumps(event) + '\n\n').encode())
        self.wfile.flush()


def run(args, **kwargs):
    return subprocess.run(args, check=True, capture_output=True, text=True, **kwargs).stdout


def until(check, label, timeout=40):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        value = check()
        if value:
            return value
        time.sleep(.1)
    raise AssertionError(label)


service = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Provider)
URL = f'http://127.0.0.1:{service.server_port}'
threading.Thread(target=service.serve_forever, daemon=True).start()
sessions = []
checks = []
with tempfile.TemporaryDirectory(prefix='haarness-reliability-adaptations-') as temporary:
    root = Path(temporary).resolve()
    workspace = root / 'workspace'
    config = workspace / '.haarness'
    (config / 'agents').mkdir(parents=True)
    (config / 'config.toml').write_text('[defaults]\nprovider="echo"\nmodel="echo-1"\n[lsp]\nenabled=false\n')
    for agent in ['review', 'review-two']:
        (config / 'agents' / (agent + '.md')).write_text('---\ndescription: Fixture reviewer\nmode: primary\nmodel: openai/gpt-5.4-mini\n---\nReply briefly.\n')
    run(['git', 'init', '-q', str(workspace)])
    env = {key: os.environ[key] for key in ['PATH', 'HOME', 'USER', 'LANG', 'TMPDIR'] if key in os.environ}
    env.update(HAARNESS_DATA_DIR=str(root / 'data'), HAARNESS_HOME=str(root / 'home'), HAARNESS_TEST_PROVIDER='1',
               OPENAI_API_KEY='fixture-key', OPENAI_BASE_URL=URL + '/v1', HAARNESS_CLOUD_API_URL=URL,
               HAARNESS_CLOUD_CONFIG_DIR=str(root / 'account'))
    account = root / 'account'
    account.mkdir()
    credentials = account / 'credentials.json'
    credentials.write_text(json.dumps({'apiUrl': URL + '/', 'accessToken': 'fixture-account', 'refreshToken': 'fixture-refresh',
        'accessExpiresAt': (datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(hours=1)).isoformat()}))
    credentials.chmod(0o600)
    args = [str(BINARY), '--workspace', str(workspace), '--trust-project']
    endpoint = json.loads(run(args + ['desktop'], env=env, input='{"action":"connect"}', timeout=120))

    def api(path, body=None, method=None):
        request = urllib.request.Request(endpoint['url'] + '/v1' + path, method=method,
            data=None if body is None else json.dumps(body).encode(),
            headers={'Authorization': 'Bearer ' + endpoint['token'], 'Content-Type': 'application/json'})
        with urllib.request.urlopen(request, timeout=30) as response:
            data = response.read()
            return json.loads(data) if data else None

    def new(provider='echo', model='echo-1', title='Adaptation verification'):
        return api('/threads', {'title': title, 'provider': provider, 'model': model})['id']

    def start(thread, text):
        return api(f'/threads/{thread}/turns', {'requestId': str(uuid.uuid4()), 'text': text})

    def idle():
        until(lambda: not api('/turns/active'), 'turn remained active')

    def attach(thread, width=120):
        name = f'haarness-adaptations-{os.getpid()}-{len(sessions)}'
        sessions.append(name)
        command = shlex.join(['env', *[f'{key}={value}' for key, value in env.items()], *args, '--thread', thread])
        run(['tmux', 'new-session', '-d', '-x', str(width), '-y', '45', '-s', name, command])
        until(lambda: 'review' in capture(name).lower(), 'TUI did not open')
        return name

    def capture(name):
        return run(['tmux', 'capture-pane', '-p', '-t', name])

    def key(name, value):
        run(['tmux', 'send-keys', '-t', name, value])

    def send(name, text):
        run(['tmux', 'send-keys', '-l', '-t', name, ' ' + text])
        key(name, 'Enter')

    def proof(label, session=None):
        if session:
            (OUT / (label + '.txt')).write_text(capture(session))
        checks.append(label)
        print('PASS:', label, flush=True)

    try:
        thread = new()
        session = attach(thread)
        send(session, '!tool agent_switch {"agent":"review","reason":"Review the changes"}')
        pending = until(lambda: api('/permissions'), 'switch approval missing')[0]
        assert pending['alwaysPatterns'] == [f'agent_switch:{thread}:{"review".encode().hex()}'], pending
        until(lambda: 'Model: openai/gpt-5.4-mini' in capture(session) and 'Previous model: echo/echo-1' in capture(session), 'model disclosure missing')
        proof('switch-model-consent', session)
        key(session, 'a')
        until(lambda: 'only agent review in this chat' in capture(session), 'remembered scope missing')
        proof('switch-remembered-scope', session)
        key(session, 'Enter')
        until(lambda: api('/threads/' + thread)['agent'] == 'review', 'switch did not execute')
        until(lambda: 'HANDOFF_READY' in capture(session), 'fresh target turn did not complete')
        idle()
        for target, approve in [('review', True), ('review-two', False)]:
            api('/threads/' + thread, {'agent': 'build', 'provider': 'echo', 'model': 'echo-1'}, 'PATCH')
            send(session, '!tool agent_switch ' + json.dumps({'agent': target, 'reason': 'Explicitly switch agent'}))
            if approve:
                until(lambda: api('/threads/' + thread)['agent'] == target, 'remembered exact destination prompted again')
                idle()
                assert not api('/permissions')
            else:
                until(lambda: api('/permissions'), 'remembered approval leaked to another destination')
                # The server lists the request before the TUI draws its card;
                # a key pressed in between is typing, not an answer.
                until(lambda: 'Agent: review-two' in capture(session), 'switch approval card missing')
                key(session, 'n')
                idle()
                assert api('/threads/' + thread)['agent'] == 'build'
        proof('switch-destination-isolation', session)
        other = new(title='Separate switch session')
        other_session = attach(other)
        send(other_session, '!tool agent_switch {"agent":"review"}')
        until(lambda: api('/permissions'), 'remembered approval leaked to another session')
        # Change the current model while consent is pending: the old consent must fail.
        api('/threads/' + other, {'model': 'echo-changed'}, 'PATCH')
        until(lambda: 'Agent: review' in capture(other_session), 'stale switch approval card missing')
        key(other_session, 'y')
        idle()
        items = api(f'/threads/{other}/items')
        assert any('changed while awaiting approval' in item['data'].get('output', '') for item in items), items
        assert api('/threads/' + other)['agent'] == 'build'
        proof('switch-stale-consent-rejected', other_session)

        state['response'] = 'blocked'
        for provider in ['openai', 'haarness']:
            filtered = new(provider, 'gpt-5.4-mini', 'Filtered response ' + provider)
            filtered_session = attach(filtered)
            send(filtered_session, 'Show the filtered response fixture')
            until(lambda: 'safety check stopped' in capture(filtered_session), 'filtered response was not explained')
            idle()
            items = api(f'/threads/{filtered}/items')
            assert any(item['data']['type'] == 'assistant_message' and item['data']['text'] == 'PARTIAL_FILTER_PROOF' for item in items), items
            assert any(item['data']['type'] == 'error' and not item['data']['retryable'] for item in items), items
            assert not (workspace / 'must-not-exist').exists()
            proof('filtered-' + provider + '-wire-and-tui', filtered_session)
        state['response'] = 'ok'

        def catalog():
            time.sleep(1.1)
            return next(p for p in api('/providers/refresh', {}) if p['id'] == 'haarness')['models']
        assert catalog()
        state['catalog'] = 'failed'
        assert catalog(), 'Cloud outage erased last successful catalog'
        state['catalog'] = 'empty'
        assert catalog() == [], 'successful empty Cloud catalog retained removed models'
        state['catalog'] = 'ok'
        assert catalog(), 'Cloud catalog did not recover'
        proof('cloud-catalog-outage-empty-recovery')

        log = root / 'processes'
        mcp = {'transport': 'stdio', 'command': sys.executable,
               'args': [str(REPO / 'crates/haarness-mcp/tests/fixtures/process_tree.py')],
               'env': {'TREE_LOG': str(log)}, 'enabled': True, 'timeout_ms': 2000}
        api('/mcp/tree', mcp)
        pids = [int(pid) for pid in log.read_text().splitlines()]
        assert len(pids) == 3
        api('/mcp/tree/disconnect', {})
        def gone():
            for pid in pids:
                result = subprocess.run(['ps', '-p', str(pid), '-o', 'stat='], capture_output=True, text=True)
                if result.returncode == 0 and not result.stdout.lstrip().startswith('Z'):
                    return False
            return True
        until(gone, 'MCP descendant survived API disconnect')
        proof('mcp-api-descendant-cleanup')
        unicode_thread = new(title='👩‍💻' * 40 + ' 日本語')
        unicode_session = attach(unicode_thread, 60)
        run(['tmux', 'send-keys', '-l', '-t', unicode_session, '/sessions'])
        key(unicode_session, 'Enter')
        # tmux before 3.4 (Ubuntu 22.04 has 3.2a) does not join emoji drawn a
        # cell at a time: the next grapheme overwrites the laptop of 👩‍💻.
        version = run(['tmux', '-V']).split()[-1]
        number = re.search(r'(\d+)\.(\d+)', version)
        joins = not number or (int(number[1]), int(number[2])) >= (3, 4)
        until(lambda: ('👩‍💻' in capture(unicode_session) or not joins) and '…' in capture(unicode_session), 'Unicode session picker did not render a truncated label')
        rendered = capture(unicode_session)
        assert '\ufffd' not in rendered
        if joins:
            assert '👩' not in rendered.replace('👩‍💻', ''), 'truncation split a joined emoji'
            proof('unicode-narrow-terminal', unicode_session)
        else:
            print(f'SKIP: unicode-narrow-terminal joined emoji (tmux {version} draws them apart)')
        (OUT / 'results.json').write_text(json.dumps(checks, indent=2))
    finally:
        for session in sessions:
            try:
                (OUT / (session + '-final.txt')).write_text(capture(session))
            except subprocess.CalledProcessError:
                pass
            subprocess.run(['tmux', 'kill-session', '-t', session], capture_output=True)
        subprocess.run(args + ['serve', '--stop'], env=env, capture_output=True, timeout=30)
        service.shutdown()
print('Evidence:', OUT)
