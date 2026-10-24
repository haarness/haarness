#!/usr/bin/env python3
"""A real daemon and TUI against a mock Gemini API (the real Gemini adapter, SSE).

Proves, end to end:
- a response Gemini ends with MALFORMED_FUNCTION_CALL and nothing else is retried
  with the continue note, and the turn answers instead of finishing blank;
- a valid call followed by MALFORMED_FUNCTION_CALL runs the call (it reads a real
  file), and the next request carries its result and the continue note;
- a discovered `gemini-4-argon` asked for `minimal` thinks at `thinkingLevel: low`,
  never a budget or `minimal`;
- "User location is not supported" fails the turn once, unretried, with a message
  that names the provider; the TUI shows both the recovery and that message.
"""
import http.server
import json
import os
from pathlib import Path
import shlex
import subprocess
import tempfile
import threading
import time
import urllib.request
import uuid
from e2e_env import ISOLATED

REPO = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get('HAARNESS_BIN', REPO / 'target/debug/haarness')).resolve()
EVIDENCE = Path(os.environ.get('HAARNESS_GEMINI_EVIDENCE', REPO / 'target/gemini-recovery/e2e'))
EVIDENCE.mkdir(parents=True, exist_ok=True)
MODEL = 'gemini-4-argon'
NOTE = 'ended before completion'
REQUESTS = []


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


def texts(body):
    return [part.get('text', '') for content in body.get('contents', [])
            for part in content.get('parts', [])]


def function_responses(body):
    return [part['functionResponse'] for content in body.get('contents', [])
            for part in content.get('parts', []) if 'functionResponse' in part]


def current_turn(body):
    """This turn's marker (an upper-case word such as MALFORMED_ALONE) and the
    contents from the prompt that carried it on, earlier turns left out."""
    contents = body.get('contents', [])
    for index in range(len(contents) - 1, -1, -1):
        for part in contents[index].get('parts', []):
            words = [word for word in part.get('text', '').split() if word.isupper() and '_' in word]
            if contents[index].get('role') == 'user' and words:
                return words[0], {'contents': contents[index:]}
    raise AssertionError(f'no marker in {contents}')


def chunk(parts=None, finish=None):
    candidate = {'content': {'role': 'model', **({'parts': parts} if parts else {})}}
    if finish:
        candidate['finishReason'] = finish
    event = {'responseId': 'fixture', 'candidates': [candidate]}
    if finish:
        event['usageMetadata'] = {'promptTokenCount': 40, 'candidatesTokenCount': 5}
    return event


class Gemini(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def reply(self, status, body, content_type='application/json'):
        self.send_response(status)
        self.send_header('Content-Type', content_type)
        self.end_headers()
        self.wfile.write(body.encode())

    def do_GET(self):
        self.reply(200, json.dumps({'models': [{
            'name': f'models/{MODEL}', 'displayName': 'Gemini 4 Argon', 'thinking': True,
            'inputTokenLimit': 1048576, 'outputTokenLimit': 65536,
            'supportedGenerationMethods': ['generateContent'],
        }]}))

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        REQUESTS.append({'path': self.path, 'body': body})
        marker, since = current_turn(body)
        asked = any(NOTE in text for text in texts(since))
        if marker.startswith('REGION'):
            self.reply(400, json.dumps({'error': {
                'code': 400, 'message': 'User location is not supported for the API use.',
                'status': 'FAILED_PRECONDITION'}}))
            return
        if marker.startswith('AFTER_CALL') and not function_responses(since):
            events = [chunk([{'functionCall': {'name': 'read', 'args': {'path': 'sample.txt'}},
                              'thoughtSignature': 'fixture-signature'}]),
                      chunk(finish='MALFORMED_FUNCTION_CALL')]
        elif marker.startswith('MALFORMED') and not asked:
            events = [chunk(finish='MALFORMED_FUNCTION_CALL')]
        else:
            events = [chunk([{'text': f'RECOVERED {marker}'}], 'STOP')]
        self.reply(200, ''.join('data: ' + json.dumps(event) + '\n\n' for event in events),
                   'text/event-stream')


service = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Gemini)
threading.Thread(target=service.serve_forever, daemon=True).start()
session = 'haarness-gemini-' + str(uuid.uuid4())[:8]
with tempfile.TemporaryDirectory(prefix='haarness-gemini-recovery-',
                                 dir=os.environ.get('HAARNESS_E2E_TMP')) as temporary:
    root = Path(temporary).resolve()
    home, workspace = root / 'home', root / 'workspace'
    home.mkdir()
    (workspace / '.haarness').mkdir(parents=True)
    (workspace / 'sample.txt').write_text('GEMINI_FILE_PROOF\n')
    (workspace / '.haarness/config.toml').write_text(
        f'[defaults]\nprovider="gemini"\nmodel="{MODEL}"\n[permissions]\nread="allow"\n[lsp]\nenabled=false\n')
    run(['git', 'init', '-q', str(workspace)])
    env = {**ISOLATED, 'PATH': '/usr/bin:/bin:/usr/sbin:/sbin', 'HOME': str(home), 'HAARNESS_HOME': str(home),
           'HAARNESS_DATA_DIR': str(root / 'data'), 'USER': os.environ.get('USER', 'fixture'),
           'LANG': 'en_US.UTF-8', 'TERM': 'xterm-256color', 'GEMINI_API_KEY': 'fixture-key',
           'GEMINI_BASE_URL': f'http://127.0.0.1:{service.server_port}'}
    args = [str(BINARY), '--workspace', str(workspace), '--trust-project']
    connection = None
    try:
        connection = json.loads(run(args + ['desktop'], input='{"action":"connect"}', env=env, timeout=120))

        def api(path, body=None):
            request = urllib.request.Request(connection['url'] + '/v1' + path,
                data=None if body is None else json.dumps(body).encode(),
                headers={'Authorization': 'Bearer ' + connection['token'], 'Content-Type': 'application/json'})
            with urllib.request.urlopen(request, timeout=30) as response:
                raw = response.read()
                return json.loads(raw) if raw else None

        gemini = until(lambda: next((p for p in api('/providers') if p['id'] == 'gemini'
                                     and any(m['id'] == MODEL for m in p['models'])), None),
                       'mock Gemini catalog was not discovered')
        argon = next(m for m in gemini['models'] if m['id'] == MODEL)
        assert argon['reasoningEfforts'] == ['low', 'medium', 'high'], argon
        thread = api('/threads', {'title': 'Gemini recovery', 'provider': 'gemini', 'model': MODEL})['id']

        def items():
            return api(f'/threads/{thread}/items')

        def turn(text, effort=None):
            offset = len(REQUESTS)
            api(f'/threads/{thread}/turns', {'requestId': str(uuid.uuid4()), 'text': text,
                                             'reasoningEffort': effort})
            until(lambda: len(REQUESTS) > offset, f'{text} reached no provider')
            until(lambda: not api('/turns/active'), f'{text} stayed active')
            return REQUESTS[offset:]

        def answered(marker):
            return any(i['data']['type'] == 'assistant_message' and i['data']['text'] == f'RECOVERED {marker}'
                       for i in items())

        # 1. A dropped call and nothing else: one retry with the note, then an answer.
        sent = turn('MALFORMED_ALONE please', 'minimal')
        assert len(sent) == 2, [r['body'] for r in sent]
        assert not any(NOTE in t for t in texts(sent[0]['body']))
        assert NOTE in texts(sent[1]['body'])[-1], texts(sent[1]['body'])
        assert answered('MALFORMED_ALONE'), items()
        for request in sent:
            assert f'/models/{MODEL}:streamGenerateContent' in request['path'], request['path']
            assert request['body']['generationConfig']['thinkingConfig'] == {
                'thinkingLevel': 'low', 'includeThoughts': True}, request['body']['generationConfig']
        assert not any(i['data']['type'] == 'error' for i in items())

        # 2. A valid call, then a dropped one: the read runs, then the model continues.
        sent = turn('AFTER_CALL_MALFORMED read sample.txt')
        assert len(sent) == 2, [r['body'] for r in sent]
        responses = function_responses(sent[1]['body'])
        assert responses and 'GEMINI_FILE_PROOF' in json.dumps(responses[-1]), responses
        assert NOTE in texts(sent[1]['body'])[-1], texts(sent[1]['body'])
        assert any(i['data']['type'] == 'tool_result' and not i['data']['is_error']
                   and 'GEMINI_FILE_PROOF' in i['data']['output'] for i in items()), items()
        assert answered('AFTER_CALL_MALFORMED'), items()
        assert not any(i['data']['type'] == 'error' for i in items())

        # 3. The same recovery driven from the TUI.
        command = shlex.join(['env', *[f'{key}={value}' for key, value in env.items()], *args, '--thread', thread])
        run(['tmux', 'new-session', '-d', '-x', '160', '-y', '45', '-s', session, command])

        def pane():
            return run(['tmux', 'capture-pane', '-p', '-t', session])

        until(lambda: 'review' in pane().lower(), 'TUI did not open')
        offset = len(REQUESTS)
        run(['tmux', 'send-keys', '-l', '-t', session, ' MALFORMED_TUI please'])
        run(['tmux', 'send-keys', '-t', session, 'Enter'])
        until(lambda: 'RECOVERED MALFORMED_TUI' in pane(), 'TUI did not show the recovered answer')
        until(lambda: not api('/turns/active'), 'TUI turn stayed active')
        assert len(REQUESTS) - offset == 2, REQUESTS[offset:]
        recovered = pane()

        # 4. A region refusal: one request, no retry, a message naming the provider.
        offset = len(REQUESTS)
        run(['tmux', 'send-keys', '-l', '-t', session, ' REGION_CHECK please'])
        run(['tmux', 'send-keys', '-t', session, 'Enter'])
        until(lambda: 'notavailableinyourcountryorregion' in ''.join(pane().split()),
              'TUI did not show the region message')
        until(lambda: not api('/turns/active'), 'region turn stayed active')
        time.sleep(1)
        assert len(REQUESTS) - offset == 1, f'region refusal was retried: {len(REQUESTS) - offset} requests'
        errors = [i['data']['message'] for i in items() if i['data']['type'] == 'error']
        assert errors and errors[-1] == ('Google Gemini is not available in your country or region. '
                                         'Choose a model from another provider.'), errors
        region = pane()
        (EVIDENCE / 'tui.txt').write_text('RECOVERY\n' + recovered + '\nREGION\n' + region)
        (EVIDENCE / 'requests.json').write_text(json.dumps(REQUESTS, indent=2))
        (EVIDENCE / 'items.json').write_text(json.dumps(items(), indent=2))
        print('PASS: Gemini MALFORMED_FUNCTION_CALL recovers (alone and after a valid call, API and TUI), '
              'gemini-4-argon thinks at a level, a region refusal is explained once; evidence:', EVIDENCE)
    finally:
        subprocess.run(['tmux', 'kill-session', '-t', session], capture_output=True)
        if connection:
            subprocess.run(args + ['serve', '--stop'], env=env, capture_output=True, timeout=30)
        service.shutdown()
