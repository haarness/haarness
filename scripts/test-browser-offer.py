#!/usr/bin/env python3
"""A real server offers the browser tools only while a browser can serve the chat.

Drives `haarness serve` against a local Chat Completions stand-in that records each
request's tools, and attaches browsers the way Desktop and Haarness for Chrome do,
over the server's own WebSocket routes:

- nothing attached (the terminal, a script, a Cloud workspace): no browser_* tool is
  sent, and a call the model makes anyway is answered in plain words; the turn goes on;
- Desktop's browser attached for the chat: the tools are sent;
- that browser gone: the next turn sends none;
- a Chrome profile connected: sent again, to another chat too; gone with it;
- Desktop's new-chat browser attached: a chat whose first turn starts before its own
  browser is attached is sent them already.

Needs the `websockets` package (run it with a venv that has it).
"""
import asyncio
import http.server
import json
import os
import pathlib
import queue
import re
import secrets
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request
import uuid

import websockets

import e2e_env  # noqa: F401 (usage statistics stay off for every Haarness this script starts)

BINARY = pathlib.Path(os.environ.get('HAARNESS_BIN', 'target/debug/haarness')).resolve()
REQUESTS = []
SCRIPT = []


class Provider(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        body = json.dumps({'data': [{'id': 'fake-1', 'object': 'model'}]}).encode()
        self.send_response(200)
        self.send_header('content-type', 'application/json')
        self.send_header('content-length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers['content-length'])))
        tools = [tool['function']['name'] for tool in request.get('tools') or []]
        if tools:
            REQUESTS.append({'tools': tools, 'bytes': len(json.dumps(request.get('tools'))), 'messages': request['messages']})
        step = SCRIPT.pop(0) if tools and SCRIPT else 'done'
        if isinstance(step, dict):
            delta = {'tool_calls': [{'index': 0, 'id': 'call_' + secrets.token_hex(4), 'type': 'function',
                                     'function': {'name': step['call'], 'arguments': json.dumps(step.get('arguments', {}))}}]}
            finish = 'tool_calls'
        else:
            delta, finish = {'content': step if tools else 'A title'}, 'stop'
        chunks = [{'choices': [{'index': 0, 'delta': delta}]},
                  {'choices': [{'index': 0, 'delta': {}, 'finish_reason': finish}],
                   'usage': {'prompt_tokens': 10, 'completion_tokens': 2}}]
        body = ''.join('data: ' + json.dumps(chunk) + '\n\n' for chunk in chunks) + 'data: [DONE]\n\n'
        self.send_response(200)
        self.send_header('content-type', 'text/event-stream')
        self.send_header('content-length', str(len(body.encode())))
        self.end_headers()
        self.wfile.write(body.encode())


def main():
    root = pathlib.Path(tempfile.mkdtemp(prefix='haarness-browser-offer-'))
    fake = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Provider)
    threading.Thread(target=fake.serve_forever, daemon=True).start()
    workspace = root / 'workspace'
    (workspace / '.haarness').mkdir(parents=True)
    (workspace / '.haarness/config.toml').write_text(
        '[defaults]\nprovider="fakeai"\nmodel="fake-1"\n[lsp]\nenabled=false\n'
        '[providers.ollama]\nenabled=false\n[providers.lmstudio]\nenabled=false\n'
        '[providers.fakeai]\nprotocol="openai-chat-completions"\nname="Fake"\n'
        f'base_url="http://127.0.0.1:{fake.server_address[1]}/v1"\nmodels=["fake-1"]\nrequires_auth=false\n'
        'tools=true\ncontext_window=131072\nmax_output_tokens=4096\n')
    token = secrets.token_hex(24)
    env = {**os.environ, 'HAARNESS_DATA_DIR': str(root / 'profile'), 'HAARNESS_SERVER_TOKEN': token, 'HAARNESS_TELEMETRY': '0'}
    env.pop('HAARNESS_TEST_PROVIDER', None)
    log = (root / 'server.log').open('w')
    server = subprocess.Popen([str(BINARY), '--workspace', str(workspace), '--trust-project', 'serve', '--port', '0'],
                              env=env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=log, text=True)
    ready = queue.Queue()

    def reader():
        for line in server.stdout:
            found = re.search(r'Haarness server listening on (http://\S+)', line)
            if found:
                ready.put(found[1])
        ready.put(None)

    threading.Thread(target=reader, daemon=True).start()
    try:
        url = ready.get(timeout=90)
        assert url, 'server did not start'
        asyncio.run(scenario(url, token))
        print('PASS browser tools follow the browsers that can serve the chat; evidence:', root, flush=True)
        return 0
    finally:
        server.terminate()
        try:
            server.wait(timeout=15)
        except subprocess.TimeoutExpired:
            server.kill()
        fake.shutdown()


async def scenario(url, token):
    headers = {'Authorization': 'Bearer ' + token, 'Content-Type': 'application/json'}

    def api(path, value=None, method=None):
        request = urllib.request.Request(url + '/v1' + path, data=None if value is None else json.dumps(value).encode(), headers=headers, method=method)
        with urllib.request.urlopen(request, timeout=30) as response:
            body = response.read()
            return json.loads(body) if body else None

    async def turn(thread, text):
        before = len(REQUESTS)
        admitted = await asyncio.to_thread(api, f'/threads/{thread}/turns', {'requestId': str(uuid.uuid4()), 'text': text})
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            status = await asyncio.to_thread(api, '/turns/' + admitted['id'])
            if status['status'] in ('completed', 'failed', 'cancelled', 'interrupted'):
                assert status['status'] == 'completed', status
                return REQUESTS[before:]
            await asyncio.sleep(0.1)
        raise AssertionError('turn did not finish')

    def browser(requests):
        return [any(name.startswith('browser_') for name in request['tools']) for request in requests]

    ws = url.replace('http://', 'ws://') + '/v1'
    chat = (await asyncio.to_thread(api, '/threads', {'title': 'browser offer'}))['id']

    # Nothing attached: none sent; a call anyway is answered in plain words.
    SCRIPT[:] = [{'call': 'browser_navigate', 'arguments': {'tabID': 'tab_1', 'url': 'https://example.com'}}, 'no browser here']
    requests = await turn(chat, 'open example.com')
    assert browser(requests) == [False, False], browser(requests)
    without = requests[0]['bytes']
    answer = next(message for message in requests[1]['messages'] if message['role'] == 'tool')
    assert 'No browser is available to this chat now' in answer['content'] and 'webfetch' in answer['content'], answer

    # Desktop's browser for this chat: sent.
    desktop = await websockets.connect(ws + f'/threads/{chat}/browser/host', additional_headers=headers, ping_interval=None)
    assert json.loads(await desktop.recv())['type'] == 'attached'
    requests = await turn(chat, 'with Desktop')
    assert browser(requests) == [True], browser(requests)
    with_browser = requests[0]['bytes']

    # Desktop closed the chat: the next turn sends none.
    await desktop.close()
    await asyncio.sleep(0.3)
    requests = await turn(chat, 'Desktop closed')
    assert browser(requests) == [False], browser(requests)

    # A Chrome profile serves every chat, a new one too, until it goes.
    chrome = await websockets.connect(ws + '/browser/extension/host?version=1', additional_headers=headers, ping_interval=None)
    assert json.loads(await chrome.recv())['type'] == 'attached'
    await chrome.send(json.dumps({'type': 'hello', 'browser': {'id': str(uuid.uuid4()), 'kind': 'chrome', 'name': 'Google Chrome', 'label': None, 'version': '154.0.8037.98'}, 'extension': {'version': '0.1.1'}}))
    await asyncio.sleep(0.3)
    other = (await asyncio.to_thread(api, '/threads', {'title': 'another chat'}))['id']
    assert browser(await turn(other, 'with Chrome')) == [True]
    assert browser(await turn(chat, 'with Chrome too')) == [True]
    await chrome.close()
    await asyncio.sleep(0.3)
    assert browser(await turn(other, 'Chrome gone')) == [False]

    # Desktop on its new-chat screen (a browser of no chat yet) attaches each
    # chat's browser as it is shown: a chat whose first turn starts before
    # that is sent the tools already.
    draft = await websockets.connect(ws + f'/browser/drafts/{uuid.uuid4()}/host', additional_headers=headers, ping_interval=None)
    assert json.loads(await draft.recv())['type'] == 'attached'
    fresh = (await asyncio.to_thread(api, '/threads', {'title': 'a new chat'}))['id']
    assert browser(await turn(fresh, 'from the new-chat screen')) == [True]
    await draft.close()
    await asyncio.sleep(0.3)
    assert browser(await turn(fresh, 'Desktop quit')) == [False]
    print(f'tool schemas per request: {without} bytes without a browser, {with_browser} with one', flush=True)


if __name__ == '__main__':
    sys.exit(main())
