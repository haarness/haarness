#!/usr/bin/env python3
"""Real daemon + TUI proof that safety stops are explained and that the prompt prefix stays stable.

A mock Anthropic Messages server records every request body. Against it, a debug `haarness`
daemon in a private data folder shows that:

1. a `refusal` stop with no text fails the turn (not a blank reply), is not retried, reaches
   `StopFailure` as `content_filter`, is logged with the provider's request id, and the TUI
   shows the new message; a refusal after some text keeps that text; a side question that
   meets a refusal answers HTTP 502 with code `provider_blocked`;
2. with a `UserPromptSubmit` hook that adds a timestamp, two turns send a byte-identical
   `system` and the second request starts with the first one's messages; each stamp is in
   the messages, not the system prompt;
3. a chat that began days ago (its `created_at` edited in the store) is told today's date
   once, and the next turn replays that reminder instead of adding another.

Usage: python3 scripts/test-prompt-stability.py   (HAARNESS_BIN picks the binary,
HAARNESS_E2E_ROOT a folder to keep the fixture in, HAARNESS_E2E_EVIDENCE the evidence folder).
"""
import datetime
import http.server
import json
import os
from pathlib import Path
import shlex
import sqlite3
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
EVIDENCE = Path(os.environ.get('HAARNESS_E2E_EVIDENCE', REPO / 'target/prompt-stability/e2e'))
EVIDENCE.mkdir(parents=True, exist_ok=True)
MODEL = 'claude-sonnet-4-6'
REQUESTS = []


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
    """Drop `cache_control`: its breakpoints move to the newest messages by design."""
    if isinstance(value, list):
        return [clean(item) for item in value]
    if isinstance(value, dict):
        return {key: clean(item) for key, item in value.items() if key != 'cache_control'}
    return value


def texts(body):
    return [block.get('text', '') for message in body['messages'] for block in message['content']
            if isinstance(message['content'], list)]


class Anthropic(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.end_headers()
        self.wfile.write(json.dumps({'data': [{'id': MODEL, 'display_name': 'Fixture Sonnet'}]}).encode())

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        REQUESTS.append(body)
        prompt = ' '.join(texts(body)[-4:])
        refuse = 'REFUSE' in prompt
        partial = 'REFUSE_PARTIAL' in prompt
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.send_header('request-id', f'req_e2e_{len(REQUESTS)}')
        self.end_headers()
        events = [{'type': 'message_start', 'message': {'id': f'msg_e2e_{len(REQUESTS)}', 'usage': {'input_tokens': 40, 'output_tokens': 0}}}]
        text = 'PARTIAL_BEFORE_REFUSAL' if partial else None if refuse else 'STABLE_ANSWER'
        if text:
            events += [{'type': 'content_block_start', 'index': 0, 'content_block': {'type': 'text', 'text': ''}},
                       {'type': 'content_block_delta', 'index': 0, 'delta': {'type': 'text_delta', 'text': text}},
                       {'type': 'content_block_stop', 'index': 0}]
        events += [{'type': 'message_delta', 'delta': {'stop_reason': 'refusal' if refuse else 'end_turn', 'stop_sequence': None},
                    'usage': {'output_tokens': 3}},
                   {'type': 'message_stop'}]
        for event in events:
            self.wfile.write(('event: ' + event['type'] + '\ndata: ' + json.dumps(event) + '\n\n').encode())
        self.wfile.flush()


service = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Anthropic)
threading.Thread(target=service.serve_forever, daemon=True).start()
checks = []
keep = os.environ.get('HAARNESS_E2E_ROOT')
holder = None if keep else tempfile.TemporaryDirectory(prefix='haarness-prompt-stability-')
root = Path(keep or holder.name).resolve()
root.mkdir(parents=True, exist_ok=True)
home, workspace, data = root / 'home', root / 'workspace', root / 'data'
session = 'haarness-stability-' + str(uuid.uuid4())[:8]
env = {'PATH': '/usr/bin:/bin:/usr/sbin:/sbin', 'HOME': str(home), 'HAARNESS_HOME': str(home),
       'HAARNESS_DATA_DIR': str(data), 'USER': os.environ.get('USER', 'fixture'), 'LANG': 'en_US.UTF-8',
       'ANTHROPIC_API_KEY': 'fixture-key', 'ANTHROPIC_BASE_URL': f'http://127.0.0.1:{service.server_port}/v1'}
args = [str(BINARY), '--workspace', str(workspace), '--trust-project']
connection = None
try:
    (home / '.local/bin').mkdir(parents=True, exist_ok=True)
    (workspace / '.haarness').mkdir(parents=True, exist_ok=True)
    (workspace / '.haarness/config.toml').write_text(
        f'[defaults]\nprovider="anthropic"\nmodel="{MODEL}"\n[permissions]\n"*"="deny"\n[lsp]\nenabled=false\n')
    stamp = f"{sys.executable} -c \"import json,time; print(json.dumps({{'additionalContext': 'HOOK_STAMP %d' % time.time_ns()}}))\""
    (workspace / 'haarness.json').write_text(json.dumps({'hooks': {
        'UserPromptSubmit': [{'hooks': [{'type': 'command', 'command': stamp}]}],
        'StopFailure': [{'matcher': 'content_filter', 'hooks': [{'type': 'command', 'command': f"cat >> '{root / 'stop-failure.jsonl'}'; echo >> '{root / 'stop-failure.jsonl'}'"}]}],
    }}))
    run(['git', 'init', '-q', str(workspace)])
    connection = json.loads(run(args + ['desktop'], input='{"action":"connect"}', env=env, timeout=120))

    def api(path, body=None, method=None):
        request = urllib.request.Request(connection['url'] + '/v1' + path,
                                         data=None if body is None else json.dumps(body).encode(), method=method,
                                         headers={'Authorization': 'Bearer ' + connection['token'], 'Content-Type': 'application/json'})
        with urllib.request.urlopen(request, timeout=30) as response:
            raw = response.read()
            return json.loads(raw) if raw else None

    def chat(title):
        return api('/threads', {'title': title, 'provider': 'anthropic', 'model': MODEL})['id']

    def finished(identifier):
        value = api(f'/turns/{identifier}')
        return value if value['status'] not in ('queued', 'running') else None

    def turn(thread, text):
        identifier = api(f'/threads/{thread}/turns', {'requestId': str(uuid.uuid4()), 'text': text})['id']
        return until(lambda: finished(identifier), f'turn {text!r} did not finish')

    def items(thread):
        return api(f'/threads/{thread}/items')

    # 1. Safety stops.
    refused = chat('Refusal fixture')
    command = shlex.join(['env', *[f'{key}={value}' for key, value in env.items()], *args, '--thread', refused])
    run(['tmux', 'new-session', '-d', '-x', '150', '-y', '42', '-s', session, command])

    def pane():
        return run(['tmux', 'capture-pane', '-p', '-J', '-t', session])
    until(lambda: 'review' in pane().lower(), 'TUI failed to start', timeout=60)
    offset = len(REQUESTS)
    run(['tmux', 'send-keys', '-l', '-t', session, 'REFUSE_EMPTY please answer'])
    run(['tmux', 'send-keys', '-t', session, 'Enter'])
    until(lambda: 'safety check stopped' in pane(), 'TUI did not explain the refusal')
    until(lambda: not api('/turns/active'), 'refused turn stayed active')
    (EVIDENCE / 'refusal-tui.txt').write_text(pane())
    assert len(REQUESTS) - offset == 1, f'a refusal was retried: {len(REQUESTS) - offset} requests'
    run(['tmux', 'kill-session', '-t', session])
    transcript = items(refused)
    errors = [item for item in transcript if item['data']['type'] == 'error']
    assert len(errors) == 1, errors
    assert finished(errors[0]['turnId'])['status'] == 'failed', 'the refused turn did not fail'
    errors = [item['data'] for item in errors]
    assert not errors[0]['retryable'], errors
    assert errors[0]['message'].startswith("The provider's safety check stopped this response (refusal)."), errors
    assert 'rephrase the request or choose another model' in errors[0]['message'], errors
    assert not any(item['data']['type'] == 'assistant_message' for item in transcript), 'a refusal became a blank reply'
    stop = until(lambda: (root / 'stop-failure.jsonl').exists() and (root / 'stop-failure.jsonl').read_text().strip(), 'StopFailure hook did not run')
    stop = json.loads(stop.splitlines()[0])
    assert stop['error'] == 'content_filter', stop
    logs = until(lambda: [path for path in root.rglob('*.log*') if 'provider safety check stopped the response' in path.read_text(errors='ignore')],
                 'no log line for the refusal')
    line = next(line for line in logs[0].read_text(errors='ignore').splitlines() if 'provider safety check stopped the response' in line)
    assert 'refusal' in line and f'req_e2e_{offset + 1}' in line and f'msg_e2e_{offset + 1}' in line, line
    (EVIDENCE / 'refusal-log-line.txt').write_text(line + '\n')

    partial = chat('Partial refusal fixture')
    assert turn(partial, 'REFUSE_PARTIAL please answer')['status'] == 'failed'

    transcript = items(partial)
    assert any(item['data'].get('text') == 'PARTIAL_BEFORE_REFUSAL' for item in transcript if item['data']['type'] == 'assistant_message'), transcript
    assert any(item['data']['type'] == 'error' and 'safety check stopped' in item['data']['message'] for item in transcript), transcript
    try:
        api(f'/threads/{partial}/btw', {'question': 'REFUSE_EMPTY side question'})
        raise AssertionError('a refused side question succeeded')
    except urllib.error.HTTPError as error:
        failure = json.loads(error.read())
        assert error.code == 502 and failure['code'] == 'provider_blocked' and not failure['retryable'], failure
    checks.append({'refusal': ['TUI message', 'one request', 'error item not retryable', 'StopFailure content_filter',
                               'request id logged', 'partial text kept', 'btw 502 provider_blocked']})
    print('PASS refusal stops', flush=True)

    # 2. Hook context stays out of the system prompt.
    stable = chat('Stable prefix fixture')
    offset = len(REQUESTS)
    assert turn(stable, 'FIRST stable turn')['status'] == 'completed'
    assert turn(stable, 'SECOND stable turn')['status'] == 'completed'
    first, second = REQUESTS[offset:offset + 2]
    assert len(REQUESTS) == offset + 2, 'extra requests in the stable chat'
    assert json.dumps(first['system']) == json.dumps(second['system']), 'system prompt changed between turns'
    assert first['tools'] == second['tools'], 'tool schemas changed between turns'
    assert clean(second['messages'][:len(first['messages'])]) == clean(first['messages']), 'an earlier message changed'
    stamps = [text for text in texts(second) if 'HOOK_STAMP' in text]
    assert len(stamps) == 2 and stamps[0] != stamps[1], stamps
    assert all(text.startswith('<system-reminder>\nContext from hooks:\nHOOK_STAMP ') for text in stamps), stamps
    assert 'HOOK_STAMP' not in json.dumps(first['system']), 'hook context reached the system prompt'
    prompts = texts(second)
    assert prompts.index(stamps[0]) == prompts.index('FIRST stable turn') + 1, prompts
    shown = [item for item in items(stable) if item['data']['type'] == 'user_message' and not item['data'].get('synthetic')]
    assert [item['data']['text'] for item in shown] == ['FIRST stable turn', 'SECOND stable turn'], shown
    # The TUI shows the two prompts and answers, not the stored reminders.
    command = shlex.join(['env', *[f'{key}={value}' for key, value in env.items()], *args, '--thread', stable])
    run(['tmux', 'new-session', '-d', '-x', '150', '-y', '42', '-s', session, command])
    until(lambda: 'SECOND stable turn' in pane() and pane().count('STABLE_ANSWER') == 2, 'TUI did not show the stable chat', timeout=60)
    shown = pane()
    (EVIDENCE / 'stable-tui.txt').write_text(shown)
    assert 'HOOK_STAMP' not in shown and 'system-reminder' not in shown, shown
    run(['tmux', 'kill-session', '-t', session])
    (EVIDENCE / 'stable-requests.json').write_text(json.dumps([first, second], indent=2))
    checks.append({'stable prefix': ['system byte-identical', 'tools identical', 'messages prefix identical',
                                     'hook stamp after its prompt', 'reminders hidden from the API transcript and the TUI']})
    print('PASS stable system prompt with per-turn hook context', flush=True)

    # 3. A chat begun days ago is told today's date once.
    old = chat('Old chat fixture')
    began = (datetime.datetime.now(datetime.timezone.utc) - datetime.timedelta(days=5)).replace(microsecond=0)
    database = next(path for path in data.rglob('haarness.db')
                    if sqlite3.connect(path).execute('SELECT 1 FROM threads WHERE id=?', (old,)).fetchone())
    with sqlite3.connect(database) as store:
        store.execute('UPDATE threads SET created_at=? WHERE id=?', (began.isoformat().replace('+00:00', 'Z'), old))
    offset = len(REQUESTS)
    turn(old, 'FIRST old turn')
    turn(old, 'SECOND old turn')
    first, second = REQUESTS[offset:offset + 2]
    today = datetime.date.today().isoformat()
    began_local = began.astimezone().date().isoformat()
    system = json.dumps(first['system'])
    assert f"Today's date: {began_local}" in system, system[:2000]
    assert json.dumps(second['system']) == system, 'system prompt changed'
    told = f"Today's date is now {today}."
    for body in (first, second):
        assert sum(text.count(told) for text in texts(body)) == 1, texts(body)
    assert clean(second['messages'][:len(first['messages'])]) == clean(first['messages']), 'an earlier message changed'
    (EVIDENCE / 'date-requests.json').write_text(json.dumps([first, second], indent=2))
    checks.append({'date reminder': [f'system says {began_local}', f'told {today} once', 'replayed unchanged']})
    print('PASS date reminder for an old chat', flush=True)
    (EVIDENCE / 'results.json').write_text(json.dumps(checks, indent=2))
finally:
    subprocess.run(['tmux', 'kill-session', '-t', session], capture_output=True)
    if connection:
        subprocess.run(args + ['serve', '--stop'], env=env, capture_output=True, timeout=60)
    service.shutdown()
    if holder:
        holder.cleanup()
