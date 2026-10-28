#!/usr/bin/env python3
"""Real daemon and TUI coverage for what hooks can answer, with the Echo provider and a mock server.

- Always: a PermissionRequest hook told it may answer Always (with the pattern Always keeps and
  Claude Code's permission_suggestions) answers Always for `echo one`; `echo two` then runs without
  asking it or anyone. An edit of Haarness's own config offers the hook no Always and still asks.
- Questions: a PreToolUse hook answers a known question; the agent gets the answer, nobody is asked,
  and the attached TUI shows "Answered by hook". Another question is left to the person, who answers
  it in the TUI.
- StopFailure: a provider's HTTP 429 reaches hooks for `rate_limit`, for `provider` and for Claude
  Code's `billing_error`, with `error` still `provider` and `error_type` set to `rate_limit`.
- Reasoning: a custom OpenAI-compatible server that streams `reasoning` gets it back in that field
  with the tool call in the next request.
- Matchers: `/hooks` in the TUI marks a SessionEnd hook for `clear` as never running.
- After a tool ran: a PostToolUse hook that exits 2 cannot undo the call; its stderr follows the
  tool's result, reaches the model in the next request (a mock server records it) and shows in the
  TUI. A person's rejected `!` command runs PostToolUseFailure hooks, told that event's name. So does a
  tool that fails in the model's turn: the hook is told the error, and its exit-2 stderr follows that
  error, which stays an error, and reaches the model in the next request.
Every daemon runs on isolated data under one temporary root and is stopped at the end.
"""
import json, os, pathlib, shlex, subprocess, sys, tempfile, threading, time, urllib.error, urllib.request, uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import e2e_scratch
from e2e_tmux import pane_command
from e2e_env import clean_env

REPO = pathlib.Path(__file__).resolve().parents[1]
root = pathlib.Path(tempfile.mkdtemp(prefix='haarness-hook-answers-')).resolve()
binary = str(pathlib.Path(os.environ.get('HAARNESS_BIN', REPO / 'target/debug/haarness')).resolve())
env = clean_env()
env.update(HAARNESS_HOME=str(root / 'home'), HAARNESS_DATA_DIR=str(root / 'data'), HAARNESS_TEST_PROVIDER='1',
           HAARNESS_SERVER_TOKEN='', HAARNESS_SERVER_PASSWORD='')
session = 'haarness-hooks-' + uuid.uuid4().hex[:8]
connections, checks, requests = [], [], []


def write(path, content):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content)


def check(label, condition, detail=None):
    assert condition, f'{label}: {detail}'
    checks.append(label)
    (root / 'results.json').write_text(json.dumps(checks, indent=2))
    print('PASS', label, flush=True)


def until(fn, label, seconds=30):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        value = fn()
        if value:
            return value
        time.sleep(.1)
    raise AssertionError(label)


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


def tmux(*args):
    return subprocess.run(['tmux', *args], text=True, capture_output=True, check=True, timeout=10).stdout


def capture():
    return tmux('capture-pane', '-p', '-t', session + ':0.0')


class Chat:
    def __init__(self, c, title, provider='echo', model='echo-1'):
        self.c = c
        self.thread = api(c, '/threads', {'title': title, 'provider': provider, 'model': model})

    def turn(self, text):
        return api(self.c, '/threads/' + self.thread['id'] + '/turns', {'requestId': str(uuid.uuid4()), 'text': text})

    def finish(self, turn):
        return until(lambda: (v if (v := api(self.c, '/turns/' + turn['id']))['status'] not in ['running', 'queued'] else None),
                     'turn completion')

    def items(self):
        return api(self.c, '/threads/' + self.thread['id'] + '/items')

    def results(self, name):
        return [i['data'] for i in self.items() if i['data']['type'] == 'tool_result' and i['data']['name'] == name]


class Mock(BaseHTTPRequestHandler):
    """An OpenAI-compatible server: `limited` answers 429; `thinker` streams its reasoning in
    `reasoning`, then lists the folder, and answers once the listing comes back; `linter` runs a
    command and `reader` reads a file that is not there."""

    def log_message(self, *args):
        pass

    def reply(self, status, body, content_type='application/json'):
        data = body.encode()
        self.send_response(status)
        self.send_header('Content-Type', content_type)
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        self.reply(200, json.dumps({'data': [{'id': 'thinker'}, {'id': 'limited'}, {'id': 'linter'}, {'id': 'reader'}]}))

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        requests.append(body)
        if body['model'] == 'limited':
            return self.reply(429, json.dumps({'error': {'message': 'Rate limit reached for requests', 'type': 'requests'}}))
        if any(m['role'] == 'tool' for m in body['messages']):
            chunks = [{'choices': [{'delta': {'content': 'Listed.'}}]}, {'choices': [{'delta': {}, 'finish_reason': 'stop'}]}]
        elif body['model'] == 'linter':
            chunks = [{'choices': [{'delta': {'tool_calls': [{'index': 0, 'id': 'call_lint', 'function': {
                          'name': 'bash', 'arguments': json.dumps({'command': 'echo lint-me'})}}]}}]},
                      {'choices': [{'delta': {}, 'finish_reason': 'tool_calls'}]}]
        elif body['model'] == 'reader':
            chunks = [{'choices': [{'delta': {'tool_calls': [{'index': 0, 'id': 'call_read', 'function': {
                          'name': 'read', 'arguments': json.dumps({'path': 'missing.txt'})}}]}}]},
                      {'choices': [{'delta': {}, 'finish_reason': 'tool_calls'}]}]
        else:
            chunks = [{'choices': [{'delta': {'reasoning': 'List the folder '}}]},
                      {'choices': [{'delta': {'reasoning': 'before answering.'}}]},
                      {'choices': [{'delta': {'tool_calls': [{'index': 0, 'id': 'call_list', 'function': {
                          'name': 'list', 'arguments': json.dumps({'path': '.'})}}]}}]},
                      {'choices': [{'delta': {}, 'finish_reason': 'tool_calls'}]}]
        self.reply(200, ''.join(f'data: {json.dumps(c)}\n\n' for c in chunks) + 'data: [DONE]\n\n', 'text/event-stream')


mock = ThreadingHTTPServer(('127.0.0.1', 0), Mock)
threading.Thread(target=mock.serve_forever, daemon=True).start()

try:
    log = root / 'hook-log.jsonl'
    hook = root / 'hook.py'
    # Answers Always the first time it is asked about a command and refuses any later ask, answers
    # one known question, and records every payload it gets.
    write(hook, '''import json, os, sys
data = json.load(sys.stdin)
open(sys.argv[1], 'a').write(json.dumps(data) + '\\n')
event = data['hook_event_name']
def say(value):
    print(json.dumps({'hookSpecificOutput': dict(value, hookEventName=event)}))
if event == 'PermissionRequest' and data['tool_name'] == 'bash':
    marker = sys.argv[1] + '.answered'
    if os.path.exists(marker):
        say({'decision': {'behavior': 'deny', 'message': 'asked again'}})
    else:
        open(marker, 'w').close()
        say({'decision': {'behavior': 'allow', 'updatedPermissions': data.get('permission_suggestions', [])}})
elif event == 'PermissionRequest':
    say({'decision': {'behavior': 'allow', 'always': True}})
elif event == 'PreToolUse' and data['tool_input'].get('prompt') == 'Which test suite should I run?':
    say({'updatedInput': dict(data['tool_input'], answers=[['Unit tests']])})
elif event == 'StopFailure':
    print('{}')
elif event == 'PostToolUse' and 'lint-me' in data['tool_input'].get('command', ''):
    print('lint: 2 warnings in notes.md', file=sys.stderr)
    sys.exit(2)
elif event == 'PostToolUseFailure' and data['tool_name'] == 'read':
    print('nothing at ' + data['tool_input']['path'] + ': list the folder first', file=sys.stderr)
    sys.exit(2)
elif event == 'PostToolUseFailure':
    print('refusal noted by ' + event, file=sys.stderr)
    sys.exit(2)
''')
    command = shlex.join(['python3', str(hook), str(log)])
    group = lambda matcher: [{'matcher': matcher, 'hooks': [{'type': 'command', 'command': command}]}]
    ws = root / 'project'
    # Chats start in Auto, where a hook's Always counts once; Review offers the hook Always.
    write(ws / '.haarness/config.toml', f'''[defaults]
provider="echo"
model="echo-1"
approval_mode="review"
[permissions]
bash="ask"
[providers.mock]
protocol="openai-chat-completions"
name="Mock"
base_url="http://127.0.0.1:{mock.server_address[1]}/v1"
models=["thinker","limited","linter","reader"]
requires_auth=false
reasoning=true
tools=true
''')
    write(ws / 'haarness.json', json.dumps({'hooks': {
        'PermissionRequest': group('*'), 'PreToolUse': group('question'),
        'StopFailure': group('rate_limit') + group('provider') + group('billing_error'),
        'SessionEnd': group('clear'), 'PostToolUse': group('Bash'), 'PostToolUseFailure': group('Bash') + group('Read')}}, indent=1))
    c = connect(ws)
    env['HAARNESS_SERVER_TOKEN'] = c.get('token', '')
    entries = lambda event: [json.loads(line) for line in log.read_text().splitlines() if line and json.loads(line)['hook_event_name'] == event] if log.exists() else []

    # --- Always ---------------------------------------------------------------------------
    chat = Chat(c, 'Hook answers')
    attach = [binary, '--workspace', str(root), 'attach', c['url'], '--thread', chat.thread['id']]
    tmux('new-session', '-d', '-s', session, '-x', '150', '-y', '44', pane_command(env, attach))
    until(lambda: 'Hook answers' in capture() or 'approvals' in capture(), 'TUI attach')
    t = chat.turn('!tool bash ' + json.dumps({'command': 'echo one'}))
    check('the hook answers Always for echo one', chat.finish(t)['status'] == 'completed'
          and any('one' in r['output'] and not r['is_error'] for r in chat.results('bash')), chat.results('bash'))
    asked = entries('PermissionRequest')
    check('the hook was told it may answer Always and what Always keeps',
          len(asked) == 1 and asked[0]['allow_always'] is True and asked[0]['always_patterns'] == ['echo *']
          and asked[0]['permission_suggestions'][0]['rules'][0]['ruleContent'] == 'echo *', asked)
    t = chat.turn('!tool bash ' + json.dumps({'command': 'echo two'}))
    check('echo two runs on that Always, without asking the hook or anyone',
          chat.finish(t)['status'] == 'completed' and len(entries('PermissionRequest')) == 1
          and any('two' in r['output'] and not r['is_error'] for r in chat.results('bash')), chat.results('bash'))
    check('no approval card reached the TUI', 'Allow once' not in capture() and not api(c, '/permissions'), capture())
    t = chat.turn('!tool write ' + json.dumps({'path': 'haarness.json', 'content': '{}'}))
    pending = until(lambda: api(c, '/permissions'), 'config edit asks')[0]
    config_ask = entries('PermissionRequest')[-1]
    check('an edit of Haarness config offers the hook no Always and still asks the person',
          pending['permission'] == 'edit' and config_ask['allow_always'] is False and 'always_patterns' not in config_ask, config_ask)
    api(c, '/permissions/' + pending['permissionId'], {'decision': 'reject'})
    chat.finish(t)
    check('the rejected config edit changed nothing', json.loads((ws / 'haarness.json').read_text()).get('hooks'))

    # --- Questions ------------------------------------------------------------------------
    question = {'prompt': 'Which test suite should I run?', 'choices': ['Unit tests', 'Full suite']}
    t = chat.turn('!tool question ' + json.dumps(question))
    check('the hook answers the known question without asking', chat.finish(t)['status'] == 'completed'
          and not api(c, '/questions'), api(c, '/questions'))
    answered = chat.results('question')[-1]
    check('the agent gets the hook answer, marked as the hook', answered['output'].startswith('Unit tests')
          and answered['metadata'] == {'answeredBy': 'hook'}, answered)
    until(lambda: 'Answered by hook' in capture(), 'TUI shows the hook answer')
    (root / 'tui-answered-by-hook.txt').write_text(capture())
    check('the TUI row says the hook answered it', 'Which test suite should I run?' in capture(), capture())
    t = chat.turn('!tool question ' + json.dumps({'prompt': 'Which color?', 'choices': ['Blue', 'Green']}))
    until(lambda: api(c, '/questions'), 'the other question is asked')
    until(lambda: 'Which color?' in capture() and 'Blue' in capture(), 'TUI question card')
    (root / 'tui-question-card.txt').write_text(capture())
    tmux('send-keys', '-t', session, 'Enter')
    check('the person answers the other question in the TUI', chat.finish(t)['status'] == 'completed'
          and chat.results('question')[-1]['output'].startswith('Blue')
          and not chat.results('question')[-1].get('metadata'), chat.results('question'))

    # --- StopFailure categories -----------------------------------------------------------
    limited = Chat(c, 'Rate limited', 'mock', 'limited')
    t = limited.turn('Say hi')
    check('the rate-limited turn fails', limited.finish(t)['status'] == 'failed')
    failures = until(lambda: (v if len(v := entries('StopFailure')) >= 3 else None), 'StopFailure hooks', 15)
    check('rate_limit, provider and billing_error hooks all run, with error kept and error_type added',
          len(failures) == 3 and all(f['error'] == 'provider' and f['error_type'] == 'rate_limit' for f in failures),
          failures)

    # --- Interleaved reasoning ------------------------------------------------------------
    thinker = Chat(c, 'Thinker', 'mock', 'thinker')
    t = thinker.turn('List the folder')
    check('the reasoning turn completes', thinker.finish(t)['status'] == 'completed')
    follow = [r for r in requests if r['model'] == 'thinker' and any(m['role'] == 'tool' for m in r['messages'])]
    assistant = next(m for m in follow[-1]['messages'] if m.get('tool_calls'))
    check('the server gets its reasoning back in the field it used',
          assistant.get('reasoning') == 'List the folder before answering.' and 'reasoning_content' not in assistant, assistant)

    # --- After a tool ran -----------------------------------------------------------------
    feedback = '\n\n<hook-feedback>\nlint: 2 warnings in notes.md\n</hook-feedback>'
    linter = Chat(c, 'Linter', 'mock', 'linter')
    t = linter.turn('Lint the notes')
    check('the linted turn completes', linter.finish(t)['status'] == 'completed')
    linted = linter.results('bash')[-1]
    check('the exit-2 stderr follows the tool result, which stays as it was', not linted['is_error']
          and linted['output'] == 'exit: 0\nlint-me\n' + feedback, linted)
    follow = [r for r in requests if r['model'] == 'linter' and any(m['role'] == 'tool' for m in r['messages'])]
    sent = next(m for m in follow[-1]['messages'] if m['role'] == 'tool')['content']
    sent = sent if isinstance(sent, str) else ''.join(part.get('text', '') for part in sent)
    check('the next request carries the feedback to the model', sent.endswith(feedback), sent)
    posted = [e for e in entries('PostToolUse') if e['session_id'] == linter.thread['id']]
    check('the PostToolUse hook was told its event, the tool and the agent', len(posted) == 1
          and posted[0]['tool_name'] == 'bash' and posted[0]['agent'] == linter.thread['agent'], posted)
    t = chat.turn('!tool bash ' + json.dumps({'command': 'echo lint-me'}))
    check('the echo turn with the lint hook completes', chat.finish(t)['status'] == 'completed')
    until(lambda: 'lint: 2 warnings in notes.md' in capture(), 'TUI shows the hook feedback')
    (root / 'tui-hook-feedback.txt').write_text(capture())
    check('the TUI shows the feedback under the tool output', '<hook-feedback>' in capture(), capture())
    # A person's own command, rejected: the call failed, and PostToolUseFailure hooks say so.
    api(c, '/threads/' + chat.thread['id'] + '/shell', {'command': 'touch refused.txt'})
    pending = until(lambda: api(c, '/permissions'), 'the shell command asks')[0]
    api(c, '/permissions/' + pending['permissionId'], {'decision': 'reject'})
    failed = until(lambda: [e for e in entries('PostToolUseFailure') if e['session_id'] == chat.thread['id']], 'PostToolUseFailure')
    check('a rejected command runs PostToolUseFailure hooks under that name', len(failed) == 1
          and failed[0]['hook_event_name'] == 'PostToolUseFailure' and failed[0]['tool_name'] == 'bash'
          and failed[0]['tool_response']['is_error'] is True and failed[0]['error'], failed)
    refused = until(lambda: [r for r in chat.results('bash') if r['is_error']], 'the refused result')[-1]
    check('its feedback follows the failed result', refused['output'].endswith(
        '\n\n<hook-feedback>\nrefusal noted by PostToolUseFailure\n</hook-feedback>')
          and not (ws / 'refused.txt').exists(), refused)
    # A tool that fails in the model's turn: PostToolUseFailure hooks run, and what they say follows the error.
    reader = Chat(c, 'Reader', 'mock', 'reader')
    t = reader.turn('Read the missing file')
    check('the failed read turn completes', reader.finish(t)['status'] == 'completed')
    failed = [e for e in entries('PostToolUseFailure') if e['session_id'] == reader.thread['id']]
    check('a tool that fails in a turn runs PostToolUseFailure hooks, told the error', len(failed) == 1
          and failed[0]['tool_name'] == 'read' and failed[0]['tool_input'] == {'path': 'missing.txt'}
          and failed[0]['tool_response']['is_error'] is True and failed[0]['error']
          and failed[0]['agent'] == reader.thread['agent'], failed)
    check('no PostToolUse hook runs for the failed read',
          not [e for e in entries('PostToolUse') if e['session_id'] == reader.thread['id']], entries('PostToolUse'))
    feedback = '\n\n<hook-feedback>\nnothing at missing.txt: list the folder first\n</hook-feedback>'
    read = reader.results('read')[-1]
    check('the failure hook\'s stderr follows the error, which stays an error',
          read['is_error'] and read['output'] == failed[0]['error'] + feedback, read)
    follow = [r for r in requests if r['model'] == 'reader' and any(m['role'] == 'tool' for m in r['messages'])]
    sent = next(m for m in follow[-1]['messages'] if m['role'] == 'tool')['content']
    sent = sent if isinstance(sent, str) else ''.join(part.get('text', '') for part in sent)
    check('the next request carries the failure feedback to the model', sent.endswith(feedback), sent)

    # --- Matchers in /hooks ---------------------------------------------------------------
    tmux('send-keys', '-t', session, '/hooks', 'Enter')
    until(lambda: 'never runs' in capture(), 'TUI /hooks marks the hook that never runs')
    (root / 'tui-hooks.txt').write_text(capture())
    check('/hooks marks the SessionEnd hook for clear as never running',
          any('SessionEnd' in line and 'never runs' in line for line in capture().splitlines())
          or 'never runs' in capture(), capture())
    print('PASS', len(checks), 'checks.', flush=True)
finally:
    subprocess.run(['tmux', 'kill-session', '-t', session], capture_output=True)
    for ws in connections:
        try:
            cli(ws, 'serve', '--stop')
        except Exception as error:
            print('stop failed', ws, error, flush=True)
    mock.shutdown()
    e2e_scratch.settle(root)
