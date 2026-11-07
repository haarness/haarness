#!/usr/bin/env python3
"""Real server, echo provider: MCP results carried only as structured content reach the model and workflows.

Usage: HAARNESS_BIN=target/debug/haarness python3 scripts/test-mcp-structured.py
Set HAARNESS_E2E_DIR to keep the run's state under a chosen folder.
"""
import json, os, pathlib, subprocess, sys, tempfile, time, urllib.request, uuid
import e2e_scratch
from e2e_env import clean_env

repo = pathlib.Path(__file__).resolve().parents[1]
binary = pathlib.Path(os.environ.get('HAARNESS_BIN', 'target/debug/haarness')).resolve()
fixture = repo/'crates/haarness-mcp/tests/fixtures/structured.py'
root = pathlib.Path(tempfile.mkdtemp(prefix='haarness-mcp-structured-', dir=os.environ.get('HAARNESS_E2E_DIR'))).resolve()
workspace = root/'project'; workspace.mkdir()
state = root/'state'; (state/'config').mkdir(parents=True); (state/'data').mkdir()
(state/'config/config.toml').write_text('[defaults]\nprovider="echo"\nmodel="echo-1"\n')
env = clean_env()
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
def call(thread, tool):
    before = len(results(thread))
    api(f'/threads/{thread}/turns', {'requestId': str(uuid.uuid4()), 'text': f'!tool mcp__structured__{tool} {{}}'})
    result = until(lambda: (values[before] if len(values := results(thread)) > before else None), f'{tool}: no result')
    until(lambda: not api('/turns/active'), 'turn still active')
    return result
def check(label):
    checks.append(label); print('PASS ' + label, flush=True)

try:
    run([str(binary), '--workspace', str(workspace), 'serve', '--daemon', '--port', '0'], env=env)
    registry = json.loads((state/'data/workspaces.json').read_text())
    identity = next(item['id'] for item in registry['workspaces'] if item['path'] == str(workspace))
    runtime = json.loads((state/'cache/workspaces'/identity/'runtime.json').read_text())
    url, token = runtime['url'], runtime.get('token')
    api('/mcp/structured', {'transport': 'stdio', 'command': sys.executable, 'args': [str(fixture)], 'enabled': True, 'timeout_ms': 5000})
    until(lambda: any(item['name'] == 'structured' and item['status'] == 'connected' for item in api('/mcp')), 'fixture did not connect')
    thread = api('/threads', {'title': 'MCP structured content', 'provider': 'echo', 'model': 'echo-1', 'agent': 'build'})['id']

    result = call(thread, 'weather')
    assert not result['is_error'] and json.loads(result['output']) == {'temperature': 21, 'unit': 'C', 'proof': 'structured-only-proof'}, result
    check('a tool answering only with structured content gives the model that content')

    result = call(thread, 'chart')
    assert not result['is_error'] and 'structured-beside-image-proof' in result['output'] and 'image attachment' in result['output'], result
    check('structured content beside only an image reaches the model too')

    result = call(thread, 'broken')
    assert result['is_error'] and 'MCP tool broken failed without saying why' in result['output'], result
    result = call(thread, 'refused')
    assert result['is_error'] and 'E42-refused-proof' in result['output'], result
    check('a failure without text names the tool, or gives its structured reason')

    source = ("export const meta = { name: 'structured-mcp', phases: ['call'] };\n"
              "phase('call');\n"
              "const result = await tool('mcp__structured__weather', {});\n"
              "return result.output;\n")
    run_state = api('/workflows/run', {'source': source, 'model': 'echo/echo-1'})
    deadline = time.monotonic() + 60
    while run_state['status'] == 'running' and time.monotonic() < deadline:
        time.sleep(.3); run_state = api('/workflows/runs/' + run_state['id'])
    assert run_state['status'] == 'completed', run_state
    assert 'structured-only-proof' in json.dumps(run_state.get('result')), run_state.get('result')
    check('a workflow tool() call to that MCP tool returns the structured content as its output')

    (root/'result.json').write_text(json.dumps({'passed': True, 'checks': checks}, indent=2))
    print(f'PASS {len(checks)} MCP structured-content scenarios.', flush=True)
finally:
    if url: subprocess.run([str(binary), '--workspace', str(workspace), 'serve', '--stop'], env=env, capture_output=True, timeout=35)
    e2e_scratch.settle(root)


def audio_reaches_a_model_that_cannot_hear_it():
    """Built-in Anthropic provider against a mock: Claude takes no audio. An MCP tool's recording reaches it as a
    note and the chat goes on; a recording the person sends now is refused by the model's name, and the chat still
    answers the next message."""
    import base64, http.server, re, threading
    clip = base64.b64encode(b'ID3\x04\x00\x00\x00\x00\x00\x00haarness-clip').decode()
    model, chats = 'claude-sonnet-4-6', []

    class Claude(http.server.BaseHTTPRequestHandler):
        def log_message(self, *args): pass
        def answer(self, status, body, kind='application/json'):
            self.send_response(status); self.send_header('Content-Type', kind); self.end_headers(); self.wfile.write(body.encode())
        def do_GET(self): self.answer(200, json.dumps({'data': [{'id': model, 'type': 'model'}]}))
        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            sent = json.dumps(body.get('messages', []))
            # Claude refuses content it has no block for; the recording must never reach it.
            if clip in sent or '"input_audio"' in sent:
                return self.answer(400, json.dumps({'type': 'error', 'error': {'type': 'invalid_request_error', 'message': 'audio is not supported'}}))
            last = body['messages'][-1]['content'] if body.get('messages') else []
            calls = any(tool['name'] == 'mcp__voice__clip' for tool in body.get('tools', []))
            if calls: chats.append(body)
            result = isinstance(last, list) and any(block.get('type') == 'tool_result' for block in last)
            if calls and not result and 'PLAY_THE_CLIP' in json.dumps(last):
                blocks = [{'type': 'tool_use', 'id': 'toolu_clip', 'name': 'mcp__voice__clip', 'input': {}}]; stop = 'tool_use'
            else:
                blocks = [{'type': 'text', 'text': 'NOTED' if result else 'REPLY_OK'}]; stop = 'end_turn'
            events = [{'type': 'message_start', 'message': {'id': 'msg_' + uuid.uuid4().hex[:8], 'usage': {'input_tokens': 9, 'output_tokens': 0}}}]
            for index, block in enumerate(blocks):
                if block['type'] == 'text':
                    events += [{'type': 'content_block_start', 'index': index, 'content_block': {'type': 'text', 'text': ''}},
                               {'type': 'content_block_delta', 'index': index, 'delta': {'type': 'text_delta', 'text': block['text']}}]
                else:
                    events += [{'type': 'content_block_start', 'index': index, 'content_block': {**block, 'input': {}}},
                               {'type': 'content_block_delta', 'index': index, 'delta': {'type': 'input_json_delta', 'partial_json': '{}'}}]
                events.append({'type': 'content_block_stop', 'index': index})
            events += [{'type': 'message_delta', 'delta': {'stop_reason': stop}, 'usage': {'output_tokens': 3}}, {'type': 'message_stop'}]
            self.answer(200, ''.join(f"event: {event['type']}\ndata: {json.dumps(event)}\n\n" for event in events), 'text/event-stream')

    mock = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Claude)
    threading.Thread(target=mock.serve_forever, daemon=True).start()
    base = pathlib.Path(tempfile.mkdtemp(prefix='haarness-mcp-audio-', dir=os.environ.get('HAARNESS_E2E_DIR'))).resolve()
    project = base/'project'; (project/'.haarness').mkdir(parents=True)
    (project/'.haarness/config.toml').write_text(
        f'[defaults]\nprovider="anthropic"\nmodel="{model}"\n[permissions]\n"mcp__voice__*"="allow"\n'
        '[lazy_tools]\nmode="off"\n[lsp]\nenabled=false\n')
    server = base/'voice.py'
    server.write_text(
        'import json, sys\n'
        f'CLIP = {clip!r}\n'
        'for line in sys.stdin:\n'
        '    request = json.loads(line)\n'
        "    if 'id' not in request: continue\n"
        "    method = request.get('method')\n"
        "    result = ({'protocolVersion': '2025-06-18', 'capabilities': {'tools': {}}, 'serverInfo': {'name': 'voice', 'version': '1'}} if method == 'initialize'\n"
        "        else {'tools': [{'name': 'clip', 'description': 'Records a clip', 'inputSchema': {'type': 'object'}}]} if method == 'tools/list'\n"
        "        else {'content': [{'type': 'text', 'text': 'clip recorded'}, {'type': 'audio', 'data': CLIP, 'mimeType': 'audio/mpeg'}]} if method == 'tools/call'\n"
        '        else {})\n'
        "    print(json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'result': result}), flush=True)\n")
    voice_env = clean_env('ANTHROPIC_')
    voice_env.update(HAARNESS_DATA_DIR=str(base/'data'), HAARNESS_HOME=str(base/'home'), HOME=str(base/'home'),
                     ANTHROPIC_API_KEY='fixture-key', ANTHROPIC_BASE_URL=f'http://127.0.0.1:{mock.server_port}/v1')
    (base/'home').mkdir()
    args = [str(binary), '--workspace', str(project), '--trust-project']
    connection = None
    try:
        connection = json.loads(run(args + ['desktop'], env=voice_env, input='{"action":"connect"}'))
        def request(path, body=None):
            message = urllib.request.Request(connection['url'] + '/v1' + path, data=None if body is None else json.dumps(body).encode(),
                headers={'Authorization': 'Bearer ' + connection['token'], 'Content-Type': 'application/json'})
            with urllib.request.urlopen(message, timeout=15) as reply:
                data = reply.read(); return json.loads(data) if data else None
        def turn(thread, text, attachments=()):
            started = request(f'/threads/{thread}/turns', {'requestId': str(uuid.uuid4()), 'text': text, 'attachments': list(attachments)})
            return until(lambda: (lambda now: now if now['status'] not in ('queued', 'running') else None)(request(f"/turns/{started['id']}")), f'{text}: turn did not end')
        def items(thread, kind): return [item['data'] for item in request(f'/threads/{thread}/items') if item['data']['type'] == kind]
        request('/mcp/voice', {'transport': 'stdio', 'command': sys.executable, 'args': [str(server)], 'enabled': True, 'timeout_ms': 5000})
        until(lambda: any(item['name'] == 'voice' and item['status'] == 'connected' for item in request('/mcp')), 'voice fixture did not connect')
        thread = request('/threads', {'title': 'MCP audio', 'provider': 'anthropic', 'model': model})['id']

        played = turn(thread, 'PLAY_THE_CLIP')
        assert played['status'] == 'completed', (played, items(thread, 'error'))
        assert items(thread, 'assistant_message')[-1]['text'] == 'NOTED', items(thread, 'assistant_message')
        noted = json.dumps(chats[-1]['messages'][-1])
        # The note names the recording; how a tool's file is named is the MCP client's business.
        assert re.search(r'\[Audio \S+ is left out', noted) and 'clip recorded' in noted, noted
        later = turn(thread, 'SECOND_TURN')
        assert later['status'] == 'completed' and items(thread, 'assistant_message')[-1]['text'] == 'REPLY_OK', later
        assert re.search(r'\[Audio \S+ is left out', json.dumps(chats[-1]['messages'])), 'the note goes with every later request'
        check("a tool's recording reaches a model that takes no audio as a note, and the chat goes on")

        sent = len(chats)
        refused = turn(thread, 'Listen to this', [f'data:audio/mpeg;name=voice.mp3;base64,{clip}'])
        errors = [item['message'] for item in items(thread, 'error')]
        assert refused['status'] == 'failed' and errors and "Haarness can't send audio to Claude Sonnet 4.6" in errors[-1], (refused, errors)
        assert 'Send the message without the recording' in errors[-1] and len(chats) == sent, errors
        after = turn(thread, 'AFTER_REFUSAL')
        assert after['status'] == 'completed' and items(thread, 'assistant_message')[-1]['text'] == 'REPLY_OK', after
        assert '[Audio voice.mp3 is left out' in json.dumps(chats[-1]['messages']), chats[-1]['messages']
        check('a recording the person sends is refused by the model\'s name, and the next message is answered')
    finally:
        if connection: subprocess.run(args + ['serve', '--stop'], env=voice_env, capture_output=True, timeout=35)
        mock.shutdown()
        e2e_scratch.settle(base)


audio_reaches_a_model_that_cannot_hear_it()
