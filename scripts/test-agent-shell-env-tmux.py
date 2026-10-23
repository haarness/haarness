#!/usr/bin/env python3
"""Real TUI in tmux, echo provider: a command the agent runs says it is an agent's and names its chat.

The TUI starts the way a `haarness` started by another chat's agent command does: its environment
holds that command's markers, AGENT=1, HAARNESS=1, AI_AGENT=haarness and the other chat's
HAARNESS_SESSION_ID. The server it starts leaves them out. In its chat, the model's command, a slash
command's !`…` block, a background child agent's command and a workflow's `shell()` each get
AGENT=1, HAARNESS=1, AI_AGENT=haarness and the id of the chat they run in; the person's own `!`
command, a terminal, an MCP server, a language server and a formatter get no chat id and no markers.
The server logs one `agent shell environment set` line per agent command, with no outer marker kept.
Then `haarness run` under an outer agent (AI_AGENT=outer-agent, AGENT=1) keeps that agent's
AI_AGENT, in its commands and in the person's own, and logs that it did.

Usage: HAARNESS_BIN=target/debug/haarness python3 scripts/test-agent-shell-env-tmux.py
Set HAARNESS_E2E_DIR to keep the run's state under a chosen folder.
"""
import json, os, pathlib, re, subprocess, tempfile, time, urllib.request, uuid
import e2e_scratch, e2e_tmux

repository = pathlib.Path(__file__).resolve().parents[1]
binary = pathlib.Path(os.environ.get('HAARNESS_BIN', 'target/debug/haarness')).resolve()
fixtures = repository/'crates/haarness-language/tests/fixtures'
root = pathlib.Path(tempfile.mkdtemp(prefix='haarness-agent-shell-', dir=os.environ.get('HAARNESS_E2E_DIR'))).resolve()
project = root/'project'; outer = root/'outer'; state = root/'state'; home = root/'home'
for folder in (project/'.haarness/commands', project/'.haarness/workflows', outer, state, home):
    folder.mkdir(parents=True, exist_ok=True)
session = 'haarness-agent-shell-' + uuid.uuid4().hex[:8]
checks = []
STALE = 'stale-outer-chat'
# What a process saw, written to `file`: AGENT, HAARNESS, AI_AGENT and HAARNESS_SESSION_ID.
def show(file):
    return (f"printf '%s|%s|%s|%s' \"${{AGENT-unset}}\" \"${{HAARNESS-unset}}\" \"${{AI_AGENT-unset}}\" "
            f"\"${{HAARNESS_SESSION_ID-unset}}\" > '{file}'")
UNMARKED = 'unset|unset|unset|unset'

(project/'.haarness/commands/marks.md').write_text(
    "---\ndescription: Show the agent markers\n---\nMARKS !`" + show(project/'snippet.txt') + f"; cat '{project/'snippet.txt'}'`\n")
(project/'.haarness/workflows/shellmarks.js').write_text(
    'export const meta = { name: "shellmarks", description: "Show the agent markers" }\n\n'
    f'await shell({json.dumps(show(project/"workflow.txt"))})\nreturn "marked"\n')
record = lambda file: f"{show(file)}; exec python3 \"$@\""
(project/'.haarness/config.toml').write_text(f'''
[mcp.envdump]
transport = "stdio"
command = "sh"
args = ["-c", {json.dumps(show(project/'mcp.txt'))}]
enabled = true

[lsp]
enabled = true
builtins = false

[lsp.servers.mock]
command = ["sh", "-c", {json.dumps(record(project/'lsp.txt'))}, "sh", {json.dumps(str(fixtures/'mock_lsp.py'))}]
extensions = [".mock"]

[lsp.servers.mock.language_ids]
mock = "mock-language"

[formatter]
enabled = true
builtins = false

[formatter.formatters.mock]
command = ["sh", "-c", {json.dumps(record(project/'formatter.txt'))}, "sh", {json.dumps(str(fixtures/'mock_formatter.py'))}, "$FILE"]
extensions = [".mock"]
''')

def run(args, **kwargs): return subprocess.run(args, check=True, text=True, capture_output=True, timeout=60, **kwargs).stdout
def capture(): return run(['tmux', 'capture-pane', '-p', '-J', '-t', session])
def keys(*values): run(['tmux', 'send-keys', '-t', session, *values])
def paste(text):
    """A prompt as a paste, so its leading `!` does not open shell mode."""
    run(['tmux', 'load-buffer', '-'], input=' ' + text)
    run(['tmux', 'paste-buffer', '-p', '-t', session]); keys('Enter')
def until(predicate, label, seconds=45):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        value = predicate()
        if value: return value
        time.sleep(.2)
    raise AssertionError(f'{label}\n{capture()}')
def read(path): return path.read_text() if path.exists() else None
def api(workspace):
    """Requests to `workspace`'s server, read from where its TUI started it."""
    registry = json.loads((state/'data/workspaces.json').read_text())
    identity = next(item['id'] for item in registry['workspaces'] if item['path'] == str(workspace))
    runtime = json.loads((state/'cache/workspaces'/identity/'runtime.json').read_text())
    def call(path, body=None):
        request = urllib.request.Request(runtime['url'] + '/v1' + path,
            data=None if body is None else json.dumps(body).encode(),
            headers={'Authorization': 'Bearer ' + runtime['token'], 'Content-Type': 'application/json'})
        with urllib.request.urlopen(request, timeout=15) as reply: return json.loads(reply.read() or b'null')
    return call
def connect(workspace):
    try:
        call = api(workspace); call('/threads'); return call
    except (OSError, ValueError, StopIteration, KeyError):
        return None
def logged():
    """The server's `agent shell environment set` lines, by the chat they name."""
    lines = []
    for file in state.rglob('haarness.log*'):
        lines += [line for line in file.read_text(errors='replace').splitlines() if 'agent shell environment set' in line]
    return lines
def check(label):
    checks.append(label); print('PASS ' + label, flush=True)
env = {**os.environ, 'HAARNESS_DATA_DIR': str(state), 'HAARNESS_HOME': str(home), 'HOME': str(home),
       'HAARNESS_TEST_PROVIDER': '1', 'HAARNESS_DEFAULT_PROVIDER': 'echo', 'HAARNESS_DEFAULT_MODEL': 'echo-1',
       'HAARNESS_NO_CONNECT_PROMPT': '1'}
stopped = []

try:
    # A `haarness` that another chat's agent command started.
    nested = {**env, 'AGENT': '1', 'HAARNESS': '1', 'AI_AGENT': 'haarness', 'HAARNESS_SESSION_ID': STALE}
    command = e2e_tmux.pane_command(nested, [binary, '--workspace', project, '--trust-project'])
    run(['tmux', 'new-session', '-d', '-x', '200', '-y', '50', '-c', str(root), '-s', session, command])
    stopped.append(project)
    until(lambda: 'echo' in capture().lower(), 'TUI did not open')
    call = until(lambda: connect(project), 'the server did not start')

    paste('!tool bash ' + json.dumps({'command': show(project/'model.txt')}))
    seen = until(lambda: read(project/'model.txt'), 'the model command did not run')
    chats = [thread for thread in call('/threads') if not thread.get('parentId')]
    assert len(chats) == 1, chats
    chat = chats[0]['id']
    assert seen == f'1|1|haarness|{chat}', seen
    check("the model's command is marked as an agent's and names its chat, not the one the TUI inherited")

    keys('!'); until(lambda: 'shell' in capture(), 'shell mode did not open')
    run(['tmux', 'send-keys', '-l', '-t', session, show(project/'person.txt')]); keys('Enter')
    seen = until(lambda: read(project/'person.txt'), "the person's command did not run")
    assert seen == UNMARKED, seen
    check("the person's own ! command is not marked and gets no chat id")

    run(['tmux', 'send-keys', '-l', '-t', session, '/marks']); keys('Enter')
    seen = until(lambda: read(project/'snippet.txt'), 'the slash command block did not run')
    assert seen == f'1|1|haarness|{chat}', seen
    until(lambda: f'Echo: MARKS 1|1|haarness|{chat}' in capture(), 'the slash command reply did not show the markers')
    check("a slash command's !`…` block is marked and names the chat")

    task = {'agent': 'general', 'background': True,
            'prompt': '!tool bash ' + json.dumps({'command': show(project/'background.txt')})}
    paste('!tool task ' + json.dumps(task))
    seen = until(lambda: read(project/'background.txt'), "the background child's command did not run")
    children = [thread['id'] for thread in call('/threads?archived=true') if thread.get('parentId') == chat]
    assert len(children) == 1 and seen == f'1|1|haarness|{children[0]}', (seen, children)
    check("a background child agent's command names the child's own chat")

    run(['tmux', 'send-keys', '-l', '-t', session, '/shellmarks']); keys('Enter')
    seen = until(lambda: read(project/'workflow.txt'), "the workflow's shell() did not run")
    runs = [thread['id'] for thread in call('/threads?archived=true')
            if thread.get('parentId') == chat and thread['id'] not in children]
    assert len(runs) == 1 and seen == f'1|1|haarness|{runs[0]}', (seen, runs)
    check("a workflow's shell() is marked and names the workflow's own chat")
    # The run's dashboard opened with it; Esc goes back to the composer.
    until(lambda: 'esc close' in capture(), 'the workflow dashboard did not open')
    keys('Escape'); until(lambda: 'esc close' not in capture(), 'the workflow dashboard did not close')

    paste('!tool write ' + json.dumps({'path': 'sample.mock', 'content': 'entity BADFMT BROKEN\n'}))
    until(lambda: read(project/'lsp.txt') and read(project/'formatter.txt'), 'the language server and formatter did not start')
    assert 'GOODFMT' in (project/'sample.mock').read_text()
    assert read(project/'lsp.txt') == UNMARKED and read(project/'formatter.txt') == UNMARKED, (read(project/'lsp.txt'), read(project/'formatter.txt'))
    check('a language server and a formatter get no chat id and no markers')

    assert until(lambda: read(project/'mcp.txt'), 'the MCP server did not start') == UNMARKED, read(project/'mcp.txt')
    check('an MCP server gets no chat id and no markers')

    call('/ptys', {'command': 'sh', 'args': ['-c', show(project/'terminal.txt')]})
    assert until(lambda: read(project/'terminal.txt'), 'the terminal did not start') == UNMARKED, read(project/'terminal.txt')
    check('a terminal gets no chat id and no markers')

    # Model, slash command block, background child and workflow: one line each, none with an outer
    # marker (the TUI's AI_AGENT=haarness was Haarness's own) or a stale id; the person's command and
    # the write tool log none.
    lines = until(lambda: len(logged()) >= 4 and logged(), 'the server did not log the agent commands')
    time.sleep(1)
    lines = logged()
    assert len(lines) == 4, lines
    named = sorted(re.search(r'thread_id=(\S+)', line).group(1) for line in lines)
    assert named == sorted([chat, chat, children[0], runs[0]]), (named, lines)
    assert all('outer_marker_kept=false stale_id_replaced=false' in line and ' INFO ' in line for line in lines), lines
    assert not any(STALE in line for line in lines), lines
    check('the server logs one line per agent command with its chat id, no outer marker and no stale id')
    (root/'pane.txt').write_text(capture())

    # `haarness run` under an outer agent keeps its marker.
    outer_env = {**env, 'AI_AGENT': 'outer-agent', 'AGENT': '1'}
    outer_env.pop('HAARNESS_SESSION_ID', None)
    stopped.append(outer)
    reply = run([str(binary), '--workspace', str(outer), '--trust-project', 'run',
                 '!tool bash ' + json.dumps({'command': show(outer/'run.txt')})], env=outer_env, cwd=outer)
    seen = read(outer/'run.txt')
    threads = api(outer)('/threads')
    assert len(threads) == 1 and seen == f"1|1|outer-agent|{threads[0]['id']}", (seen, threads, reply)
    line = next(line for line in logged() if threads[0]['id'] in line)
    assert 'outer_marker_kept=true stale_id_replaced=false' in line and 'outer-agent' not in line, line
    api(outer)(f"/threads/{threads[0]['id']}/shell", {'command': show(outer/'person.txt')})
    seen = until(lambda: read(outer/'person.txt'), "the person's command did not run")
    assert seen == 'unset|unset|outer-agent|unset', seen
    check("haarness run under an outer agent keeps that agent's AI_AGENT, in its command and the person's, and logs it")
    print(f'PASS {len(checks)} agent shell environment scenarios.', flush=True)
finally:
    subprocess.run(['tmux', 'kill-session', '-t', session], capture_output=True)
    for workspace in stopped:
        subprocess.run([str(binary), '--workspace', str(workspace), 'serve', '--stop'], capture_output=True, timeout=35,
                       env={**os.environ, 'HAARNESS_DATA_DIR': str(state), 'HAARNESS_HOME': str(home), 'HOME': str(home)})
    e2e_scratch.settle(root)
