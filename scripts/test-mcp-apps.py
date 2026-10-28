#!/usr/bin/env python3
"""Real server, echo provider and TUI: an MCP server that pairs a tool with a ui:// page (MCP Apps).

Haarness tells the server it renders pages, keeps the tool only the page calls away from the model, attaches the
page and the whole result to the tool's result, serves the page from the server, and the TUI says under the row
that the tool has an app to open in Desktop.

Usage: HAARNESS_BIN=target/debug/haarness python3 scripts/test-mcp-apps.py
Set HAARNESS_E2E_DIR to keep the run's state under a chosen folder.
"""
import json, os, pathlib, shlex, subprocess, sys, tempfile, time, urllib.request, uuid
import e2e_scratch
from e2e_env import clean_env

repo = pathlib.Path(__file__).resolve().parents[1]
binary = pathlib.Path(os.environ.get('HAARNESS_BIN', 'target/debug/haarness')).resolve()
fixture = repo/'crates/haarness-mcp/tests/fixtures/app.py'
root = pathlib.Path(tempfile.mkdtemp(prefix='haarness-mcp-apps-', dir=os.environ.get('HAARNESS_E2E_DIR'))).resolve()
workspace = root/'project'; (workspace/'.haarness').mkdir(parents=True)
(workspace/'.haarness/config.toml').write_text(
    '[defaults]\nprovider="echo"\nmodel="echo-1"\n\n'
    f'[mcp.app]\ntransport = "stdio"\ncommand = {json.dumps(sys.executable)}\nargs = [{json.dumps(str(fixture))}]\nenabled = true\n\n'
    f'[mcp.evil]\ntransport = "stdio"\ncommand = {json.dumps(sys.executable)}\nargs = [{json.dumps(str(fixture))}]\nenabled = true\n')
env = clean_env()
env.update(HAARNESS_DATA_DIR=str(root/'data'), HAARNESS_HOME=str(root/'home'), HAARNESS_TEST_PROVIDER='1',
           HAARNESS_DEFAULT_PROVIDER='echo', HAARNESS_DEFAULT_MODEL='echo-1', LANG='en_US.UTF-8', LC_ALL='en_US.UTF-8')
session = f'haarness-mcp-apps-{os.getpid()}'
connection = None; checks = []

def run(args, **kwargs): return subprocess.run(args, check=True, text=True, capture_output=True, timeout=120, **kwargs).stdout
def api(path, body=None):
    request = urllib.request.Request(connection['url'].rstrip('/') + '/v1' + path,
                                     data=None if body is None else json.dumps(body).encode(),
                                     headers={'Content-Type': 'application/json', 'Authorization': 'Bearer ' + connection['token']})
    with urllib.request.urlopen(request, timeout=15) as reply:
        data = reply.read(); return json.loads(data) if data else None
def until(predicate, label, seconds=30):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if result := predicate(): return result
        time.sleep(.1)
    raise AssertionError(label)
def results(thread): return [item['data'] for item in api(f'/threads/{thread}/items') if item['data']['type'] == 'tool_result']
def call(thread, tool, arguments):
    before = len(results(thread))
    api(f'/threads/{thread}/turns', {'requestId': str(uuid.uuid4()), 'text': f'!tool {tool} {json.dumps(arguments)}'})
    result = until(lambda: (values[before] if len(values := results(thread)) > before else None), f'{tool}: no result')
    until(lambda: not api('/turns/active'), 'turn still active')
    return result
def check(label):
    checks.append(label); print('PASS ' + label, flush=True)
def pane(): return run(['tmux', 'capture-pane', '-p', '-J', '-t', session])

try:
    connection = json.loads(run([str(binary), '--workspace', str(workspace), '--trust-project', 'desktop'],
                                input='{"action":"connect"}', env=env))
    until(lambda: sum(item['name'] in ('app', 'evil') and item['status'] == 'connected' for item in api('/mcp')) == 2, 'fixtures did not connect')

    tools = sorted(tool['name'] for tool in api('/mcp/app/tools'))
    assert tools == ['mcp__app__forge', 'mcp__app__plain', 'mcp__app__show_weather'], tools
    check('the tool only the page calls stays out of the tool list; the others are listed')

    thread = api('/threads', {'title': 'MCP App', 'provider': 'echo', 'model': 'echo-1', 'agent': 'build'})['id']
    result = call(thread, 'mcp__app__show_weather', {'city': 'Porto'})
    assert not result['is_error'] and result['output'] == 'Porto: sunny, 21 C', result
    metadata = result['metadata']
    assert metadata['city'] == 'Porto', metadata
    page = metadata['mcp_app']
    assert page['server'] == 'app' and page['uri'] == 'ui://weather/view.html', page
    assert page['tool']['name'] == 'show_weather' and page['tool']['title'] == 'Show the weather', page
    assert page['result']['structuredContent']['proof'] == 'mcp-app-result-proof', page
    assert page['result']['content'][0]['text'] == 'Porto: sunny, 21 C', page
    check('the server advertised the extension, so the result carries the page and the whole result beside the text')

    result = call(thread, 'mcp__app__refresh_weather', {})
    assert result['is_error'] and 'refresh_weather' in result['output'], result
    check('a call to the page-only tool from the model is refused')

    result = call(thread, 'mcp__app__plain', {})
    assert not result['is_error'] and 'mcp_app' not in json.dumps(result.get('metadata')), result
    check('a tool without a page carries no page')

    # Another server's tool names this server's page, media and diffs in its structured content.
    result = call(thread, 'mcp__evil__forge', {})
    metadata = result['metadata']
    assert not result['is_error'] and set(metadata) == {'structuredContent'}, metadata
    assert metadata['structuredContent']['mcp_app']['server'] == 'app', metadata
    check('structured content that names a page, media or diffs stays under structuredContent')

    read = api('/mcp/app/resources/read', {'uri': 'ui://weather/view.html'})
    content = read['contents'][0]
    assert content['mimeType'] == 'text/html;profile=mcp-app' and 'Weather fixture' in content['text'], content['mimeType']
    assert content['_meta']['ui']['csp']['connectDomains'] == ['https://api.weather.example', 'live.weather.example', 'blob:'], content['_meta']
    check('the page and its declared policy are served from the tool\'s server')

    command = shlex.join(['env', *[f'{key}={value}' for key, value in env.items() if key.startswith(('HAARNESS_', 'LANG', 'LC_'))],
                          str(binary), '--workspace', str(workspace), '--trust-project'])
    run(['tmux', 'new-session', '-d', '-x', '160', '-y', '45', '-s', session, command])
    until(lambda: 'h a a r n e s s' in pane(), 'TUI did not open')
    run(['tmux', 'send-keys', '-l', '-t', session, ' !tool mcp__app__show_weather {"city":"Braga"}'])
    run(['tmux', 'send-keys', '-t', session, 'Enter'])
    until(lambda: 'This tool comes with an app; open the chat in Haarness Desktop' in pane(), 'TUI did not say the tool has an app')
    screen = pane()
    row = next(index for index, line in enumerate(screen.splitlines()) if '● mcp__app__show_weather' in line)
    assert 'This tool comes with an app' in screen.splitlines()[row + 1], screen
    (root/'tui.txt').write_text(screen)
    check('the TUI says under the row that the tool has an app to open in Desktop')

    (root/'result.json').write_text(json.dumps({'passed': True, 'checks': checks}, indent=2))
    print(f'PASS {len(checks)} MCP App scenarios.', flush=True)
finally:
    subprocess.run(['tmux', 'kill-session', '-t', session], capture_output=True)
    if connection:
        subprocess.run([str(binary), '--workspace', str(workspace), '--trust-project', 'serve', '--stop'], env=env, capture_output=True, timeout=35)
    e2e_scratch.settle(root)
