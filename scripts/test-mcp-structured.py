#!/usr/bin/env python3
"""Real server, echo provider: MCP results carried only as structured content reach the model and workflows;
resources embedded as base64 reach the model as what they hold (an image as an attachment, text as text, anything
else named), never as base64; audio of a type Haarness does not carry is named rather than attached; screenshots
larger than providers take (a 24 MB 4K one, a full page 9,000 pixels tall) reach the model shrunk as Read shrinks
an image, with the result's text; and a 200 KB result reaches the model cut with the whole saved, while a workflow gets
all of it. One-line JSON is laid out a value a line, whole when it then fits, and within a few times its length however deep
it nests; what a Code Mode script returns is cut too, a string as its own lines, a list whole on one line when only that
fits. A script gets the server's metadata for its own call, never an earlier call's with the same text.

Usage: HAARNESS_BIN=target/debug/haarness python3 scripts/test-mcp-structured.py
Set HAARNESS_E2E_DIR to keep the run's state under a chosen folder.
"""
import base64, json, os, pathlib, subprocess, sys, tempfile, time, urllib.request, uuid
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
def call(thread, tool, seconds=30):
    before = len(results(thread))
    api(f'/threads/{thread}/turns', {'requestId': str(uuid.uuid4()), 'text': f'!tool mcp__structured__{tool} {{}}'})
    result = until(lambda: (values[before] if len(values := results(thread)) > before else None), f'{tool}: no result', seconds)
    until(lambda: not api('/turns/active'), 'turn still active')
    return result
def execute(thread, code):
    before = len(results(thread))
    api(f'/threads/{thread}/turns', {'requestId': str(uuid.uuid4()), 'text': '!tool execute ' + json.dumps({'code': code})})
    until(lambda: not api('/turns/active') and any(item['name'] == 'execute' for item in results(thread)[before:]), 'execute: no result', 60)
    coded = next(item for item in results(thread)[before:] if item['name'] == 'execute')
    assert not coded['is_error'], coded['output'][:400]
    return coded
def workflow(source):
    run_state = api('/workflows/run', {'source': source, 'model': 'echo/echo-1'})
    deadline = time.monotonic() + 60
    while run_state['status'] == 'running' and time.monotonic() < deadline:
        time.sleep(.3); run_state = api('/workflows/runs/' + run_state['id'])
    assert run_state['status'] == 'completed', run_state
    return run_state.get('result')
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

    PIXEL = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg=='
    result = call(thread, 'embedded')
    output = result['output']
    assert not result['is_error'] and output.startswith('Three files.\n[image/png attachment: ui://chart.png]\n'), output
    assert '\n[data://row.json: application/json]\n{"proof": "embedded-text-proof"}\n' in output, output
    assert '[files://pack.zip: application/zip, 300 bytes]' in output, output
    assert PIXEL not in output and 'blob' not in output, output
    assert result['metadata']['attachments'] == ['data:image/png;base64,' + PIXEL], result['metadata']
    check('embedded resources reach the model as what they hold: an image under IMAGE/PNG as an attachment, base64 JSON as its text, an archive named by type and size, and no base64 text')

    result = call(thread, 'odd_media')
    assert not result['is_error'] and result['output'].startswith('[audio/webm audio, about 6 bytes, left out: Haarness does not attach this type]'), result['output']
    assert result['metadata']['attachments'] == ['data:image/png;base64,' + PIXEL], result['metadata']
    again = call(thread, 'weather')
    assert not again['is_error'], again
    check('audio of a type Haarness does not carry is named, not attached, beside an image it does attach, and the chat goes on')

    def shown(data_url):
        """The width and height of the PNG a data URL holds, from its header."""
        assert data_url.startswith('data:image/png;base64,'), data_url[:60]
        header = base64.b64decode(data_url.split(',', 1)[1][:44])
        return int.from_bytes(header[16:20], 'big'), int.from_bytes(header[20:24], 'big')
    for tool, size in [('screenshot', (2000, 1125)), ('full_page', (320, 2000))]:
        started = time.monotonic()
        # Shrinking takes a debug build far longer than a release one.
        result = call(thread, tool, 240)
        print(f'{tool}: answered in {time.monotonic() - started:.1f} s', flush=True)
        assert not result['is_error'], result['output'][:400]
        assert result['output'] == f'The whole page.\n[image/png image attachment, shown to you as {size[0]}×{size[1]}]', result['output']
        attachments = result['metadata']['attachments']
        assert len(attachments) == 1 and shown(attachments[0]) == size, (len(attachments), attachments[0][:80])
    again = call(thread, 'weather')
    assert not again['is_error'], again
    check('a 24 MB 4K screenshot and a full page 9,000 pixels tall reach the model shrunk to 2000 pixels on their longest side, with the result\'s text, and the chat goes on')

    WIDE = ''.join('row %05d %s\n' % (n, 'w' * 90) for n in range(2000))
    result = call(thread, 'wide')
    assert not result['is_error'] and len(result['output']) < 52 * 1024, len(result['output'])
    assert result['output'].startswith('row 00000 ') and 'Full output saved to: ' in result['output'], result['output'][-300:]
    saved = pathlib.Path(result['metadata']['outputPath'])
    assert saved.read_text() == WIDE and result['metadata']['truncated'] is True and result['metadata']['rows'] == 2000, result['metadata']
    check('a 200 KB result reaches the model as its first 50 KiB, the whole saved where the note says, its structured content kept')

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

    source = ("export const meta = { name: 'whole-mcp-text', phases: ['call'] };\n"
              "phase('call');\n"
              "const result = await tool('mcp__structured__wide', {});\n"
              "return result.output.length;\n")
    run_state = api('/workflows/run', {'source': source, 'model': 'echo/echo-1'})
    deadline = time.monotonic() + 60
    while run_state['status'] == 'running' and time.monotonic() < deadline:
        time.sleep(.3); run_state = api('/workflows/runs/' + run_state['id'])
    assert run_state['status'] == 'completed' and run_state.get('result') == len(WIDE), run_state.get('result')
    check('a workflow tool() call to the 200 KB tool gets all of its text')

    before = len(results(thread))
    code = "return (await tool('mcp__structured__wide', {})).text.length;"
    api(f'/threads/{thread}/turns', {'requestId': str(uuid.uuid4()), 'text': '!tool execute ' + json.dumps({'code': code})})
    until(lambda: not api('/turns/active') and any(item['name'] == 'execute' for item in results(thread)[before:]), 'execute: no result', 60)
    coded = next(item for item in results(thread)[before:] if item['name'] == 'execute')
    assert not coded['is_error'] and json.loads(coded['output']) == len(WIDE), coded['output'][:400]
    check('Code Mode gets all of the 200 KB tool\'s text too')

    # What Code Mode hands back is cut for the model in turn.
    before = len(results(thread))
    code = "return (await tool('mcp__structured__wide', {})).text;"
    api(f'/threads/{thread}/turns', {'requestId': str(uuid.uuid4()), 'text': '!tool execute ' + json.dumps({'code': code})})
    until(lambda: not api('/turns/active') and any(item['name'] == 'execute' for item in results(thread)[before:]), 'execute: no result', 60)
    coded = next(item for item in results(thread)[before:] if item['name'] == 'execute')
    assert not coded['is_error'] and len(coded['output']) < 52 * 1024 and 'Full output saved to: ' in coded['output'], len(coded['output'])
    # A string comes as its own lines, the model's and the saved file's, not as one escaped JSON line.
    assert coded['output'].startswith('row 00000 ' + 'w' * 90 + '\nrow 00001 '), coded['output'][:200]
    saved = pathlib.Path(coded['metadata']['outputPath']).read_text()
    assert saved == WIDE and max(len(line) for line in saved.splitlines()) < 200, (len(saved), saved[:200])
    check('what a Code Mode script returns is cut for the model too, a string as lines read and grep can take')

    ROWS = json.dumps([{'id': n, 'title': 'row %d' % n, 'body': 'b' * 40} for n in range(1500)], separators=(',', ':'))
    result = call(thread, 'rows')
    assert not result['is_error'] and result['output'].startswith('[\n  {\n    "id": 0,'), result['output'][:200]
    assert result['output'].count('\n') > 1900, result['output'].count('\n')
    saved = pathlib.Path(result['metadata']['outputPath']).read_text()
    assert json.loads(saved) == json.loads(ROWS) and max(len(line) for line in saved.splitlines()) < 200
    source = ("export const meta = { name: 'compact-rows', phases: ['call'] };\nphase('call');\n"
              "return (await tool('mcp__structured__rows', {})).output;\n")
    run_state = api('/workflows/run', {'source': source, 'model': 'echo/echo-1'})
    deadline = time.monotonic() + 60
    while run_state['status'] == 'running' and time.monotonic() < deadline:
        time.sleep(.3); run_state = api('/workflows/runs/' + run_state['id'])
    assert run_state['status'] == 'completed' and run_state.get('result') == ROWS, str(run_state.get('result'))[:200]
    check('120 KB of one-line JSON reaches the model as its first 2,000 lines laid out a value a line, saved readable by line, and a workflow gets it as the server wrote it')

    # Laid out by adding spaces and line breaks only: no number rounded, no repeated key dropped.
    for tool, start in [('ledger', '[\n  {\n    "wei": 1234567890123456789012345,\n    "rate": 0.12345678901234567890123,'),
                        ('found', 'Found 800 entries:\n[\n  {\n    "wei": 1234567890123456789012345,')]:
        result = call(thread, tool)
        assert not result['is_error'] and result['output'].startswith(start), result['output'][:200]
        assert '"key": 1,\n    "key": 2\n  },' in result['output'], result['output'][:300]
        saved = pathlib.Path(result['metadata']['outputPath']).read_text()
        sent = subprocess.run([sys.executable, str(fixture)], input=json.dumps(
            {'jsonrpc': '2.0', 'id': 1, 'method': 'tools/call', 'params': {'name': tool}}) + '\n',
            text=True, capture_output=True, timeout=30, check=True).stdout
        written = json.loads(sent)['result']['content'][0]['text']
        assert saved.replace('\n', '').replace(' ', '') == written.replace('\n', '').replace(' ', ''), tool
        assert max(len(line) for line in saved.splitlines()) < 80, tool
    check('one-line JSON is laid out with every number and repeated key as the server wrote it, whole or after a heading')

    source = ("export const meta = { name: 'tree-metadata', phases: ['call'] };\nphase('call');\n"
              "const result = await tool('mcp__structured__tree', {});\n"
              "return { tree: result.metadata.tree.length, truncated: result.metadata.truncated, text: result.output.length };\n")
    run_state = api('/workflows/run', {'source': source, 'model': 'echo/echo-1'})
    deadline = time.monotonic() + 60
    while run_state['status'] == 'running' and time.monotonic() < deadline:
        time.sleep(.3); run_state = api('/workflows/runs/' + run_state['id'])
    assert run_state['status'] == 'completed' and run_state.get('result') == {'tree': 1, 'truncated': False, 'text': 80 * 1024}, run_state.get('result')
    check('a workflow gets the server\'s own metadata for a cut result, its own `truncated` included')

    # Indented four spaces it passes 50 KiB; laid out at two it fits, so it is not cut.
    SPACED = json.dumps([{'id': n, 'title': 't' * 50, 'body': 'b' * 50, 'owner': 'o' * 50, 'state': 'open'}
                         for n in range(200)], indent=4)
    result = call(thread, 'spaced')
    assert not result['is_error'] and len(SPACED) > 50 * 1024, len(SPACED)
    assert result['output'] == json.dumps(json.loads(SPACED), indent=2), result['output'][-300:]
    assert 'truncated' not in result['output'] and 'outputPath' not in (result.get('metadata') or {}), result.get('metadata')
    source = ("export const meta = { name: 'spaced-rows', phases: ['call'] };\nphase('call');\n"
              "return (await tool('mcp__structured__spaced', {})).output;\n")
    run_state = api('/workflows/run', {'source': source, 'model': 'echo/echo-1'})
    deadline = time.monotonic() + 60
    while run_state['status'] == 'running' and time.monotonic() < deadline:
        time.sleep(.3); run_state = api('/workflows/runs/' + run_state['id'])
    assert run_state['status'] == 'completed' and run_state.get('result') == SPACED, str(run_state.get('result'))[:200]
    check('JSON that fits once laid out reaches the model whole, with no truncation note or saved file, and a workflow gets it as the server wrote it')

    # Two spaces a level would make this 1.8 GB; past 32 levels the rest stays on its line.
    started = time.monotonic()
    result = call(thread, 'nested')
    assert not result['is_error'] and len(result['output']) < 52 * 1024, len(result['output'])
    saved = pathlib.Path(result['metadata']['outputPath']).read_text()
    assert len(saved) < 2 * 60000 and len(saved.splitlines()) == 65, (len(saved), len(saved.splitlines()))
    assert saved.replace('\n', '').replace(' ', '') == '[' * 30000 + ']' * 30000
    assert time.monotonic() - started < 20, time.monotonic() - started
    check('JSON nested 30,000 deep is laid out 32 levels down, its saved file about as long as the server\'s text')

    # Each call of `stamp` gives the same text, laid out to fit, and its own number as structured content.
    stamp = "return (await tool('mcp__structured__stamp', {})).metadata;"
    assert json.loads(execute(thread, stamp)['output']) == {'call': 1}
    assert call(thread, 'stamp')['metadata'] == {'call': 2}
    call(thread, 'spaced')
    assert json.loads(execute(thread, stamp)['output']) == {'call': 3}
    assert call(thread, 'stamp')['metadata'] == {'call': 4}
    spaced = execute(thread, "return (await tool('mcp__structured__spaced', {})).metadata;")
    assert json.loads(spaced['output']) is None, spaced['output']
    source = ("export const meta = { name: 'stamp-metadata', phases: ['call'] };\nphase('call');\n"
              "return (await tool('mcp__structured__stamp', {})).metadata;\n")
    assert workflow(source) == {'call': 5}
    check('a script gets the server\'s metadata for its own call, not for the model\'s earlier call of that tool or of one with the same text')

    SERIES = json.dumps({'result': {'series': [n % 10 for n in range(40000)]}}, separators=(',', ':'))
    result = call(thread, 'series')
    assert not result['is_error'] and result['output'].startswith('{\n  "result": {\n    "series": [\n    0,\n'), result['output'][:200]
    saved = pathlib.Path(result['metadata']['outputPath']).read_text()
    assert len(saved.splitlines()) == 40006 and max(len(line) for line in saved.splitlines()) < 20, len(saved.splitlines())
    assert len(saved) <= 4 * len(SERIES) and saved.replace('\n', '').replace(' ', '') == SERIES, len(saved)
    nested = execute(thread, "return { m: [[0, 0, 0], [0, 0, 0], [0, 0, 0]] };")
    assert nested['output'] == json.dumps({'m': [[0, 0, 0]] * 3}, indent=2), nested['output']
    check('JSON that indenting every level would make too long is still laid out a value a line, indented less deep, and a short nested value a script returns is laid out in full')

    ids = execute(thread, "return Array.from({ length: 2500 }, (_, n) => n);")
    assert ids['output'] == json.dumps(list(range(2500)), separators=(',', ':')), ids['output'][:200]
    assert 'outputPath' not in (ids.get('metadata') or {}), ids.get('metadata')
    check('a Code Mode list past 2,000 lines laid out comes whole on one line when that fits')

    result = call(thread, 'loud_failure')
    assert result['is_error'] and len(result['output']) < 52 * 1024, len(result['output'])
    assert 'The tool call failed and its output was truncated. Full output saved to: ' in result['output'], result['output'][-300:]
    check('a long failure is cut for the model as well, and says the call failed')

    (root/'result.json').write_text(json.dumps({'passed': True, 'checks': checks}, indent=2))
    print(f'PASS {len(checks)} MCP structured-content scenarios.', flush=True)
finally:
    if url: subprocess.run([str(binary), '--workspace', str(workspace), 'serve', '--stop'], env=env, capture_output=True, timeout=35)
    e2e_scratch.settle(root)


def voice_server(base, clip):
    """An MCP stdio server under `base` whose one tool, `clip`, answers with a recording."""
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
    return server


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
    server = voice_server(base, clip)
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
        # The refusal is Haarness's own advice, not called a provider error.
        assert refused['status'] == 'failed' and errors and errors[-1].startswith("Haarness can't send audio to Claude Sonnet 4.6."), (refused, errors)
        assert 'Send the message without the recording' in errors[-1] and len(chats) == sent, errors
        after = turn(thread, 'AFTER_REFUSAL')
        assert after['status'] == 'completed' and items(thread, 'assistant_message')[-1]['text'] == 'REPLY_OK', after
        assert '[Audio voice.mp3 is left out' in json.dumps(chats[-1]['messages']), chats[-1]['messages']
        check('a recording the person sends is refused by the model\'s name, and the next message is answered')
    finally:
        if connection: subprocess.run(args + ['serve', '--stop'], env=voice_env, capture_output=True, timeout=35)
        mock.shutdown()
        e2e_scratch.settle(base)


def audio_on_haarness_cloud():
    """Haarness Cloud stand-in, as its router answers: Claude takes no `input_audio` ("… doesn't take audio") and
    Gemini does. A tool's MP3 reaches Claude as a note and the chat goes on; Gemini gets the recording itself."""
    import base64, datetime, http.server, re, threading
    clip = base64.b64encode(b'ID3\x04\x00\x00\x00\x00\x00\x00haarness-cloud-clip').decode()
    chats = []

    class Cloud(http.server.BaseHTTPRequestHandler):
        def log_message(self, *args): pass
        def answer(self, status, value):
            body = json.dumps(value).encode()
            self.send_response(status); self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(body))); self.end_headers(); self.wfile.write(body)
        def do_GET(self):
            self.answer(200, {'object': 'list', 'data': [{'id': 'anthropic/claude-sonnet-5-5', 'supports_vision': True},
                                                         {'id': 'google/gemini-3.5-flash'}]})
        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            if self.path.endswith('/router-session'):
                expires = datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(hours=1)
                return self.answer(200, {'token': 'fixture-router', 'routerUrl': cloud_url + '/', 'expiresAt': expires.isoformat()})
            sent = json.dumps(body['messages'])
            if body.get('tools'): chats.append(body)
            if 'input_audio' in sent and not body['model'].startswith('google/'):
                return self.answer(400, {'error': {'message': f"{body['model']} doesn't take audio", 'type': 'invalid_request_error',
                                                   'code': 'unsupported_capability'}})
            person = next(i for i, message in reversed(list(enumerate(body['messages'])))
                          if message['role'] == 'user' and isinstance(message['content'], str))
            answered = any(message['role'] == 'tool' for message in body['messages'][person:])
            if body.get('tools') and not answered and 'PLAY_THE_CLIP' in body['messages'][person]['content']:
                events = [{'choices': [{'delta': {'tool_calls': [{'index': 0, 'id': 'call_clip', 'type': 'function',
                           'function': {'name': 'mcp__voice__clip', 'arguments': '{}'}}]}, 'finish_reason': None}]},
                          {'choices': [{'delta': {}, 'finish_reason': 'tool_calls'}]}]
            else:
                events = [{'choices': [{'delta': {'content': 'NOTED' if answered else 'REPLY_OK'}, 'finish_reason': None}]},
                          {'choices': [{'delta': {}, 'finish_reason': 'stop'}], 'usage': {'prompt_tokens': 9, 'completion_tokens': 2}}]
            self.send_response(200); self.send_header('Content-Type', 'text/event-stream'); self.end_headers()
            for event in events: self.wfile.write(('data: ' + json.dumps(event) + '\n\n').encode())
            self.wfile.write(b'data: [DONE]\n\n'); self.wfile.flush()

    stand_in = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Cloud)
    cloud_url = f'http://127.0.0.1:{stand_in.server_port}'
    threading.Thread(target=stand_in.serve_forever, daemon=True).start()
    base = pathlib.Path(tempfile.mkdtemp(prefix='haarness-mcp-cloud-audio-', dir=os.environ.get('HAARNESS_E2E_DIR'))).resolve()
    project = base/'project'; (project/'.haarness').mkdir(parents=True)
    (project/'.haarness/config.toml').write_text('[permissions]\n"mcp__voice__*"="allow"\n[lazy_tools]\nmode="off"\n[lsp]\nenabled=false\n')
    server = voice_server(base, clip)
    account = base/'account'; account.mkdir()
    credentials = account/'credentials.json'
    credentials.write_text(json.dumps({'apiUrl': cloud_url + '/', 'accessToken': 'fixture-account', 'refreshToken': 'fixture-refresh',
        'accessExpiresAt': (datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(hours=1)).isoformat()}))
    credentials.chmod(0o600)
    cloud_env = clean_env()
    cloud_env.update(HAARNESS_DATA_DIR=str(base/'data'), HAARNESS_HOME=str(base/'home'), HOME=str(base/'home'),
                     HAARNESS_CLOUD_API_URL=cloud_url, HAARNESS_CLOUD_CONFIG_DIR=str(account))
    (base/'home').mkdir()
    args = [str(binary), '--workspace', str(project), '--trust-project']
    connection = None
    try:
        connection = json.loads(run(args + ['desktop'], env=cloud_env, input='{"action":"connect"}'))
        def request(path, body=None):
            message = urllib.request.Request(connection['url'] + '/v1' + path, data=None if body is None else json.dumps(body).encode(),
                headers={'Authorization': 'Bearer ' + connection['token'], 'Content-Type': 'application/json'})
            with urllib.request.urlopen(message, timeout=15) as reply:
                data = reply.read(); return json.loads(data) if data else None
        def turn(thread, text):
            started = request(f'/threads/{thread}/turns', {'requestId': str(uuid.uuid4()), 'text': text})
            return until(lambda: (lambda now: now if now['status'] not in ('queued', 'running') else None)(request(f"/turns/{started['id']}")), f'{text}: turn did not end')
        def items(thread, kind): return [item['data'] for item in request(f'/threads/{thread}/items') if item['data']['type'] == kind]
        request('/mcp/voice', {'transport': 'stdio', 'command': sys.executable, 'args': [str(server)], 'enabled': True, 'timeout_ms': 5000})
        until(lambda: any(item['name'] == 'voice' and item['status'] == 'connected' for item in request('/mcp')), 'voice fixture did not connect')

        thread = request('/threads', {'title': 'Cloud audio', 'provider': 'haarness', 'model': 'anthropic/claude-sonnet-5-5'})['id']
        played = turn(thread, 'PLAY_THE_CLIP')
        assert played['status'] == 'completed', (played, items(thread, 'error'))
        assert items(thread, 'assistant_message')[-1]['text'] == 'NOTED', items(thread, 'assistant_message')
        sent = json.dumps(chats[-1]['messages'])
        assert re.search(r'\[Audio \S+ is left out', sent) and 'input_audio' not in sent and clip not in sent, sent
        later = turn(thread, 'SECOND_TURN')
        assert later['status'] == 'completed' and items(thread, 'assistant_message')[-1]['text'] == 'REPLY_OK', later
        check("on Haarness Cloud a tool's MP3 reaches Claude as a note, and the next turn is answered")

        thread = request('/threads', {'title': 'Cloud audio, Gemini', 'provider': 'haarness', 'model': 'google/gemini-3.5-flash'})['id']
        played = turn(thread, 'PLAY_THE_CLIP')
        assert played['status'] == 'completed', (played, items(thread, 'error'))
        sent = json.dumps(chats[-1]['messages'])
        assert '"input_audio"' in sent and clip in sent and 'is left out' not in sent, sent
        check("on Haarness Cloud Gemini gets the tool's recording itself")
    finally:
        if connection: subprocess.run(args + ['serve', '--stop'], env=cloud_env, capture_output=True, timeout=35)
        stand_in.shutdown()
        e2e_scratch.settle(base)


audio_reaches_a_model_that_cannot_hear_it()
audio_on_haarness_cloud()
