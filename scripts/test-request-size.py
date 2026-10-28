#!/usr/bin/env python3
"""A real daemon against a mock Anthropic server that refuses requests over 32 MB.

Proves, end to end: two 15 MiB PDFs a person attaches in consecutive turns no
longer make a 40 MB request the provider refuses: the earlier one is noted and
the one just attached reaches the model, with no compaction. The same holds for
two PDFs the model reads with the read tool, and for the turn after them. A PDF
attached and another read in the same turn still go together, read once. A
provider that keeps refusing the request as too large compacts the chat once,
then the turn fails with one error that says what to do, and the TUI shows it;
a context too long in tokens still compacts up to four times.
"""
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
import e2e_tmux
from e2e_env import ISOLATED

REPO = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get('HAARNESS_BIN', REPO / 'target/debug/haarness')).resolve()
EVIDENCE = Path(os.environ.get('HAARNESS_REQUEST_SIZE_EVIDENCE', REPO / 'target/request-size/e2e'))
EVIDENCE.mkdir(parents=True, exist_ok=True)
REQUESTS = []
MOST_BYTES = 32 * 1024 * 1024
MIB = 1024 * 1024
# A chat holding one of these meets a provider that refuses every request, as
# too large in bytes or as a prompt too long in tokens.
TOO_LARGE = 'ALWAYS_TOO_LARGE'
TOO_LONG = 'ALWAYS_TOO_LONG'
SUMMARY = 'Summarize this conversation for continuation:'
PLAIN = ('This chat is too large for Anthropic to take in one request, even after compacting: '
         'continue in a new chat, or attach smaller files.')
NOTE = 'omitted to keep the request within the provider'
READS = []


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


def pdf(path, mib, pages=3):
    """A PDF large in bytes and small in pages, as a scan is."""
    with open(path, 'wb') as file:
        file.write(b'%PDF-1.7\n')
        for page in range(1, pages + 1):
            file.write(f'{page} 0 obj << /Type /Page >> endobj\n'.encode())
        file.truncate(mib * MIB)


def texts(message):
    content = message.get('content')
    if isinstance(content, str):
        return [content]
    found = []
    for block in content or []:
        if block.get('type') == 'text':
            found.append(block['text'])
        inner = block.get('content') if block.get('type') == 'tool_result' else None
        for item in inner if isinstance(inner, list) else []:
            if item.get('type') == 'text':
                found.append(item['text'])
    return found


def documents(body):
    """The size of every document block in order, nested tool results included."""
    found = []
    for message in body.get('messages', []):
        content = message.get('content')
        for block in content if isinstance(content, list) else []:
            inner = block.get('content') if block.get('type') == 'tool_result' else None
            for item in [block, *(inner if isinstance(inner, list) else [])]:
                if item.get('type') == 'document':
                    found.append(len(item['source']['data']))
    return found


def is_summary(body):
    last = body['messages'][-1] if body.get('messages') else {}
    return last.get('role') == 'user' and any(text.startswith(SUMMARY) for text in texts(last))


class Provider(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.end_headers()
        self.wfile.write(json.dumps({'data': [{'id': 'claude-sonnet-4-6', 'type': 'model'}], 'has_more': False}).encode())

    def refuse(self, status, kind, message):
        data = json.dumps({'type': 'error', 'error': {'type': kind, 'message': message}}).encode()
        self.send_response(status)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def stream(self, blocks, stop):
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.end_headers()
        events = [{'type': 'message_start', 'message': {'usage': {'input_tokens': 21, 'output_tokens': 0}}}]
        for index, (start, delta) in enumerate(blocks):
            events += [{'type': 'content_block_start', 'index': index, 'content_block': start},
                       {'type': 'content_block_delta', 'index': index, 'delta': delta},
                       {'type': 'content_block_stop', 'index': index}]
        events += [{'type': 'message_delta', 'delta': {'stop_reason': stop}, 'usage': {'output_tokens': 5}},
                   {'type': 'message_stop'}]
        for event in events:
            self.wfile.write(('data: ' + json.dumps(event) + '\n\n').encode())
        self.wfile.flush()

    def answer(self, text):
        self.stream([({'type': 'text', 'text': ''}, {'type': 'text_delta', 'text': text})], 'end_turn')

    def do_POST(self):
        length = int(self.headers['Content-Length'])
        body = json.loads(self.rfile.read(length))
        body['bytes'] = length
        summary = is_summary(body)
        body['summary'] = summary
        REQUESTS.append(body)
        if summary:
            return self.answer('SUMMARY_OF_THE_CHAT')
        history = json.dumps(body.get('messages', []))
        if TOO_LARGE in history or length > MOST_BYTES:
            # Anthropic's 413 for a request over its 32 MB.
            return self.refuse(413, 'request_too_large', 'Request exceeds the maximum size')
        if TOO_LONG in history:
            return self.refuse(400, 'invalid_request_error', 'prompt is too long: 210000 tokens > 200000 maximum')
        last = body['messages'][-1]
        wanted = [line[len('read '):] for text in texts(last) for line in text.splitlines() if line.startswith('read ')]
        results = [block for block in last.get('content') or [] if isinstance(block, dict) and block.get('type') == 'tool_result']
        if results and NOTE in json.dumps(results[-1]) and len(READS) < 4:
            # A model told that the file it read was left out reads it again.
            wanted = [READS[-1]]
        elif results or last.get('role') != 'user':
            wanted = []
        if wanted:
            READS.append(wanted[-1])
            return self.stream([({'type': 'tool_use', 'id': 'toolu_' + uuid.uuid4().hex[:12], 'name': 'read', 'input': {}},
                                 {'type': 'input_json_delta', 'partial_json': json.dumps({'path': wanted[-1]})})],
                               'tool_use')
        self.answer(f'ANSWER_WITH_{len(documents(body))}_DOCUMENTS')


service = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Provider)
threading.Thread(target=service.serve_forever, daemon=True).start()
scratch = Path(os.environ.get('HAARNESS_REQUEST_SIZE_SCRATCH', tempfile.gettempdir()))
scratch.mkdir(parents=True, exist_ok=True)
root = Path(tempfile.mkdtemp(prefix='haarness-request-size-', dir=scratch)).resolve()
home, workspace = root / 'home', root / 'workspace'
home.mkdir()
(workspace / '.haarness').mkdir(parents=True)
(workspace / '.haarness/config.toml').write_text('[defaults]\nprovider="anthropic"\nmodel="claude-sonnet-4-6"\n[lsp]\nenabled=false\n')
run(['git', 'init', '-q', str(workspace)])
env = {**ISOLATED, 'PATH': '/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin', 'HOME': str(home), 'HAARNESS_HOME': str(home),
       'HAARNESS_DATA_DIR': str(root / 'data'), 'USER': os.environ.get('USER', 'fixture'),
       'LANG': 'en_US.UTF-8', 'LC_ALL': 'en_US.UTF-8', 'SSH_TTY': 'fixture',
       'ANTHROPIC_API_KEY': 'fixture-key', 'ANTHROPIC_BASE_URL': f'http://127.0.0.1:{service.server_port}'}
args = [str(BINARY), '--workspace', str(workspace), '--trust-project']
session = 'haarness-request-size-' + uuid.uuid4().hex[:8]
socket = 'haarness-request-size'
connection = None
results = []


def connect():
    return json.loads(run(args + ['desktop'], input='{"action":"connect"}', env=env, timeout=120))


def api(path, body=None, method=None):
    request = urllib.request.Request(connection['url'] + '/v1' + path,
                                     data=None if body is None else json.dumps(body).encode(), method=method,
                                     headers={'Authorization': 'Bearer ' + connection['token'], 'Content-Type': 'application/json'})
    with urllib.request.urlopen(request, timeout=60) as response:
        raw = response.read()
        return json.loads(raw) if raw else None


def turn(thread, text, attachments=()):
    offset = len(REQUESTS)
    started = api(f'/threads/{thread}/turns', {'requestId': str(uuid.uuid4()), 'text': text, 'attachments': list(attachments)})
    until(lambda: not api('/turns/active'), f'turn {text!r} stayed active', timeout=180)
    turn_id = started.get('id') or started.get('turn', {}).get('id')
    state = api(f'/turns/{turn_id}') if turn_id else started
    return state, REQUESTS[offset:]


def items_of(thread, kind):
    return [item['data'] for item in api(f'/threads/{thread}/items') if item['data']['type'] == kind]


def new_thread(title):
    return api('/threads', {'title': title, 'provider': 'anthropic', 'model': 'claude-sonnet-4-6'})['id']


try:
    connection = connect()
    first, second = workspace / 'first.pdf', workspace / 'second.pdf'
    pdf(first, 15)
    pdf(second, 15)
    pdf_base64 = (15 * MIB + 2) // 3 * 4

    # 1. A person attaches a 15 MiB PDF in each of two turns. Together their
    # base64 passes 32 MB; the earlier one is noted before the request goes.
    attached = new_thread('Two attached PDFs')
    state, sent = turn(attached, 'Summarise this report', [str(first)])
    assert state['status'] == 'completed', state
    assert len(sent) == 1 and documents(sent[0]) == [pdf_base64], [documents(body) for body in sent]
    state, sent = turn(attached, 'And compare it with this one', [str(second)])
    assert state['status'] == 'completed', state
    assert len(sent) == 1, f'{len(sent)} requests; one was refused'
    assert sent[0]['bytes'] <= MOST_BYTES, sent[0]['bytes']
    assert documents(sent[0]) == [pdf_base64], documents(sent[0])
    note = next(text for message in sent[0]['messages'] for text in texts(message) if '[PDF ' in text)
    assert f'[PDF {first} omitted to keep the request within the provider' in note, note
    assert 'read the file again if it is needed' in note, note
    assert not items_of(attached, 'compaction') and not items_of(attached, 'error')
    results.append({'check': 'two attached 15 MiB PDFs: earlier noted, newest sent, no refusal',
                    'requestBytes': sent[0]['bytes'], 'note': note[-220:]})
    print(f'PASS attached PDFs: one request of {sent[0]["bytes"]} bytes, the earlier PDF noted', flush=True)

    # 2. The model reads one PDF in each of two turns with the read tool.
    read = new_thread('Two PDFs read')
    state, sent = turn(read, 'read first.pdf')
    assert state['status'] == 'completed', state
    assert [len(documents(body)) for body in sent] == [0, 1], [documents(body) for body in sent]
    state, sent = turn(read, 'read second.pdf')
    assert state['status'] == 'completed', state
    assert len(sent) == 2, f'{len(sent)} requests; expected the read and the answer'
    assert all(body['bytes'] <= MOST_BYTES for body in sent), [body['bytes'] for body in sent]
    assert documents(sent[1]) == [pdf_base64], documents(sent[1])
    noted = json.dumps(sent[1]['messages'])
    assert '/first.pdf omitted to keep the request within the provider' in noted, 'no note for the earlier PDF'
    state, after = turn(read, 'Thanks, anything else?')
    assert state['status'] == 'completed' and len(after) == 1 and len(documents(after[0])) == 1, (state, len(after))
    assert not items_of(read, 'compaction') and not items_of(read, 'error')
    results.append({'check': 'two PDFs read across turns: no refusal, the next turn too',
                    'requestBytes': [body['bytes'] for body in sent + after]})
    print('PASS read PDFs: the earlier read is noted, no refusal, the next turn fits too', flush=True)

    # 3. The person attaches a 10 MiB PDF and the model reads another in the
    # same turn: 28 MB, which one request carried before. Both go, once.
    pdf(workspace / 'a10.pdf', 10)
    pdf(workspace / 'b10.pdf', 10)
    together = new_thread('Attached and read')
    del READS[:]
    state, sent = turn(together, 'Compare this with b10.pdf.\nread b10.pdf', [str(workspace / 'a10.pdf')])
    assert state['status'] == 'completed', state
    ten = (10 * MIB + 2) // 3 * 4
    assert [documents(body) for body in sent] == [[ten], [ten, ten]], [documents(body) for body in sent]
    assert READS == ['b10.pdf'], READS
    assert all(NOTE not in json.dumps(body['messages']) for body in sent)
    assert not items_of(together, 'compaction') and not items_of(together, 'error')
    results.append({'check': 'a PDF attached and one read in one turn: both sent whole, read once',
                    'requestBytes': [body['bytes'] for body in sent]})
    print(f'PASS attached and read in one turn: {sent[-1]["bytes"]} bytes, both PDFs, one read', flush=True)

    # 4. A provider that refuses every request as too large: one compaction,
    # then one plain error.
    large = new_thread('Always too large')
    state, sent = turn(large, f'{TOO_LARGE}: hello')
    assert state['status'] == 'failed', state
    summaries = [body for body in sent if body['summary']]
    assert len(summaries) == 1, f'{len(summaries)} summary requests'
    assert len(sent) == 3, f'expected refused, summary, refused; saw {len(sent)}'
    errors = items_of(large, 'error')
    assert len(errors) == 1 and errors[0]['message'] == PLAIN, errors
    assert state.get('error') == PLAIN, state.get('error')
    assert len(items_of(large, 'compaction')) == 1
    results.append({'check': 'refused as too large again after compacting: one summary, one plain error',
                    'error': errors[0]['message']})
    print(f'PASS still too large: one summary, then "{errors[0]["message"]}"', flush=True)

    # 5. A prompt too long in tokens still compacts up to four times.
    long = new_thread('Always too long')
    state, sent = turn(long, f'{TOO_LONG}: hello')
    assert state['status'] == 'failed', state
    assert len([body for body in sent if body['summary']]) == 4, [body['summary'] for body in sent]
    assert 'prompt is too long' in state.get('error', ''), state.get('error')
    results.append({'check': 'context too long in tokens: four compactions as before', 'error': state.get('error')})
    print('PASS too long in tokens: four compactions as before', flush=True)

    # 6. The TUI shows the plain error in the transcript.
    def pane(name):
        return run(['tmux', '-L', socket, 'capture-pane', '-p', '-J', '-S', '-200', '-t', name])

    run(['tmux', '-L', socket, 'new-session', '-d', '-x', '140', '-y', '45', '-s', session,
         e2e_tmux.pane_command(env, args + ['--thread', large])])
    screen = until(lambda: (lambda text: text if 'or attach smaller files.' in text else None)(pane(session)),
                   'TUI did not show the error', timeout=60)
    (EVIDENCE / 'tui.txt').write_text(screen)
    # The error wraps beside the side panel.
    assert '× This chat is too large for Anthropic to take in one request' in screen, screen
    results.append({'check': 'TUI transcript shows the plain error', 'capture': str(EVIDENCE / 'tui.txt')})
    print('PASS TUI shows the plain error', flush=True)
    (EVIDENCE / 'results.json').write_text(json.dumps(results, indent=2))
    print('evidence:', EVIDENCE)
finally:
    subprocess.run(['tmux', '-L', socket, 'kill-server'], capture_output=True)
    if connection:
        subprocess.run(args + ['serve', '--stop'], env=env, capture_output=True, timeout=60)
    service.shutdown()
    e2e_scratch.settle(root)
