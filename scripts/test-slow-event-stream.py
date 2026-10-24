#!/usr/bin/env python3
"""A client reading the event stream slowly keeps its place while a model streams fast.

A mock Anthropic Messages server answers one prompt with 4,000 tiny text deltas as fast as it can.
Against it, a debug `haarness` daemon in a private data folder shows that:

1. a raw `/v1/events` reader with a 4 KiB receive buffer that takes 1 KiB every 10 ms (about 100 KB/s,
   far below the stream) never gets `resync_required`, and the deltas it reads, joined while they
   waited for it, spell the whole answer in order;
2. the terminal UI attached to the chat in tmux shows the answer's last words and never says it
   reconnected.

On a build without joined deltas the slow reader falls out of the server's 2,048-event queue and
is sent `resync_required` within the first seconds.

Usage: python3 scripts/test-slow-event-stream.py   (HAARNESS_BIN picks the binary,
HAARNESS_E2E_ROOT a folder to keep the fixture in).
"""
import http.server
import json
import os
from pathlib import Path
import shlex
import socket
import subprocess
import tempfile
import threading
import time
import urllib.parse
import urllib.request
import uuid

import e2e_scratch
from e2e_env import ISOLATED

REPO = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get('HAARNESS_BIN', REPO / 'target/debug/haarness')).resolve()
MODEL = 'claude-sonnet-4-6'
WORDS = [f'w{index} ' for index in range(4000)]


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
        flood = 'FLOOD' in json.dumps(body['messages'][-1])
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.end_headers()
        events = [{'type': 'message_start', 'message': {'id': 'msg_flood', 'usage': {'input_tokens': 40, 'output_tokens': 0}}},
                  {'type': 'content_block_start', 'index': 0, 'content_block': {'type': 'text', 'text': ''}}]
        for word in (WORDS if flood else ['done']):
            events.append({'type': 'content_block_delta', 'index': 0, 'delta': {'type': 'text_delta', 'text': word}})
        events += [{'type': 'content_block_stop', 'index': 0},
                   {'type': 'message_delta', 'delta': {'stop_reason': 'end_turn', 'stop_sequence': None}, 'usage': {'output_tokens': 3}},
                   {'type': 'message_stop'}]
        payload = b''.join(('event: ' + event['type'] + '\ndata: ' + json.dumps(event) + '\n\n').encode() for event in events)
        self.wfile.write(payload)
        self.wfile.flush()


class SlowReader(threading.Thread):
    """`/v1/events` read through a small receive buffer, a little at a time."""

    def __init__(self, url, token, after):
        super().__init__(daemon=True)
        parsed = urllib.parse.urlparse(url)
        self.address = (parsed.hostname, parsed.port)
        self.request = (f'GET /v1/events HTTP/1.1\r\nHost: {parsed.netloc}\r\nAuthorization: Bearer {token}\r\n'
                        f'Accept: text/event-stream\r\nLast-Event-ID: {after}\r\n\r\n').encode()
        self.events, self.resyncs, self.stop = [], 0, threading.Event()

    def run(self):
        connection = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        connection.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 4096)
        connection.connect(self.address)
        connection.sendall(self.request)
        buffer = b''
        while not self.stop.is_set():
            chunk = connection.recv(1024)
            if not chunk:
                break
            buffer += chunk
            while b'\n\n' in buffer:
                frame, buffer = buffer.split(b'\n\n', 1)
                lines = frame.decode(errors='replace').splitlines()
                kind = next((line[7:] for line in lines if line.startswith('event: ')), None)
                data = '\n'.join(line[6:] for line in lines if line.startswith('data: '))
                if kind == 'resync_required':
                    self.resyncs += 1
                elif kind and data:
                    self.events.append(json.loads(data))
            time.sleep(.01)
        connection.close()


service = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Anthropic)
threading.Thread(target=service.serve_forever, daemon=True).start()
keep = os.environ.get('HAARNESS_E2E_ROOT')
root = Path(keep or tempfile.mkdtemp(prefix='haarness-slow-stream-')).resolve()
root.mkdir(parents=True, exist_ok=True)
home, workspace, data = root / 'home', root / 'workspace', root / 'data'
session = 'haarness-slow-' + str(uuid.uuid4())[:8]
env = {**ISOLATED, 'PATH': '/usr/bin:/bin:/usr/sbin:/sbin', 'HOME': str(home), 'HAARNESS_HOME': str(home),
       'HAARNESS_DATA_DIR': str(data), 'USER': os.environ.get('USER', 'fixture'), 'LANG': 'en_US.UTF-8',
       'ANTHROPIC_API_KEY': 'fixture-key', 'ANTHROPIC_BASE_URL': f'http://127.0.0.1:{service.server_port}/v1'}
args = [str(BINARY), '--workspace', str(workspace), '--trust-project']
reader = None
try:
    (workspace / '.haarness').mkdir(parents=True, exist_ok=True)
    (workspace / '.haarness/config.toml').write_text(f'[defaults]\nprovider="anthropic"\nmodel="{MODEL}"\n[lsp]\nenabled=false\n')
    run(['git', 'init', '-q', str(workspace)])
    connection = json.loads(run(args + ['desktop'], input='{"action":"connect"}', env=env, timeout=120))

    def api(path, body=None):
        request = urllib.request.Request(connection['url'] + '/v1' + path, data=None if body is None else json.dumps(body).encode(),
                                         headers={'Authorization': 'Bearer ' + connection['token'], 'Content-Type': 'application/json'})
        with urllib.request.urlopen(request, timeout=30) as response:
            raw = response.read()
            return json.loads(raw) if raw else None

    chat = api('/threads', {'title': 'Slow reader', 'provider': 'anthropic', 'model': MODEL})['id']
    command = shlex.join(['env', *[f'{key}={value}' for key, value in env.items()], *args, '--thread', chat])
    run(['tmux', 'new-session', '-d', '-x', '120', '-y', '40', '-s', session, command])

    def pane():
        return run(['tmux', 'capture-pane', '-p', '-J', '-t', session])
    until(lambda: 'review' in pane().lower(), 'TUI failed to start')
    reader = SlowReader(connection['url'], connection['token'], api('/info')['eventCursor'])
    reader.start()
    time.sleep(.5)
    turn = api(f'/threads/{chat}/turns', {'requestId': str(uuid.uuid4()), 'text': 'FLOOD please'})['id']
    until(lambda: api(f'/turns/{turn}')['status'] not in ('queued', 'running'), 'the flood turn did not finish')
    until(lambda: any(event.get('type') == 'turn.finished' and event.get('turnId') == turn for event in reader.events) or reader.resyncs,
          'the slow reader never reached the end of the turn', timeout=180)
    deltas = [event for event in reader.events if event.get('type') == 'turn.text_delta' and event.get('turnId') == turn]
    text = ''.join(event['payload']['delta'] for event in deltas)
    print(f'slow reader: {len(deltas)} delta events for {len(WORDS)} streamed, {reader.resyncs} resyncs', flush=True)
    assert reader.resyncs == 0, f'the slow reader was sent resync_required {reader.resyncs} times'
    assert text == ''.join(WORDS), f'the joined deltas lost or reordered text: {len(text)} characters'
    assert len(deltas) < len(WORDS), 'nothing was joined for a reader this slow'
    ids = [event['eventId'] for event in reader.events]
    assert ids == sorted(ids) and len(set(ids)) == len(ids), 'event ids went backwards'
    print('PASS a slow reader keeps its place and every word', flush=True)
    until(lambda: 'w3999' in pane(), 'the TUI did not show the end of the answer', timeout=60)
    shown = pane()
    (root / 'tui.txt').write_text(shown)
    assert 'Reconnected' not in shown, shown
    print('PASS the TUI shows the whole answer without reconnecting', flush=True)
finally:
    if reader:
        reader.stop.set()
    subprocess.run(['tmux', 'kill-session', '-t', session], capture_output=True)
    subprocess.run(args + ['serve', '--stop'], env=env, capture_output=True, timeout=60)
    service.shutdown()
    if not keep:
        e2e_scratch.settle(root)
