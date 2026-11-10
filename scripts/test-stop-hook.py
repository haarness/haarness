#!/usr/bin/env python3
"""Real daemon and TUI coverage for Stop hooks that keep a turn going, with the Echo provider.

- A Stop hook that exits 2 with stderr keeps the turn going: the model answers its feedback in the
  same turn, which then ends once. The hook hears `stop_hook_active` false, then true; `TurnEnd`
  runs once, at the real end. The hook's message is hidden from the person as a message (it is
  marked `hook: "Stop"`) and the attached TUI shows a quiet note with the feedback instead.
- `{"decision": "block", "reason": …}` does the same with its reason.
- A hook that always blocks keeps the turn going at most 8 times in a row, then the turn ends and
  the daemon log says why.
- `continue: false`, an exit 2 with nothing on stderr and a block with no reason all let it end.
- A Stop pressed while the hook runs ends the turn, and no message of the hook's is added.
- `haarness run` writes one stderr line when a Stop hook sends the agent back; its JSON output is unchanged.
- A message the person sends while the hook runs goes first: no feedback is added for that end.
Every daemon runs on isolated data under one temporary root and is stopped at the end.
"""
import json, os, pathlib, shlex, subprocess, sys, tempfile, time, urllib.error, urllib.request, uuid
import e2e_scratch
from e2e_tmux import pane_command
from e2e_env import clean_env

REPO = pathlib.Path(__file__).resolve().parents[1]
root = pathlib.Path(tempfile.mkdtemp(prefix='haarness-stop-hook-')).resolve()
binary = str(pathlib.Path(os.environ.get('HAARNESS_BIN', REPO / 'target/debug/haarness')).resolve())
env = clean_env()
env.update(HAARNESS_HOME=str(root / 'home'), HAARNESS_DATA_DIR=str(root / 'data'), HAARNESS_TEST_PROVIDER='1',
           HAARNESS_SERVER_TOKEN='', HAARNESS_SERVER_PASSWORD='')
session = 'haarness-stop-hook-' + uuid.uuid4().hex[:8]
connections, checks = [], []


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


def log_text():
    return ''.join(path.read_text(errors='replace') for path in sorted((root / 'data').glob('**/logs/haarness.log*')))


class Chat:
    def __init__(self, c, title):
        self.c = c
        self.thread = api(c, '/threads', {'title': title, 'provider': 'echo', 'model': 'echo-1'})
        self.id = self.thread['id']

    def turn(self, text):
        return api(self.c, '/threads/' + self.id + '/turns', {'requestId': str(uuid.uuid4()), 'text': text})

    def finish(self, turn):
        return until(lambda: (v if (v := api(self.c, '/turns/' + turn['id']))['status'] not in ['running', 'queued'] else None),
                     'turn completion', 60)

    def items(self):
        return [i['data'] for i in api(self.c, '/threads/' + self.id + '/items')]

    def replies(self):
        return [i['text'] for i in self.items() if i['type'] == 'assistant_message']

    def notes(self):
        return [i for i in self.items() if i['type'] == 'user_message' and i.get('hook')]


try:
    log = root / 'hook-log.jsonl'
    mode = root / 'mode.txt'
    hook = root / 'hook.py'
    # Records every payload, then answers the Stop event as mode.txt says.
    write(hook, '''import json, os, pathlib, sys, time
data = json.load(sys.stdin)
log = pathlib.Path(sys.argv[1])
with log.open('a') as out:
    out.write(json.dumps(data) + '\\n')
if data['hook_event_name'] != 'Stop':
    sys.exit(0)
mode = pathlib.Path(sys.argv[2]).read_text().strip()
active = data['stop_hook_active']
if mode == 'exit2' and not active:
    print('run the tests before you finish', file=sys.stderr)
    sys.exit(2)
if mode == 'json' and not active:
    print(json.dumps({'decision': 'block', 'reason': 'check the formatting too'}))
if mode == 'always':
    print(json.dumps({'decision': 'block', 'reason': 'keep going'}))
if mode == 'halt':
    print(json.dumps({'decision': 'block', 'reason': 'never read', 'continue': False, 'stopReason': 'for the person'}))
if mode == 'quiet-exit2':
    sys.exit(2)
if mode == 'no-reason':
    print(json.dumps({'decision': 'block'}))
if mode == 'slow' and not active:
    pathlib.Path(sys.argv[2] + '.waiting').write_text('waiting')
    while not pathlib.Path(sys.argv[2] + '.release').exists():
        time.sleep(0.05)
    print('slow feedback', file=sys.stderr)
    sys.exit(2)
''')
    command = shlex.join(['python3', str(hook), str(log), str(mode)])
    group = [{'hooks': [{'type': 'command', 'command': command}]}]
    ws = root / 'project'
    write(ws / '.haarness/config.toml', '''[defaults]
provider="echo"
model="echo-1"
''')
    write(ws / 'haarness.json', json.dumps({'hooks': {'Stop': group, 'TurnEnd': group}}, indent=1))
    c = connect(ws)
    env['HAARNESS_SERVER_TOKEN'] = c.get('token', '')
    entries = lambda event, chat: [e for line in log.read_text().splitlines() if line
                                   for e in [json.loads(line)] if e['hook_event_name'] == event
                                   and e['session_id'] == chat.id] if log.exists() else []

    # --- exit 2 with stderr, and the TUI's note ---------------------------------------------
    mode.write_text('exit2')
    chat = Chat(c, 'Stop hook exit 2')
    attach = [binary, '--workspace', str(root), 'attach', c['url'], '--thread', chat.id]
    tmux('new-session', '-d', '-s', session, '-x', '120', '-y', '40', pane_command(env, attach))
    until(lambda: 'Ask anything' in capture() or 'Stop hook exit 2' in capture(), 'TUI attach')
    t = chat.turn('fix the bug')
    check('the turn completes once the hook is answered', chat.finish(t)['status'] == 'completed')
    replies = chat.replies()
    check('the model answers the prompt, then the hook\'s feedback, in the same turn',
          len(replies) == 2 and 'fix the bug' in replies[0] and 'run the tests before you finish' in replies[1], replies)
    stops = entries('Stop', chat)
    check('the Stop hook hears stop_hook_active false, then true',
          [e['stop_hook_active'] for e in stops] == [False, True], stops)
    check('TurnEnd runs once, at the real end', len(entries('TurnEnd', chat)) == 1, entries('TurnEnd', chat))
    notes = chat.notes()
    check('one hidden message carries the feedback, marked as the Stop hook\'s',
          len(notes) == 1 and notes[0]['hook'] == 'Stop' and notes[0]['synthetic'] is True
          and '<hook-feedback>\nrun the tests before you finish\n</hook-feedback>' in notes[0]['text'], notes)
    until(lambda: 'Stop hook' in capture() and 'run the tests before you finish' in capture(), 'TUI shows the note')
    (root / 'tui-stop-hook-note.txt').write_text(capture())
    (root / 'tui-stop-hook-note.ansi').write_text(tmux('capture-pane', '-p', '-e', '-t', session + ':0.0'))
    note = next(line for line in capture().splitlines() if '◇ Stop hook' in line)
    # The echo model repeats the hook's message in its reply; no prompt row shows it.
    prompts = [line for line in capture().splitlines() if line.lstrip().startswith('▎')]
    check('the TUI shows one quiet line for the hook, not the message the model read',
          'run the tests before you finish' in note and note.lstrip().startswith('◇ Stop hook · ')
          and not any('Stop hook output follows' in line for line in prompts), capture())
    tmux('send-keys', '-t', session, '/language ar', 'Enter')
    until(lambda: 'خطاف Stop' in capture(), 'the TUI note in Arabic')
    (root / 'tui-stop-hook-note-ar.txt').write_text(capture())
    check('the note names the hook in the interface language', 'run the tests before you finish' in capture(), capture())
    tmux('send-keys', '-t', session, '/language en', 'Enter')
    until(lambda: 'Stop hook' in capture(), 'the TUI back in English')

    # --- a JSON block ---------------------------------------------------------------------
    mode.write_text('json')
    chat = Chat(c, 'Stop hook JSON')
    check('a JSON block keeps the turn going too', chat.finish(chat.turn('tidy up'))['status'] == 'completed'
          and len(chat.replies()) == 2 and 'check the formatting too' in chat.replies()[1], chat.replies())

    # --- haarness run ------------------------------------------------------------------------
    mode.write_text('exit2')
    run = lambda *args: subprocess.run([binary, '--workspace', str(ws), '--trust-project', 'run', *args], env=env,
                                       text=True, capture_output=True, timeout=120)
    ran = run('fix the build')
    check('haarness run says on stderr, once, that the Stop hook sent the agent back',
          ran.returncode == 0 and ran.stderr.count('Stop hook: run the tests before you finish') == 1
          and ran.stdout.count('Echo: ') == 2, (ran.stdout, ran.stderr))
    ran = run('--format', 'json', 'fix the build again')
    check('its JSON output is unchanged: the two replies, no hook line',
          ran.returncode == 0 and 'Stop hook:' not in ran.stderr
          and [json.loads(line)['type'] for line in ran.stdout.splitlines() if line.startswith('{')] == ['assistant', 'assistant'],
          (ran.stdout, ran.stderr))

    # --- a hook that always blocks ----------------------------------------------------------
    mode.write_text('always')
    chat = Chat(c, 'Stop hook always')
    check('a hook that always blocks still lets the turn end', chat.finish(chat.turn('loop'))['status'] == 'completed')
    check('it keeps the turn going 8 times in a row, no more',
          len(chat.replies()) == 9 and len(chat.notes()) == 8 and len(entries('Stop', chat)) == 9
          and len(entries('TurnEnd', chat)) == 1, (len(chat.replies()), len(chat.notes())))
    until(lambda: 'Stop hooks kept the turn going 8 times in a row' in log_text(), 'the daemon log says why it ended')
    check('the daemon log names the chat the cap ended',
          any(chat.id in line for line in log_text().splitlines() if '8 times in a row' in line), None)

    # --- answers that let the turn end ---------------------------------------------------------
    for quiet in ['halt', 'quiet-exit2', 'no-reason']:
        mode.write_text(quiet)
        chat = Chat(c, 'Stop hook ' + quiet)
        done = chat.finish(chat.turn('done?'))
        check(f'{quiet}: the turn ends after one reply', done['status'] == 'completed'
              and len(chat.replies()) == 1 and not chat.notes() and len(entries('Stop', chat)) == 1, chat.items())

    # --- a Stop pressed while the hook runs ----------------------------------------------------
    mode.write_text('slow')
    chat = Chat(c, 'Stop hook stopped')
    t = chat.turn('stop me')
    until(lambda: pathlib.Path(str(mode) + '.waiting').exists(), 'the slow hook runs')
    api(c, '/turns/' + t['id'] + '/interrupt', {})
    pathlib.Path(str(mode) + '.release').write_text('go')
    stopped = chat.finish(t)
    time.sleep(1)
    check('a Stop while the hook runs ends the turn, with no message of the hook\'s',
          stopped['status'] == 'interrupted' and not chat.notes() and len(chat.replies()) == 1, chat.items())
    pathlib.Path(str(mode) + '.waiting').unlink()
    pathlib.Path(str(mode) + '.release').unlink()

    # --- a person's message while the hook runs goes first -------------------------------------
    chat = Chat(c, 'Stop hook steered')
    t = chat.turn('first')
    until(lambda: pathlib.Path(str(mode) + '.waiting').exists(), 'the slow hook runs again')
    mode.write_text('none')
    sent = api(c, '/threads/' + chat.id + '/input', {'requestId': str(uuid.uuid4()), 'text': 'answer me instead'})
    check('the message steers the running turn', sent.get('queued') is True, sent)
    pathlib.Path(str(mode) + '.release').write_text('go')
    done = chat.finish(t)
    replies = chat.replies()
    check('the person\'s message goes first, and the hook\'s feedback is not added',
          done['status'] == 'completed' and not chat.notes() and len(replies) == 2
          and 'answer me instead' in replies[1], chat.items())
    check('the hook ran again at the new end, told no hook kept the turn going',
          [e['stop_hook_active'] for e in entries('Stop', chat)] == [False, False], entries('Stop', chat))
    print('PASS', len(checks), 'checks.', flush=True)
finally:
    try:
        (root / 'tui-last.txt').write_text(capture())
    except Exception:
        pass
    subprocess.run(['tmux', 'kill-session', '-t', session], capture_output=True)
    for ws in connections:
        try:
            cli(ws, 'serve', '--stop')
        except Exception as error:
            print('stop failed', ws, error, flush=True)
    e2e_scratch.settle(root)
