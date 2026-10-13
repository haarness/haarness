#!/usr/bin/env python3
"""Real daemon coverage for asks about where a path really leads, with the Echo provider.

- Hooks: a Claude Code style PreToolUse + PermissionRequest hook that allows reads unless the
  path ends in `.env`. A read of `notes.txt -> .env` still asks (seen in the attached TUI and
  rejected there), the hook is shown `.env`, and the secret never reaches the transcript.
- Extensions: a `permission.ask` extension with the same rule, in a workspace of its own.
- Bash: `touch`/`mkdir -p` of paths outside the project that do not exist yet ask
  `external_directory`.
- Agents: a running agent keeps its definition when its file is deleted, and the deletion
  applies to new turns while that turn is still running.
Every daemon runs on isolated data under one temporary root and is stopped at the end.
"""
import json, os, pathlib, shlex, subprocess, tempfile, time, urllib.error, urllib.request, uuid

REPO = pathlib.Path(__file__).resolve().parents[1]
root = pathlib.Path(tempfile.mkdtemp(prefix='haarness-resolved-targets-')).resolve()
binary = str(pathlib.Path(os.environ.get('HAARNESS_BIN', REPO / 'target/debug/haarness')).resolve())
env = {k: v for k, v in os.environ.items() if not k.startswith('HAARNESS_')}
env.update(HAARNESS_HOME=str(root / 'home'), HAARNESS_DATA_DIR=str(root / 'data'), HAARNESS_TEST_PROVIDER='1',
           HAARNESS_SERVER_TOKEN='', HAARNESS_SERVER_PASSWORD='')
CANARY = 'SECRET_CANARY_' + uuid.uuid4().hex[:8]
name = 'haarness-resolved-' + uuid.uuid4().hex[:8]
connections, checks = [], []


def write(path, content):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content)


def workspace(label, config):
    ws = root / label
    write(ws / '.haarness/config.toml', '[defaults]\nprovider="echo"\nmodel="echo-1"\n')
    write(ws / '.haarness/haarness.json', json.dumps(config, indent=1))
    write(ws / '.env', CANARY + '\n')
    (ws / 'notes.txt').symlink_to('.env')
    return ws


def cli(ws, *args, input=None):
    return subprocess.run([binary, '--workspace', str(ws), '--trust-project', *args], input=input, env=env, text=True,
                          capture_output=True, check=True, timeout=120).stdout


def connect(ws):
    c = json.loads(cli(ws, 'desktop', input='{"action":"connect"}'))
    connections.append(ws)
    return c


def api(c, path, body=None):
    req = urllib.request.Request(c['url'] + '/v1' + path, data=None if body is None else json.dumps(body).encode(),
                                 headers={'Authorization': 'Bearer ' + c.get('token', ''), 'Content-Type': 'application/json'})
    try:
        with urllib.request.urlopen(req, timeout=15) as reply:
            data = reply.read()
            return json.loads(data) if data else None
    except urllib.error.HTTPError as error:
        return {'http': error.code, 'error': error.read().decode()}


def until(fn, label, seconds=30):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        value = fn()
        if value:
            return value
        time.sleep(.1)
    raise AssertionError(label)


def check(label, condition, detail=None):
    assert condition, f'{label}: {detail}'
    checks.append(label)
    (root / 'results.json').write_text(json.dumps(checks, indent=2))
    print('PASS', label, flush=True)


def tmux(*args):
    return subprocess.run(['tmux', *args], text=True, capture_output=True, check=True, timeout=10).stdout


def capture():
    return tmux('capture-pane', '-p', '-t', name + ':0.0')


class Chat:
    def __init__(self, c, title):
        self.c = c
        self.thread = api(c, '/threads', {'title': title, 'provider': 'echo', 'model': 'echo-1'})

    def turn(self, text, **extra):
        return api(self.c, '/threads/' + self.thread['id'] + '/turns', {'requestId': str(uuid.uuid4()), 'text': text, **extra})

    def finish(self, turn):
        return until(lambda: (v if (v := api(self.c, '/turns/' + turn['id']))['status'] not in ['running', 'queued'] else None),
                     'turn completion')

    def pending(self):
        return until(lambda: api(self.c, '/permissions'), 'pending permission')[0]

    def reject(self, pending):
        api(self.c, '/permissions/' + pending['permissionId'], {'decision': 'reject'})

    def transcript(self):
        return json.dumps(api(self.c, '/threads/' + self.thread['id'] + '/items'))


try:
    # --- Hooks -----------------------------------------------------------------------------
    hook_log = root / 'hook.log'
    hook = root / 'guard-hook.py'
    write(hook, '''import json, sys
data = json.load(sys.stdin)
open(sys.argv[1], 'a').write(json.dumps(data) + '\\n')
if str(data.get('tool_input', {}).get('path', '')).endswith('.env'):
    sys.exit(0)
if data['hook_event_name'] == 'PreToolUse':
    print(json.dumps({'hookSpecificOutput': {'hookEventName': 'PreToolUse', 'permissionDecision': 'allow'}}))
else:
    print(json.dumps({'hookSpecificOutput': {'hookEventName': 'PermissionRequest', 'decision': {'behavior': 'allow'}}}))
''')
    command = shlex.join(['python3', str(hook), str(hook_log)])
    guard = [{'matcher': 'Read', 'hooks': [{'type': 'command', 'command': command}]}]
    hooks_ws = workspace('hooks', {'hooks': {'PreToolUse': guard, 'PermissionRequest': guard}})
    write(hooks_ws / 'readme.txt', 'PLAIN_FILE\n')
    c = connect(hooks_ws)
    env['HAARNESS_SERVER_TOKEN'] = c.get('token', '')
    chat = Chat(c, 'Resolved targets')
    # A plain file the hook allows reads without a prompt.
    t = chat.turn('!tool read {"path":"readme.txt"}')
    check('a plain read the hook allows runs', chat.finish(t)['status'] == 'completed' and 'PLAIN_FILE' in chat.transcript())
    attach = shlex.join([binary, '--workspace', str(root), 'attach', c['url'], '--thread', chat.thread['id']])
    launch = 'env ' + ' '.join(k + '=' + shlex.quote(env[k]) for k in
                               ['HAARNESS_HOME', 'HAARNESS_DATA_DIR', 'HAARNESS_TEST_PROVIDER', 'HAARNESS_SERVER_TOKEN', 'HAARNESS_SERVER_PASSWORD']) + ' ' + attach
    tmux('new-session', '-d', '-s', name, '-x', '150', '-y', '44', launch)
    until(lambda: 'PLAIN_FILE' in capture() or 'readme' in capture(), 'TUI attach')
    hook_log.write_text('')
    t = chat.turn('!tool read {"path":"notes.txt"}')
    pending = chat.pending()
    check('a link to .env asks although the hook allows notes.txt', pending['permission'] == 'read' and pending['patterns'] == ['.env'], pending)
    until(lambda: '.env' in capture() and ('Allow once' in capture() or 'Reject' in capture()), 'approval card in the TUI')
    (root / 'tui-approval.txt').write_text(capture())
    check('the TUI card names .env', '.env' in capture())
    inputs = [json.loads(line) for line in hook_log.read_text().splitlines() if line]
    asked = [i for i in inputs if i['hook_event_name'] == 'PermissionRequest']
    check('the PermissionRequest hook was shown .env',
          len(asked) == 1 and asked[0]['tool_input']['path'] == '.env' and asked[0]['permission_patterns'] == ['.env'], inputs)
    check('the PreToolUse hook saw the path the model gave',
          any(i['hook_event_name'] == 'PreToolUse' and i['tool_input']['path'] == 'notes.txt' for i in inputs), inputs)
    tmux('send-keys', '-t', name, 'n')
    result = chat.finish(t)
    check('rejecting in the TUI keeps the secret out of the transcript', result['status'] == 'completed' and CANARY not in chat.transcript(), result)
    # --- Bash: new paths outside the project ------------------------------------------------
    outside = root / 'outside'
    outside.mkdir()
    for label, cmd, target in [('touch', f'touch {outside}/new-file', outside / 'new-file'),
                               ('mkdir -p', f'mkdir -p {outside}/made/deeper', outside / 'made')]:
        t = chat.turn('!tool bash ' + json.dumps({'command': cmd}))
        pending = chat.pending()
        check(f'bash {label} of a new path outside the project asks',
              pending['permission'] == 'external_directory' and pending['patterns'][0].startswith(str(outside.resolve())), pending)
        chat.reject(pending)
        chat.finish(t)
        check(f'rejected bash {label} created nothing', not target.exists())
    t = chat.turn('!tool bash ' + json.dumps({'command': 'touch inside-new.txt'}))
    check('a new path inside the project does not ask', chat.finish(t)['status'] == 'completed' and (hooks_ws / 'inside-new.txt').exists())
    # --- Extensions -------------------------------------------------------------------------
    ext_log = root / 'extension.log'
    ext_ws = workspace('extension', {'extensions': ['./guard.ts']})
    write(ext_ws / '.haarness/guard.ts', '''import { appendFileSync } from 'node:fs';
export default () => ({
  apiVersion: 1, name: 'guard',
  hooks: {
    'permission.ask'(input) {
      appendFileSync(%s, JSON.stringify(input) + '\\n');
      if (input.permission === 'read' && !String(input.detail.path ?? '').endsWith('.env')) return { status: 'allow' };
    },
  },
});
''' % json.dumps(str(ext_log)))
    ec = connect(ext_ws)
    ext_chat = Chat(ec, 'Extension guard')
    t = ext_chat.turn('!tool read {"path":"notes.txt"}')
    pending = ext_chat.pending()
    check('a link to .env asks although the extension allows notes.txt', pending['patterns'] == ['.env'], pending)
    inputs = [json.loads(line) for line in ext_log.read_text().splitlines() if line]
    check('the extension was shown .env', any(i['detail']['path'] == '.env' and i['patterns'] == ['.env'] for i in inputs), inputs)
    ext_chat.reject(pending)
    ext_chat.finish(t)
    check('the extension case keeps the secret out', CANARY not in ext_chat.transcript())
    # --- Agents -----------------------------------------------------------------------------
    agents_ws = workspace('agents', {})
    reviewer = agents_ws / '.haarness/agents/reviewer.md'
    write(reviewer, '---\ndescription: Reviews without writing out.txt\nmode: all\npermission: {"edit": {"out.txt": "deny"}}\n---\nREVIEWER_PROMPT\n')
    ac = connect(agents_ws)
    agent_chat = Chat(ac, 'Agent snapshot')
    check('the reviewer agent is listed', any(a.get('id', a.get('name')) == 'reviewer' for a in api(ac, '/agents')))
    calls = [{'name': 'bash', 'arguments': {'command': 'sleep 4'}}, {'name': 'write', 'arguments': {'path': 'out.txt', 'content': 'written'}}]
    t = agent_chat.turn('!multi ' + json.dumps(calls), agent='reviewer')
    until(lambda: api(ac, '/turns/' + t['id'])['status'] == 'running', 'reviewer turn running')
    time.sleep(.5)
    reviewer.unlink()
    until(lambda: not any(a.get('id', a.get('name')) == 'reviewer' for a in api(ac, '/agents')), 'deletion applied while busy', 6)
    check('deleting the agent file applies while its turn still runs', api(ac, '/turns/' + t['id'])['status'] == 'running')
    result = agent_chat.finish(t)
    items = agent_chat.transcript()
    check('the running reviewer kept its deny rule', result['status'] == 'completed' and not (agents_ws / 'out.txt').exists()
          and "'edit:out.txt' is denied" in items, result)
    again = agent_chat.turn('hello', agent='reviewer')
    check('a new turn sees the deletion', 'Unknown agent' in json.dumps(again), again)
    print('PASS', len(checks), 'checks. Evidence:', root, flush=True)
finally:
    subprocess.run(['tmux', 'kill-session', '-t', name], capture_output=True)
    for ws in connections:
        try:
            cli(ws, 'serve', '--stop')
        except Exception as error:
            print('stop failed', ws, error, flush=True)
    print('Evidence retained:', root, flush=True)
