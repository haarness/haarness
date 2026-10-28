#!/usr/bin/env python3
"""Real server, echo provider and TUI: the October 22 MCP features end to end.

- A local server whose reply passes 64 MB is stopped (the server's memory stays bounded), its status and the call
  say why, and nothing restarts it until a reconnect; a 20 MB reply still passes.
- The model's resource tools exist only while a server declares resources, list and read through the echo
  provider, follow permission rules, and read in plain words in the TUI.
- A server with `"apps": false` is a plain MCP server (no page, its page refused, no app notice in the TUI), and
  Desktop's switch request turns its apps back on in the server's own file.
- A prompt listing or a prompt a server never answers ends at the server's timeout (the listing no longer holds every
  later request to that server, a tool call included) and the server is told to stop getting the prompt.
- `${keychain:NAME}` resolves when a server starts, from `haarness mcp secret set` or Desktop's request; a missing
  entry fails that server naming the reference; the setting stays as written and no log holds the secret.
- A tool reaches the server that lists it, with its own name: a server named `a__b` gets its tools' calls, and of two
  tools of the very same name (`a`'s `b__c` and `a__b`'s `c`) the first server's by name is offered.
- A server started with `--workspace` from another folder starts its local MCP servers in the project folder, and a
  relative `cwd` is read from it.

The keychain is a test file (HAARNESS_TEST_KEYCHAIN_FILE, honoured by debug builds only), so the script never
touches the system's keychain, and it fails if a secret reaches anything else.

Usage: HAARNESS_BIN=target/debug/haarness python3 scripts/test-mcp-servers.py
Set HAARNESS_E2E_DIR to keep the run's state under a chosen folder.
"""
import json, os, pathlib, socket, subprocess, sys, tempfile, time, urllib.error, urllib.request, uuid
import e2e_scratch, e2e_tmux
from e2e_env import clean_env

repo = pathlib.Path(__file__).resolve().parents[1]
binary = pathlib.Path(os.environ.get('HAARNESS_BIN', 'target/debug/haarness')).resolve()
fixtures = repo/'crates/haarness-mcp/tests/fixtures'
root = pathlib.Path(tempfile.mkdtemp(prefix='haarness-mcp-servers-', dir=os.environ.get('HAARNESS_E2E_DIR'))).resolve()
workspace = root/'project'; (workspace/'.haarness').mkdir(parents=True)
keychain = root/'keychain.json'
SECRET = 'ghp_e2e-' + uuid.uuid4().hex
# Quotes, backslashes and length the old `security -i` path split or mangled.
AWKWARD = 'a"b\\c\\"d \'e\' $(f) %g% ' + 'x' * 4096
python = sys.executable


def server(fixture, **extra):
    return {'command': python, 'args': [str(fixtures/fixture)], **extra}


settings = workspace/'.haarness/haarness.json'
settings.write_text(json.dumps({
    'model': 'echo/echo-1',
    'permission': {'mcp_resource_read': {'docs:docs://private*': 'deny'}, 'mcp__hidden__*': 'deny'},
    'mcp': {
        'flood': server('flood.py', env={'FLOOD_MIB': '400', 'PID_LOG': str(root/'flood-pids')}),
        'docs': server('resources.py'),
        # Every tool of it denied: its resources are kept from the model too.
        'hidden': server('resources.py'),
        'app': server('app.py', apps=False),
        'keyed': server('env.py', env={'TOKEN': '${keychain:fixture-token}', 'AWKWARD': '${keychain:awkward}',
                                       'HOME_SEEN': '${HOME}'}),
        'missing': server('env.py', env={'TOKEN': 'Bearer ${keychain:absent-token}'}),
        'silent': server('prompts.py', timeoutMs=2000, env={'CANCEL_LOG': str(root/'prompt-cancels')}),
        # `__` in a server's name: `a`'s `b__c` and `a__b`'s `c` are both `mcp__a__b__c`.
        'a': server('names.py', env={'LABEL': 'a', 'TOOLS': json.dumps(['b__c'])}),
        'a__b': server('names.py', env={'LABEL': 'a__b', 'TOOLS': json.dumps(['c', 'd'])}),
    },
}, indent=2) + '\n')
env = clean_env()
env.update(HAARNESS_DATA_DIR=str(root/'data'), HAARNESS_HOME=str(root/'home'), HAARNESS_TEST_PROVIDER='1',
           HAARNESS_TEST_KEYCHAIN_FILE=str(keychain), LANG='en_US.UTF-8', LC_ALL='en_US.UTF-8')
session = f'haarness-mcp-servers-{os.getpid()}'
connection = None; foreground = None; checks = []


def run(args, **kwargs):
    return subprocess.run(args, check=True, text=True, capture_output=True, timeout=120, env=env, **kwargs).stdout
def cli(*args, **kwargs): return run([str(binary), '--workspace', str(workspace), '--trust-project', *args], **kwargs)
def desktop(request): return json.loads(cli('desktop', input=json.dumps(request)))
def api(path, body=None, method=None, at=None):
    url, token = at or (connection['url'], connection['token'])
    request = urllib.request.Request(url.rstrip('/') + '/v1' + path, method=method,
                                     data=None if body is None else json.dumps(body).encode(),
                                     headers={'Content-Type': 'application/json', 'Authorization': 'Bearer ' + token})
    with urllib.request.urlopen(request, timeout=30) as reply:
        data = reply.read(); return json.loads(data) if data else None
def until(predicate, label, seconds=30):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if result := predicate(): return result
        time.sleep(.1)
    raise AssertionError(label)
def servers(): return {item['name']: item for item in api('/mcp')}
def results(thread): return [item['data'] for item in api(f'/threads/{thread}/items') if item['data']['type'] == 'tool_result']
def call(thread, tool, arguments):
    before = len(results(thread))
    api(f'/threads/{thread}/turns', {'requestId': str(uuid.uuid4()), 'text': f'!tool {tool} {json.dumps(arguments)}'})
    result = until(lambda: (values[before] if len(values := results(thread)) > before else None), f'{tool}: no result', 60)
    until(lambda: not api('/turns/active'), 'turn still active')
    return result
def check(label):
    checks.append(label); print('PASS ' + label, flush=True)
def pane(): return subprocess.run(['tmux', 'capture-pane', '-p', '-J', '-t', session], check=True, text=True, capture_output=True).stdout
def rss_kb(pid): return int(subprocess.run(['ps', '-o', 'rss=', '-p', str(pid)], text=True, capture_output=True).stdout.strip() or 0)
def alive(pid): return subprocess.run(['ps', '-p', str(pid)], capture_output=True).returncode == 0


try:
    # The entry `keyed` needs, stored the way a TUI user stores it: piped, never echoed.
    stored = cli('mcp', 'secret', 'set', 'fixture-token', input=SECRET + '\n')
    assert 'Stored fixture-token in the' in stored and SECRET not in stored, stored
    assert json.loads(keychain.read_text()) == {'fixture-token': SECRET}, 'the secret went somewhere else than the test keychain'
    cli('mcp', 'secret', 'set', 'awkward', input=AWKWARD + '\n')
    try:
        cli('mcp', 'secret', 'set', '--', '-leading', input='x\n')
        raise AssertionError('a name that starts with "-" was taken')
    except subprocess.CalledProcessError as error:
        assert 'start with a letter or digit' in error.stderr, error.stderr
    check('`haarness mcp secret set` stores a piped secret without showing it')

    connection = desktop({'action': 'connect'})
    until(lambda: {name: item['status'] for name, item in servers().items()}.keys() >= {'flood', 'docs', 'hidden', 'app', 'keyed', 'missing', 'silent', 'a', 'a__b'}
          and all(item['status'] in ('connected', 'failed') for item in servers().values()), 'servers did not settle', 60)
    port = connection['url'].rsplit(':', 1)[1].strip('/')
    daemon = int(subprocess.run(['lsof', '-nP', f'-iTCP:{port}', '-sTCP:LISTEN', '-t'], text=True, capture_output=True).stdout.split()[0])
    thread = api('/threads', {'title': 'MCP servers', 'provider': 'echo', 'model': 'echo-1', 'agent': 'build'})['id']

    # --- keychain secrets
    result = call(thread, 'mcp__keyed__env', {'name': 'TOKEN'})
    assert not result['is_error'] and result['output'] == SECRET, result
    assert call(thread, 'mcp__keyed__env', {'name': 'HOME_SEEN'})['output'] == os.environ['HOME']
    assert call(thread, 'mcp__keyed__env', {'name': 'AWKWARD'})['output'] == AWKWARD
    check('a ${keychain:NAME} reference reaches the starting server as the stored secret, 4 KB with quotes and backslashes included, beside ${VAR} references')
    missing = servers()['missing']
    assert missing['status'] == 'failed', missing
    assert '${keychain:absent-token} is not in the' in missing['detail'] and 'haarness mcp secret set absent-token' in missing['detail'], missing
    check('a missing entry fails only that server, naming the reference and how to store it')
    written = json.loads(settings.read_text())['mcp']
    assert written['keyed']['env']['TOKEN'] == '${keychain:fixture-token}', written
    assert desktop({'action': 'store_mcp_secret', 'name': 'absent-token', 'secret': 'desktop-secret'})['reference'] == '${keychain:absent-token}'
    # Desktop's request never replaces an entry: the next one gets the next free name, and can be taken out again.
    again = desktop({'action': 'store_mcp_secret', 'name': 'absent-token', 'secret': 'other-server'})
    assert again['name'] == 'absent-token-2', again
    assert json.loads(keychain.read_text())['absent-token'] == 'desktop-secret'
    assert desktop({'action': 'remove_mcp_secrets', 'names': ['absent-token-2']})['removed'] == 1
    assert 'absent-token-2' not in json.loads(keychain.read_text())
    assert api('/mcp/missing/reconnect', {})['status'] == 'connected'
    assert call(thread, 'mcp__missing__env', {'name': 'TOKEN'})['output'] == 'Bearer desktop-secret'
    check('Desktop\'s store request keeps a secret for a reference, and a reconnect then starts the server')

    # --- a tool reaches the server that lists it
    catalog = {tool['id']: tool['source'] for tool in api(f'/tools?threadId={thread}')['tools']}
    assert catalog.get('mcp__a__b__d') == 'mcp:a__b' and catalog.get('mcp__a__b__c') == 'mcp:a', catalog
    for tool, own_server, own_tool in [('mcp__a__b__c', 'a', 'b__c'), ('mcp__a__b__d', 'a__b', 'd')]:
        result = call(thread, tool, {})
        answer = json.loads(result['output'])
        assert not result['is_error'] and (answer['server'], answer['tool']) == (own_server, own_tool), (tool, result)
    check('a tool reaches the server that lists it, `a__b`\'s included, and is listed under it; of two of the very same name the first server\'s is offered')

    # --- interactive apps switch
    app = servers()['app']
    assert app.get('apps') is False and not app.get('appTools'), app
    result = call(thread, 'mcp__app__show_weather', {'city': 'Porto'})
    assert not result['is_error'] and result['output'] == 'Porto: sunny, 21 C' and 'mcp_app' not in json.dumps(result['metadata']), result
    try:
        api('/mcp/app/resources/read', {'uri': 'ui://weather/view.html'})
        raise AssertionError('the page of a server whose apps are off was served')
    except urllib.error.HTTPError as error:
        assert error.code == 403, error.code
    check('a server with "apps": false is a plain MCP server: no page on its results and its page refused')
    reply = desktop({'action': 'asset', 'kind': 'mcp', 'input': {'action': 'edit', 'name': 'app', 'apps': True}})
    assert reply['action'] == 'edit', reply
    assert 'apps' not in json.loads(settings.read_text())['mcp']['app'], settings.read_text()
    until(lambda: servers()['app'].get('appTools') == 1 and servers()['app'].get('apps') is None, 'the switch did not reconnect the server with its apps on', 60)
    result = call(thread, 'mcp__app__show_weather', {'city': 'Braga'})
    assert result['metadata']['mcp_app']['uri'] == 'ui://weather/view.html', result['metadata']
    check('Desktop\'s switch request turns apps on in the server\'s own file and the server reconnects with pages')

    # --- resource tools
    def tool_names(): return {tool['id'] for tool in api(f'/tools?threadId={thread}')['tools']}
    assert {'mcp_resource_list', 'mcp_resource_read'} <= tool_names()
    listing = json.loads(call(thread, 'mcp_resource_list', {})['output'])
    assert [entry['uri'] for entry in listing['resources']] == ['docs://logo', 'docs://setup'], listing
    assert listing['resources'][1]['server'] == 'docs' and listing['templates'][0]['uriTemplate'] == 'docs://page/{id}', listing
    read = call(thread, 'mcp_resource_read', {'server': 'docs', 'uri': 'docs://setup'})
    assert read['output'] == '# Setup\nRun the fixture.', read
    logo = call(thread, 'mcp_resource_read', {'server': 'docs', 'uri': 'docs://logo'})
    assert logo['output'] == '[image/png attachment: docs://logo]' and logo['metadata']['attachments'][0].startswith('data:image/png;base64,'), logo
    denied = call(thread, 'mcp_resource_read', {'server': 'docs', 'uri': 'docs://private/notes'})
    assert denied['is_error'] and 'mcp_resource_read:docs:docs://private/notes' in denied['output'] and 'denied' in denied['output'], denied
    wrong = call(thread, 'mcp_resource_read', {'server': 'keyed', 'uri': 'x://y'})
    # The app fixture serves its page as a resource, so it is listed too; `hidden`, whose tools are all denied, is not.
    assert wrong['is_error'] and 'Servers with resources: app, docs.' in wrong['output'], wrong
    hidden = call(thread, 'mcp_resource_read', {'server': 'hidden', 'uri': 'docs://setup'})
    assert hidden['is_error'] and 'MCP server "hidden" is turned off for this chat' in hidden['output'], hidden
    assert not any(entry['server'] == 'hidden' for entry in listing['resources']), listing
    check('the resource tools list and read (text, an image as an attachment), a rule about server:uri refuses a read, and a server whose tools are all denied is out of reach')
    for name in ('docs', 'app', 'hidden'):
        api(f'/mcp/{name}/disconnect', {})
    until(lambda: not {'mcp_resource_list', 'mcp_resource_read'} & tool_names(), 'the resource tools stayed without a resource server')
    api('/mcp/docs/reconnect', {})
    until(lambda: {'mcp_resource_list', 'mcp_resource_read'} <= tool_names(), 'the resource tools did not come back')
    api('/mcp/app/reconnect', {})
    api('/mcp/hidden/reconnect', {})
    check('the resource tools are offered only while a server declares resources')

    # --- prompt requests a server never answers
    def refused(path, body=None):
        started = time.monotonic()
        try:
            api(path, body)
        except urllib.error.HTTPError as error:
            return error.code, json.loads(error.read())['message'], time.monotonic() - started
        raise AssertionError(f'{path} answered')
    status, message, waited = refused('/mcp/silent/prompts')
    assert status == 422 and message == 'tool error: MCP server "silent" did not answer within 2 s', (status, message)
    assert waited < 10, waited
    try:
        cli('mcp', 'prompts', 'silent')
        raise AssertionError('`haarness mcp prompts` answered')
    except subprocess.CalledProcessError as error:
        assert 'MCP server "silent" did not answer within 2 s' in error.stderr, error.stderr
    started = time.monotonic()
    assert call(thread, 'mcp__silent__ping', {})['output'] == 'pong'
    assert time.monotonic() - started < 10, 'the tool call waited behind the listing'
    check(f'a prompt listing the server never answers ends after its 2 s timeout ({waited:.1f} s), in the CLI too, and its next tool call answers at once')
    status, message, waited = refused('/mcp/silent/prompts/review', {})
    assert status == 422 and 'did not answer within 2 s' in message, (status, message)
    until(lambda: (root/'prompt-cancels').exists() and 'cancelled' in (root/'prompt-cancels').read_text(),
          'the server was not told to stop getting the prompt', 10)
    check('getting a prompt the server never answers ends after its timeout and the server is told to stop')

    # --- the 64 MB message limit
    big = call(thread, 'mcp__flood__big', {})
    assert not big['is_error'] and len(big['output']) == 20 << 20, len(big['output'])
    before = rss_kb(daemon)
    started = time.monotonic()
    flood = call(thread, 'mcp__flood__flood', {})
    elapsed = time.monotonic() - started
    peak = rss_kb(daemon)
    assert flood['is_error'] and 'sent a reply larger than 64 MB, so Haarness stopped it' in flood['output'], flood
    assert 'Calling it again fails until the user reconnects it.' in flood['output'], flood
    status = until(lambda: servers()['flood'] if servers()['flood']['status'] == 'failed' else None, 'flood stayed connected')
    assert status['detail'] == 'Its reply was larger than 64 MB, so Haarness stopped it. Reconnect it to use it again.', status
    pids = [int(pid) for pid in (root/'flood-pids').read_text().split()]
    until(lambda: not any(alive(pid) for pid in pids), 'the flooding server kept running', 10)
    # A 400 MiB flood without the limit would hold all of it, copied as it grows.
    assert peak - before < 250 * 1024, f'memory grew by {(peak - before) // 1024} MB'
    # A reload for another change (a server added) leaves it stopped.
    edited = json.loads(settings.read_text())
    edited['mcp']['probe'] = server('env.py')
    settings.write_text(json.dumps(edited, indent=2) + '\n')
    until(lambda: servers().get('probe', {}).get('status') == 'connected', 'the reload did not happen', 60)
    assert servers()['flood']['detail'] == status['detail'], servers()['flood']
    time.sleep(1.5)
    again = call(thread, 'mcp__flood__ping', {})
    assert again['is_error'] and 'larger than 64 MB' in again['output'], again
    assert len((root/'flood-pids').read_text().split()) == 1, 'something restarted the server'
    check(f'a 400 MiB reply stops its server in {elapsed:.1f} s with memory bounded ({max(peak - before, 0) // 1024} MB), says why, and stays stopped, also through a reload')
    assert api('/mcp/flood/reconnect', {})['status'] == 'connected'
    assert call(thread, 'mcp__flood__ping', {})['output'] == 'pong'
    check('a reconnect brings the stopped server back')

    # --- TUI rows
    command = e2e_tmux.pane_command(env, [binary, '--workspace', workspace, '--trust-project'])
    subprocess.run(['tmux', 'new-session', '-d', '-x', '160', '-y', '45', '-s', session, command], check=True)
    until(lambda: 'h a a r n e s s' in pane(), 'TUI did not open')
    for text in ['!tool mcp_resource_read {"server":"docs","uri":"docs://setup"}', '!tool mcp_resource_list {"server":"docs"}']:
        subprocess.run(['tmux', 'send-keys', '-l', '-t', session, ' ' + text], check=True)
        subprocess.run(['tmux', 'send-keys', '-t', session, 'Enter'], check=True)
        time.sleep(1)
    until(lambda: '● mcp_resource_read docs://setup' in pane() and '● mcp_resource_list docs' in pane(), 'TUI rows did not name the resource and server')
    (root/'tui.txt').write_text(pane())
    if os.environ.get('HAARNESS_SCREENSHOTS'):
        (pathlib.Path(os.environ['HAARNESS_SCREENSHOTS'])/'tui-mcp-resource-rows.txt').write_text(pane())
    check('the TUI names the resource read and the server listed beside the tool')

    # Nothing wrote the secret anywhere it does not belong. The chat's database holds what the fixture's tool
    # answered (the secret, by design), so only logs and settings are read.
    files = [path for path in [*(root/'data').rglob('*'), *(root/'home').rglob('*'), settings]
             if path.is_file() and '.db' not in path.name and path.stat().st_size < 64 << 20]
    assert any(path.suffix in ('.log', '.jsonl') or 'log' in path.name for path in files), 'no log was written to read'
    for path in files:
        assert SECRET.encode() not in path.read_bytes(), f'the secret is in {path}'
    check('no log or setting holds the secret')

    # --- a server started from another folder
    other = root/'other'; (other/'.haarness').mkdir(parents=True); (other/'sub').mkdir()
    elsewhere = root/'elsewhere'; elsewhere.mkdir()
    (other/'.haarness/haarness.json').write_text(json.dumps({'model': 'echo/echo-1', 'mcp': {
        'here': server('names.py', env={'LABEL': 'here', 'TOOLS': '["where"]'}),
        'inside': server('names.py', cwd='sub', env={'LABEL': 'inside', 'TOOLS': '["where"]'}),
    }}, indent=2) + '\n')
    with socket.socket() as probe:
        probe.bind(('127.0.0.1', 0)); free = probe.getsockname()[1]
    token = uuid.uuid4().hex
    with open(root/'foreground.log', 'w') as log:
        foreground = subprocess.Popen([str(binary), '--workspace', str(other), '--trust-project', 'serve', '--port', str(free)],
                                      cwd=elsewhere, env={**env, 'HAARNESS_SERVER_TOKEN': token}, stdout=log, stderr=log)
    at = (f'http://127.0.0.1:{free}', token)
    def settled_elsewhere():
        try:
            listed = {item['name']: item['status'] for item in api('/mcp', at=at)}
        except (urllib.error.URLError, ConnectionError):
            return None
        return listed if set(listed) == {'here', 'inside'} and 'connecting' not in listed.values() else None
    listed = until(settled_elsewhere, 'the server started elsewhere did not list its MCP servers', 60)
    assert set(listed.values()) == {'connected'}, listed
    for name, expected in [('here', other), ('inside', other/'sub')]:
        answer = json.loads(api(f'/mcp/{name}/tools/mcp__{name}__where', {}, at=at)['text'])
        assert pathlib.Path(answer['cwd']).resolve() == expected.resolve(), (name, answer)
    check('a server started with --workspace from another folder starts its MCP servers in the project folder, and a relative cwd inside it')
    foreground.terminate(); foreground.wait(timeout=30); foreground = None

    (root/'result.json').write_text(json.dumps({'passed': True, 'checks': checks}, indent=2))
    print(f'PASS {len(checks)} MCP server scenarios.', flush=True)
finally:
    subprocess.run(['tmux', 'kill-session', '-t', session], capture_output=True)
    if foreground:
        foreground.terminate()
        try:
            foreground.wait(timeout=30)
        except subprocess.TimeoutExpired:
            foreground.kill()
    if connection:
        subprocess.run([str(binary), '--workspace', str(workspace), '--trust-project', 'serve', '--stop'], env=env, capture_output=True, timeout=35)
    e2e_scratch.settle(root)
