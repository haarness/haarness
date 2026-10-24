#!/usr/bin/env python3
"""Real server/CLI/TUI, provider-wire, and MCP regression proof in private fixtures."""
import http.server
import json
import os
from pathlib import Path
import shlex
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request
import uuid
from e2e_env import ISOLATED

REPO = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get("HAARNESS_BIN", REPO / "target/debug/haarness")).resolve()
EVIDENCE = Path(os.environ.get("HAARNESS_FIXES_EVIDENCE", REPO / "target/runtime-reliability/e2e"))
EVIDENCE.mkdir(parents=True, exist_ok=True)
REQUESTS = []
# Prompts that make the provider fall silent, and what each silent request saw:
# when it began and when the client closed its connection.
SILENT = ('SILENT_BEFORE', 'SILENT_AFTER', 'SILENT_ALWAYS')
SILENCES = []
STALLED = set()
IDLE_LIMIT = 2


def run(args, **kwargs):
    return subprocess.run(args, check=True, capture_output=True, text=True, **kwargs).stdout


def until(check, label, timeout=30):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(.05)
    raise AssertionError(label)


def clean(value):
    if isinstance(value, list):
        return [clean(item) for item in value]
    if isinstance(value, dict):
        return {key: clean(item) for key, item in value.items() if key != 'cache_control'}
    return value


class Provider(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def stall(self, marker, partial=None):
        """Answer with headers (and optionally the start of a response and
        keep-alive comments), then send nothing until the client hangs up."""
        started = time.monotonic()
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.end_headers()
        if partial:
            for event in partial:
                self.wfile.write(('data: ' + json.dumps(event) + '\n\n').encode())
            # Comments for longer than the idle limit: they must count as activity.
            for _ in range(6):
                self.wfile.write(b': keep-alive\n\n')
                self.wfile.flush()
                time.sleep(.5)
        self.wfile.flush()
        closed = None
        self.connection.settimeout(.1)
        deadline = time.monotonic() + 60
        while closed is None and time.monotonic() < deadline:
            try:
                if self.connection.recv(1) == b'':
                    closed = time.monotonic()
            except socket.timeout:
                continue
            except OSError:
                closed = time.monotonic()
        SILENCES.append({'marker': marker, 'started': started, 'closed': closed})

    def do_GET(self):
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.end_headers()
        self.wfile.write(json.dumps({'data': [{'id': 'gpt-5.4-mini'}, {'id': 'claude-sonnet-4-6'}]}).encode())

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        body['_received'] = time.monotonic()
        REQUESTS.append(body)
        history = body.get('input', body.get('messages', []))
        serialized = json.dumps(history)
        # The newest prompt marker in the history names this turn.
        current = max(SILENT, key=serialized.rfind)
        current = current if serialized.rfind(current) > max(serialized.rfind('MAIN_HOLD'), serialized.rfind('AFTER_DISCONNECT')) else None
        key = (current, body.get('model'))
        responses = self.path.endswith('/responses')
        if current == 'SILENT_ALWAYS' or (current == 'SILENT_BEFORE' and key not in STALLED):
            STALLED.add(key)
            return self.stall(current)
        if current == 'SILENT_AFTER' and key not in STALLED:
            STALLED.add(key)
            partial = ([{'type': 'response.output_text.delta', 'delta': 'PARTIAL_BEFORE_SILENCE'}] if responses else
                       [{'type': 'message_start', 'message': {'usage': {'input_tokens': 21, 'output_tokens': 0}}},
                        {'type': 'content_block_start', 'index': 0, 'content_block': {'type': 'text', 'text': ''}},
                        {'type': 'content_block_delta', 'index': 0, 'delta': {'type': 'text_delta', 'text': 'PARTIAL_BEFORE_SILENCE'}}])
            return self.stall(current, partial)
        side = '<btw-side-question>' in serialized
        refusal = 'Tool execution is unavailable' in serialized
        result = 'MCP_COMPLETE' in serialized
        call = ('write' if side and not refusal else
                'mcp__fixture__hold' if not side and 'MAIN_HOLD' in serialized and not result else None)
        arguments = {'path': 'FORBIDDEN_SIDE_WRITE', 'content': 'bad'} if side else {}
        text = 'SIDE_WIRE_ANSWER' if side else 'ANSWER_AFTER_SILENCE' if current else 'MAIN_WIRE_ANSWER'
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.end_headers()
        if self.path.endswith('/responses'):
            events = ([{'type': 'response.output_item.done', 'item': {'type': 'function_call', 'id': 'item-1', 'call_id': 'wire-call', 'name': call, 'arguments': json.dumps(arguments)}}] if call else
                      [{'type': 'response.output_text.delta', 'delta': text}])
            events.append({'type': 'response.completed', 'response': {'usage': {'input_tokens': 21, 'output_tokens': 5}}})
        else:
            events = [{'type': 'message_start', 'message': {'usage': {'input_tokens': 21, 'output_tokens': 0}}}]
            if call:
                events += [{'type': 'content_block_start', 'index': 0, 'content_block': {'type': 'tool_use', 'id': 'wire-call', 'name': call, 'input': {}}},
                           {'type': 'content_block_delta', 'index': 0, 'delta': {'type': 'input_json_delta', 'partial_json': json.dumps(arguments)}},
                           {'type': 'content_block_stop', 'index': 0}]
            else:
                events += [{'type': 'content_block_start', 'index': 0, 'content_block': {'type': 'text', 'text': ''}},
                           {'type': 'content_block_delta', 'index': 0, 'delta': {'type': 'text_delta', 'text': text}},
                           {'type': 'content_block_stop', 'index': 0}]
            events += [{'type': 'message_delta', 'delta': {'stop_reason': 'tool_use' if call else 'end_turn'}, 'usage': {'output_tokens': 5}}, {'type': 'message_stop'}]
        for event in events:
            self.wfile.write(('data: ' + json.dumps(event) + '\n\n').encode())
        self.wfile.flush()


service = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Provider)
threading.Thread(target=service.serve_forever, daemon=True).start()
checks = []
try:
    for provider, model in [('anthropic', 'claude-sonnet-4-6'), ('openai', 'gpt-5.4-mini')]:
        with tempfile.TemporaryDirectory(prefix='haarness-port-proof-') as temporary:
            root = Path(temporary).resolve()
            home, workspace = root / 'home', root / 'workspace'
            (home / '.local/bin').mkdir(parents=True)
            (workspace / '.haarness').mkdir(parents=True)
            (workspace / 'AGENTS.md').write_text('PROJECT_WIRE_MARKER: explain fixture decisions briefly.\n')
            # The mock is on 127.0.0.1, where providers wait as long as it takes
            # unless configured: tune the built-in provider to a short limit.
            (workspace / '.haarness/config.toml').write_text(f'[defaults]\nprovider="{provider}"\nmodel="{model}"\n[permissions]\n"*"="deny"\n"mcp__fixture__*"="allow"\n[lazy_tools]\nmode="always"\n[lsp]\nenabled=false\n[providers.{provider}]\nstream_idle_timeout={IDLE_LIMIT}\n')
            run(['git', 'init', '-q', str(workspace)])
            launcher = home / '.local/bin/haarness-fixture-mcp'
            launcher.write_text('#!' + sys.executable + '\n' + '''import json,os,sys,time
from pathlib import Path
root=Path(os.environ['FIXTURE_ROOT'])
for line in sys.stdin:
    request=json.loads(line)
    if 'id' not in request: continue
    method=request.get('method')
    if method=='initialize':
        result={'protocolVersion':'2024-11-05','capabilities':{'tools':{}},'serverInfo':{'name':'fixture','version':'1'}}
    elif method=='tools/list':
        result={'tools':[{'name':name,'description':name,'inputSchema':{'type':'object'}} for name in ['hold','ping','crash']]}
    elif method=='tools/call':
        name=request['params']['name']
        with (root/'calls').open('a') as log: log.write(name+'\\n')
        if name=='crash': sys.exit(0)
        if name=='hold':
            (root/'entered').write_text('yes')
            while not (root/'release').exists(): time.sleep(.02)
        result={'content':[{'type':'text','text':'MCP_COMPLETE'}]}
    else: result={}
    print(json.dumps({'jsonrpc':'2.0','id':request['id'],'result':result}),flush=True)
''')
            launcher.chmod(0o755)
            env = {**ISOLATED, 'PATH': '/usr/bin:/bin:/usr/sbin:/sbin', 'HOME': str(home), 'HAARNESS_HOME': str(home),
                   'HAARNESS_DATA_DIR': str(root / 'data'), 'USER': os.environ.get('USER', 'fixture'), 'LANG': 'en_US.UTF-8',
                   'OPENAI_API_KEY': 'fixture-key', 'ANTHROPIC_API_KEY': 'fixture-key',
                   'OPENAI_BASE_URL': f'http://127.0.0.1:{service.server_port}/v1',
                   'ANTHROPIC_BASE_URL': f'http://127.0.0.1:{service.server_port}/v1'}
            args = [str(BINARY), '--workspace', str(workspace), '--trust-project']
            connection = None
            session = 'haarness-port-' + str(uuid.uuid4())[:8]
            try:
                connection = json.loads(run(args + ['desktop'], input='{"action":"connect"}', env=env, timeout=90))
                def api(path, body=None, method=None):
                    request = urllib.request.Request(connection['url'] + '/v1' + path,
                        data=None if body is None else json.dumps(body).encode(), method=method,
                        headers={'Authorization': 'Bearer ' + connection['token'], 'Content-Type': 'application/json'})
                    with urllib.request.urlopen(request, timeout=30) as response:
                        raw = response.read()
                        return json.loads(raw) if raw else None
                config = {'transport': 'stdio', 'command': 'haarness-fixture-mcp', 'env': {'FIXTURE_ROOT': str(root)}, 'enabled': True, 'timeout_ms': 2000}
                assert api('/mcp/fixture', config)['status'] == 'connected'
                thread = api('/threads', {'title': 'Wire parity fixture', 'provider': provider, 'model': model})['id']
                def items(): return api(f'/threads/{thread}/items')
                def start(text): return api(f'/threads/{thread}/turns', {'requestId': str(uuid.uuid4()), 'text': text})
                offset = len(REQUESTS)
                start('MAIN_HOLD')
                until(lambda: (root / 'entered').exists(), 'MCP hold did not execute through the denied-loader fallback')
                before = items()
                answer = api(f'/threads/{thread}/btw', {'question': 'SIDE_WIRE: explain the plan without changing anything'})
                assert answer['answer'] == 'SIDE_WIRE_ANSWER', answer
                assert items() == before, 'side question changed the durable transcript'
                assert not (workspace / 'FORBIDDEN_SIDE_WRITE').exists()
                assert not api('/permissions'), 'side question produced an approval'
                main, side = REQUESTS[offset:offset+2]
                key = 'instructions' if provider == 'openai' else 'system'
                history = 'input' if provider == 'openai' else 'messages'
                assert main[key] == side[key], 'system bytes drifted'
                assert main['tools'] == side['tools'], 'schema bytes drifted'
                assert 'PROJECT_WIRE_MARKER' in json.dumps(side[key]), 'project instructions missing'
                assert clean(main[history]) == clean(side[history][:-1]), 'side snapshot included unfinished output'
                assert 'load_tool' not in json.dumps(main['tools']), 'denied loader still advertised'
                assert 'mcp__fixture__hold' in json.dumps(main['tools']), 'allowed deferred tool missing'
                assert (root / 'calls').read_text() == 'hold\n', 'side question executed a tool'
                (root / 'release').write_text('yes')
                until(lambda: not api('/turns/active'), 'main turn did not complete')
                assert any(i['data'].get('text') == 'MAIN_WIRE_ANSWER' for i in items())
                assert not any(i['data']['type'] == 'error' for i in items())
                before = items()
                output = run(args + ['btw', thread, 'SIDE_CLI'], env=env, timeout=30)
                assert 'SIDE_WIRE_ANSWER' in output and items() == before
                # An actual TUI client asks the same server without leaving a transcript row.
                if provider == 'anthropic':
                    command = shlex.join(['env', *[f'{key}={value}' for key,value in env.items()], *args, '--thread', thread])
                    run(['tmux', 'new-session', '-d', '-x', '150', '-y', '42', '-s', session, command])
                    def pane(): return run(['tmux', 'capture-pane', '-p', '-t', session])
                    until(lambda: 'review' in pane().lower(), 'TUI failed to start')
                    run(['tmux', 'send-keys', '-l', '-t', session, '/btw SIDE_TUI'])
                    run(['tmux', 'send-keys', '-t', session, 'Enter'])
                    until(lambda: 'SIDE_WIRE_ANSWER' in pane(), 'TUI side answer missing')
                    (EVIDENCE / 'btw-tui.txt').write_text(pane())
                    assert items() == before, 'TUI side question changed transcript'
                    run(['tmux', 'kill-session', '-t', session])
                try:
                    api('/mcp/fixture/tools/crash', {'arguments': {}})
                    raise AssertionError('crash call unexpectedly completed')
                except urllib.error.HTTPError:
                    pass
                until(lambda: api('/mcp')[0]['status'] == 'failed', 'transport loss was not observed')
                assert api('/mcp/fixture/tools') == [], 'live MCP API presented disconnected tools as connected'
                offset = len(REQUESTS)
                start('CACHE_AFTER_FAILURE')
                until(lambda: len(REQUESTS) > offset, 'reconnect request missing')
                assert REQUESTS[offset]['tools'] == main['tools'], 'transient reconnect changed advertised schemas'
                until(lambda: not api('/turns/active'), 'cache check stayed active')
                until(lambda: api('/mcp')[0]['status'] == 'connected', 'MCP failed to recover')
                assert (root / 'calls').read_text() == 'hold\ncrash\n', 'crash was replayed'
                api('/mcp/fixture/disconnect', {})
                offset = len(REQUESTS)
                start('AFTER_DISCONNECT')
                until(lambda: len(REQUESTS) > offset, 'disconnected request missing')
                assert REQUESTS[offset]['tools'] == [], 'explicit disconnect retained schemas'
                until(lambda: not api('/turns/active'), 'disconnect check stayed active')
                # A provider that answers nothing: its request is closed when the
                # limit passes, before the backoff, and the retry is answered.
                def silent_turn(marker, timeout):
                    offset, silences = len(REQUESTS), len(SILENCES)
                    seen = len(items())
                    start(marker)
                    until(lambda: not api('/turns/active'), f'{marker} turn stayed active', timeout=timeout)
                    return REQUESTS[offset:], SILENCES[silences:], items()[seen:]
                def texts(rows): return [row['data']['text'] for row in rows if row['data']['type'] == 'assistant_message']
                def usage(rows): return [row['data'].get('estimated', False) for row in rows if row['data']['type'] == 'usage']
                requests, silences, rows = silent_turn('SILENT_BEFORE', 60)
                assert len(requests) == 2 and len(silences) == 1, (len(requests), silences)
                closed = silences[0]['closed']
                assert closed is not None, 'the silent request was never closed'
                assert closed - silences[0]['started'] >= IDLE_LIMIT - .2, 'closed before the idle limit'
                assert requests[1]['_received'] - closed >= 1.5, 'closed only when the retry replaced it, not before the backoff'
                assert texts(rows) == ['ANSWER_AFTER_SILENCE'], texts(rows)
                assert usage(rows) == [False], usage(rows)
                assert not any(row['data']['type'] == 'error' for row in rows)
                requests, silences, rows = silent_turn('SILENT_AFTER', 60)
                assert len(requests) == 2 and len(silences) == 1, (len(requests), silences)
                closed = silences[0]['closed']
                assert closed is not None, 'the silent request was never closed'
                # Three seconds of comments, then the limit: comments count as activity.
                assert closed - silences[0]['started'] >= 3 + IDLE_LIMIT - .5, closed - silences[0]['started']
                assert requests[1]['_received'] - closed >= .2, 'closed after the recovery pause, not before it'
                assert 'The previous provider response ended before completion' in json.dumps(requests[1]), 'no continuation'
                assert texts(rows) == ['PARTIAL_BEFORE_SILENCE', 'ANSWER_AFTER_SILENCE'], texts(rows)
                assert len(usage(rows)) == 2, usage(rows)
                assert not any(row['data']['type'] == 'error' for row in rows)
                silence_checks = ['silence before output retried', 'silence after output continued', 'keep-alive comments count']
                if provider == 'anthropic':
                    # Every attempt silent: five requests, then the turn says so.
                    requests, silences, rows = silent_turn('SILENT_ALWAYS', 120)
                    assert len(requests) == 5 and len(silences) == 5, (len(requests), len(silences))
                    assert all(silence['closed'] is not None for silence in silences)
                    errors = [row['data']['message'] for row in rows if row['data']['type'] == 'error']
                    assert errors and 'sent nothing for 2 seconds, so the request was stopped' in errors[-1], errors
                    assert 'reset' not in errors[-1].lower(), errors
                    silence_checks.append('silent provider named after retries')
                checks.append({'provider': provider, 'checks': ['launcher discovery', 'denied loader', 'wire system and schemas', 'settled tool snapshot', 'read-only refusal', 'CLI side answer', 'MCP reconnect without replay', 'explicit disconnect invalidation', *silence_checks]})
                (EVIDENCE / f'{provider}-requests.json').write_text(json.dumps(REQUESTS, indent=2))
                (EVIDENCE / f'{provider}-items.json').write_text(json.dumps(items(), indent=2))
                print('PASS', provider, 'server/provider/MCP/CLI flow', flush=True)
            finally:
                subprocess.run(['tmux', 'kill-session', '-t', session], capture_output=True)
                if connection:
                    subprocess.run(args + ['serve', '--stop'], env=env, capture_output=True, timeout=30)
    (EVIDENCE / 'results.json').write_text(json.dumps(checks, indent=2))
finally:
    service.shutdown()
