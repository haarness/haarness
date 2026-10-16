#!/usr/bin/env python3
"""Real server, echo provider: MCP results carried only as structured content reach the model and workflows.

Usage: HAARNESS_BIN=target/debug/haarness python3 scripts/test-mcp-structured.py
Set HAARNESS_E2E_DIR to keep the run's state under a chosen folder.
"""
import json, os, pathlib, subprocess, sys, tempfile, time, urllib.request, uuid
import e2e_scratch

repo = pathlib.Path(__file__).resolve().parents[1]
binary = pathlib.Path(os.environ.get('HAARNESS_BIN', 'target/debug/haarness')).resolve()
fixture = repo/'crates/haarness-mcp/tests/fixtures/structured.py'
root = pathlib.Path(tempfile.mkdtemp(prefix='haarness-mcp-structured-', dir=os.environ.get('HAARNESS_E2E_DIR'))).resolve()
workspace = root/'project'; workspace.mkdir()
state = root/'state'; (state/'config').mkdir(parents=True); (state/'data').mkdir()
(state/'config/config.toml').write_text('[defaults]\nprovider="echo"\nmodel="echo-1"\n')
env = {k: v for k, v in os.environ.items() if not k.startswith('HAARNESS_')}
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
