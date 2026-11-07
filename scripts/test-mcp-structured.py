#!/usr/bin/env python3
"""Real server, echo provider: MCP results carried only as structured content reach the model and workflows;
resources embedded as base64 reach the model as what they hold (an image as an attachment, text as text, anything
else named), never as base64; audio of a type Haarness does not carry is named rather than attached; and a 200 KB result reaches the model cut with the whole saved, while a workflow gets
all of it. One-line JSON is laid out a value a line within a few times its length, however
deep it nests.

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
    check('what a Code Mode script returns is cut for the model too')

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

    # Two spaces a level would make this 1.8 GB; past 32 levels the rest stays on its line.
    started = time.monotonic()
    result = call(thread, 'nested')
    assert not result['is_error'] and len(result['output']) < 52 * 1024, len(result['output'])
    saved = pathlib.Path(result['metadata']['outputPath']).read_text()
    assert len(saved) < 2 * 60000 and len(saved.splitlines()) == 65, (len(saved), len(saved.splitlines()))
    assert saved.replace('\n', '').replace(' ', '') == '[' * 30000 + ']' * 30000
    assert time.monotonic() - started < 20, time.monotonic() - started
    check('JSON nested 30,000 deep is laid out 32 levels down, its saved file about as long as the server\'s text')

    result = call(thread, 'loud_failure')
    assert result['is_error'] and len(result['output']) < 52 * 1024, len(result['output'])
    assert 'The tool call failed and its output was truncated. Full output saved to: ' in result['output'], result['output'][-300:]
    check('a long failure is cut for the model as well, and says the call failed')

    (root/'result.json').write_text(json.dumps({'passed': True, 'checks': checks}, indent=2))
    print(f'PASS {len(checks)} MCP structured-content scenarios.', flush=True)
finally:
    if url: subprocess.run([str(binary), '--workspace', str(workspace), 'serve', '--stop'], env=env, capture_output=True, timeout=35)
    e2e_scratch.settle(root)
