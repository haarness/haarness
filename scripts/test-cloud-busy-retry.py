#!/usr/bin/env python3
"""Real server against a Haarness Cloud stand-in: a busy key or router is waited out.

Cloud refuses a request with its own 429 when the sign-in already runs as many requests at once as
its key allows (`concurrency_limited`, eight by default, which a fan-out of agents can pass), when
the key's requests a minute are used up (`rate_limited`), or when the router itself is full
(`router_overloaded`). Cloud before this change sent no `Retry-After`, and every such refusal
failed the turn at once. The stand-in answers each chat request with the refusals a scenario
lists, exactly as the router words them, then with an answer.

Usage: HAARNESS_BIN=target/debug/haarness python3 scripts/test-cloud-busy-retry.py
"""
import datetime
import http.server
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import time
import urllib.request
import uuid

import e2e_scratch
from e2e_env import clean_env

REPO = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get('HAARNESS_BIN', REPO / 'target/debug/haarness')).resolve()
MODEL = 'fixture/chat'
MESSAGES = {
    'concurrency_limited': 'per-key concurrency limit reached',
    'rate_limited': 'per-key rate limit of 300 requests per minute reached',
    'router_overloaded': 'router concurrency limit reached',
    'exposure_cap': 'too many unsettled requests for this account (cap 5000000000 nanos)',
}
# Per scenario prompt: the refusals still to send ((code, Retry-After or None), …) and when each
# chat request arrived.
plans, arrivals = {}, {}


class Cloud(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def reply(self, status, value, headers=()):
        body = json.dumps(value).encode()
        self.send_response(status)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(body)))
        for name, value in headers:
            self.send_header(name, value)
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        self.reply(200, {'object': 'list', 'data': [{'id': MODEL, 'object': 'model', 'context_length': 128000}]})

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        if self.path.endswith('/router-session'):
            expires = datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(hours=1)
            return self.reply(200, {'token': 'fixture-router', 'routerUrl': URL + '/', 'expiresAt': expires.isoformat()})
        prompt = next((message['content'] for message in reversed(body['messages'])
                       if message['role'] == 'user' and isinstance(message['content'], str)), '')
        scenario = next((name for name in plans if prompt.startswith(name + ':')), None)
        if scenario is None or not body.get('tools'):
            # A title request (it quotes the message, without tools), or another the scenarios do not count.
            return self.answer('Fixture title')
        arrivals[scenario].append(time.monotonic())
        if plans[scenario]:
            code, wait = plans[scenario].pop(0)
            envelope = {'error': {'message': MESSAGES[code], 'type': 'rate_limit_error', 'code': code}}
            return self.reply(429, envelope, [('Retry-After', str(wait))] if wait else [])
        self.answer('BUSY_CLEARED')

    def answer(self, text):
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.end_headers()
        for event in [{'choices': [{'delta': {'content': text}, 'finish_reason': None}]},
                      {'choices': [{'delta': {}, 'finish_reason': 'stop'}], 'usage': {'prompt_tokens': 9, 'completion_tokens': 2}}]:
            self.wfile.write(('data: ' + json.dumps(event) + '\n\n').encode())
        self.wfile.write(b'data: [DONE]\n\n')
        self.wfile.flush()


def run(args, **kwargs):
    return subprocess.run(args, check=True, capture_output=True, text=True, **kwargs).stdout


def until(check, label, timeout=60):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(.1)
    raise AssertionError(label)


service = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Cloud)
URL = f'http://127.0.0.1:{service.server_port}'
threading.Thread(target=service.serve_forever, daemon=True).start()
root = Path(tempfile.mkdtemp(prefix='haarness-cloud-busy-', dir=os.environ.get('HAARNESS_E2E_DIR'))).resolve()
workspace = root / 'workspace'
(workspace / '.haarness').mkdir(parents=True)
(workspace / '.haarness/config.toml').write_text('[lsp]\nenabled=false\n')
account = root / 'account'
account.mkdir()
credentials = account / 'credentials.json'
credentials.write_text(json.dumps({
    'apiUrl': URL + '/', 'accessToken': 'fixture-account', 'refreshToken': 'fixture-refresh',
    'accessExpiresAt': (datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(hours=1)).isoformat()}))
credentials.chmod(0o600)
env = clean_env()
env.update(HAARNESS_DATA_DIR=str(root / 'data'), HAARNESS_HOME=str(root / 'home'), HOME=str(root / 'home'),
           HAARNESS_CLOUD_API_URL=URL, HAARNESS_CLOUD_CONFIG_DIR=str(account))
(root / 'home').mkdir()
args = [str(BINARY), '--workspace', str(workspace), '--trust-project']
connection = None
passed = 0
try:
    connection = json.loads(run(args + ['desktop'], env=env, input='{"action":"connect"}', timeout=120))

    def api(path, body=None):
        request = urllib.request.Request(connection['url'] + '/v1' + path, data=None if body is None else json.dumps(body).encode(),
                                         headers={'Authorization': 'Bearer ' + connection['token'], 'Content-Type': 'application/json'})
        with urllib.request.urlopen(request, timeout=30) as response:
            data = response.read()
            return json.loads(data) if data else None

    def scenario(name, refusals, timeout=60):
        """Starts a chat whose request is refused as `refusals` says; the finished turn, its chat and the gaps
        between the chat's requests."""
        plans[name], arrivals[name] = list(refusals), []
        thread = api('/threads', {'title': name, 'provider': 'haarness', 'model': MODEL})['id']
        started = api(f'/threads/{thread}/turns', {'requestId': str(uuid.uuid4()), 'text': name + ': say hello'})
        turn = until(lambda: (lambda now: now if now['status'] not in ('queued', 'running') else None)(
            api(f"/turns/{started['id']}")), f'{name}: the turn did not end', timeout)
        times = arrivals[name]
        return turn, thread, [later - earlier for earlier, later in zip(times, times[1:])]

    def items(thread, kind):
        return [item['data'] for item in api(f'/threads/{thread}/items') if item['data']['type'] == kind]

    def answered(turn, thread):
        assert turn['status'] == 'completed', (turn, items(thread, 'error'))
        assert items(thread, 'assistant_message')[-1]['text'] == 'BUSY_CLEARED', items(thread, 'assistant_message')

    # Today's Cloud: no Retry-After. The turn backs off (2 s, then 4 s) until a slot frees.
    turn, thread, gaps = scenario('busy_key', [('concurrency_limited', None)] * 2)
    answered(turn, thread)
    assert len(gaps) == 2 and gaps[0] >= 1.5 and gaps[1] >= 3.5, gaps
    print(f'PASS a key already running all its requests at once is waited out (gaps {gaps[0]:.1f} s, {gaps[1]:.1f} s)', flush=True)
    passed += 1

    # Cloud now says 5 s for a busy key, for clients that only follow Retry-After; Haarness backs off on its own
    # schedule, which tries sooner and lasts longer.
    turn, thread, gaps = scenario('busy_key_retry_after', [('concurrency_limited', 5)])
    answered(turn, thread)
    assert len(gaps) == 1 and 1.5 <= gaps[0] < 4.5, gaps
    print(f'PASS a busy key that names a wait is still backed off from 2 s (gap {gaps[0]:.1f} s)', flush=True)
    passed += 1

    turn, thread, gaps = scenario('busy_router', [('router_overloaded', None)])
    answered(turn, thread)
    assert len(gaps) == 1 and gaps[0] >= 1.5, gaps
    print(f'PASS a full router is waited out (gap {gaps[0]:.1f} s)', flush=True)
    passed += 1

    # The minute's end, as the router names it now: Retry-After 2 waits 3 s.
    turn, thread, gaps = scenario('used_minute', [('rate_limited', 2)])
    answered(turn, thread)
    assert len(gaps) == 1 and gaps[0] >= 2.5, gaps
    print(f'PASS a used-up minute is waited out for as long as the router says (gap {gaps[0]:.1f} s)', flush=True)
    passed += 1

    # Busy for longer than the retries last (about 30 s): the person reads what happened and what helps.
    turn, thread, gaps = scenario('still_busy', [('concurrency_limited', None)] * 6, timeout=90)
    errors = [item['message'] for item in items(thread, 'error')]
    assert turn['status'] == 'failed' and len(gaps) == 4, (turn, gaps, errors)
    assert errors[-1] == ('Haarness Cloud is already running as many of your requests at once as its rate limit allows. '
                          'Try again in a moment, or once another chat or agent finishes.'), errors
    print(f'PASS a key that stays busy ends the turn after {sum(gaps):.0f} s in plain words: {errors[-1]}', flush=True)
    passed += 1
    # The cap on an account's requests in flight, as today's Cloud sends it (no Retry-After): the turn ends at once
    # in plain words, not "Provider error", that fit a request that waits for others and one too costly alone.
    turn, thread, gaps = scenario('in_flight_cap', [('exposure_cap', None)])
    errors = [item['message'] for item in items(thread, 'error')]
    assert turn['status'] == 'failed' and gaps == [], (turn, gaps, errors)
    assert errors[-1] == ("Too many requests are still settling on this Haarness Cloud account, past its usage limit for "
                          "requests in flight. Wait a moment, then try again; if nothing else is running, lower the output "
                          "limit or shorten the conversation."), errors
    print('PASS the cap on requests in flight ends the turn at once in plain words', flush=True)
    passed += 1
    print(f'PASS {passed} Haarness Cloud busy scenarios.', flush=True)
finally:
    if connection:
        subprocess.run(args + ['serve', '--stop'], env=env, capture_output=True, timeout=35)
    service.shutdown()
    e2e_scratch.settle(root)
