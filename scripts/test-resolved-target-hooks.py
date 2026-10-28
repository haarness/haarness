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
- Rewrites: a PermissionRequest hook that allows only a rewritten command does not answer the
  prompt; the person is asked and the command as written never runs on the hook's word.
- Background results: a subagent's result whose chat lost its agent meanwhile is kept in the
  chat as a hidden message with a notice, not dropped.
- Chat folders: on a project's server, a chat created with a folder of its own runs the project's
  hooks in that folder, and they find their script through `$CLAUDE_PROJECT_DIR`, the project. On
  a `--chat-home` server started as Desktop starts it, global PreToolUse and PostToolUse hooks of
  a chat with a folder of its own run in that folder and get it as `$CLAUDE_PROJECT_DIR` and
  `cwd`, with the agent named; a chat given no folder gets one and its hooks run there too.
Every daemon runs on isolated data under one temporary root and is stopped at the end.
"""
import json, os, pathlib, shlex, subprocess, tempfile, time, urllib.error, urllib.request, uuid
import e2e_scratch
from e2e_env import clean_env

REPO = pathlib.Path(__file__).resolve().parents[1]
root = pathlib.Path(tempfile.mkdtemp(prefix='haarness-resolved-targets-')).resolve()
binary = str(pathlib.Path(os.environ.get('HAARNESS_BIN', REPO / 'target/debug/haarness')).resolve())
env = clean_env()
env.update(HAARNESS_HOME=str(root / 'home'), HAARNESS_DATA_DIR=str(root / 'data'), HAARNESS_TEST_PROVIDER='1',
           HAARNESS_SERVER_TOKEN='', HAARNESS_SERVER_PASSWORD='')
CANARY = 'SECRET_CANARY_' + uuid.uuid4().hex[:8]
name = 'haarness-resolved-' + uuid.uuid4().hex[:8]
connections, checks = [], []
home_env = None


def write(path, content):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content)


def workspace(label, config):
    ws = root / label
    # Chats start in Auto; the asks here wait for a person, so every workspace asks for Review.
    write(ws / '.haarness/config.toml', '[defaults]\nprovider="echo"\nmodel="echo-1"\napproval_mode="review"\n')
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
    check('the TUI card names the file notes.txt leads to', 'notes.txt' in capture() and 'Leads to .env' in capture(), capture())
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
    # --- Rewrites ---------------------------------------------------------------------------
    rewrite = root / 'rewrite-hook.py'
    write(rewrite, '''import json, sys
json.load(sys.stdin)
print(json.dumps({'hookSpecificOutput': {'hookEventName': 'PermissionRequest',
                  'decision': {'behavior': 'allow', 'updatedInput': {'command': 'true'}}}}))
''')
    rewrite_ws = workspace('rewrite', {'permission': {'bash': 'ask'}, 'hooks': {'PermissionRequest': [
        {'matcher': 'Bash', 'hooks': [{'type': 'command', 'command': shlex.join(['python3', str(rewrite)])}]}]}})
    rc = connect(rewrite_ws)
    rewrite_chat = Chat(rc, 'Rewriting hook')
    t = rewrite_chat.turn('!tool bash ' + json.dumps({'command': 'touch hook-rewrote-this'}))
    pending = rewrite_chat.pending()
    check('a hook allowing only a rewritten command leaves the prompt to the person',
          pending['permission'] == 'bash' and not (rewrite_ws / 'hook-rewrote-this').exists(), pending)
    rewrite_chat.reject(pending)
    rewrite_chat.finish(t)
    check('the rejected command did not run', not (rewrite_ws / 'hook-rewrote-this').exists())
    # --- Background results -----------------------------------------------------------------
    background_ws = workspace('background', {})
    helper = background_ws / '.haarness/agents/helper.md'
    write(helper, '---\ndescription: Delegates checks\nmode: all\n---\nHELPER_PROMPT\n')
    bc = connect(background_ws)
    until(lambda: any(a.get('id', a.get('name')) == 'helper' for a in api(bc, '/agents')), 'helper agent listed')
    thread = api(bc, '/threads', {'title': 'Background keep', 'provider': 'echo', 'model': 'echo-1', 'agent': 'helper'})
    background_chat = Chat.__new__(Chat)
    background_chat.c, background_chat.thread = bc, thread
    task = {'prompt': '!tool bash {"command":"sleep 3"}', 'agent': 'general', 'description': 'slow check', 'background': True}
    t = background_chat.turn('!tool task ' + json.dumps(task))
    check('the background task launches', background_chat.finish(t)['status'] == 'completed')
    helper.unlink()
    until(lambda: not any(a.get('id', a.get('name')) == 'helper' for a in api(bc, '/agents')), 'helper removal applied', 6)
    items = until(lambda: (v if 'could not start a turn in this chat' in (v := background_chat.transcript()) else None),
                  'the refused result is kept', 30)
    check('a result its chat cannot take is kept with a notice',
          'state=\\"completed\\"' in items and 'Unknown agent: helper' in items, items[-2000:])
    # --- Chat folders -----------------------------------------------------------------------
    # On a project's server, a chat created with a folder of its own runs the project's hooks in
    # that folder, and they still find their script through $CLAUDE_PROJECT_DIR, the project.
    project_log = root / 'project-folder-hooks.log'
    by_project = [{'matcher': 'Bash', 'hooks': [{'type': 'command', 'command': '"$CLAUDE_PROJECT_DIR"/.haarness/hooks/record.sh'}]}]
    project_ws = workspace('folder-project', {'hooks': {'PreToolUse': by_project, 'PostToolUse': by_project}})
    log_path = shlex.quote(str(project_log))
    write(project_ws / '.haarness/hooks/record.sh',
          f'#!/bin/sh\nprintf "%s|%s|" "$(pwd -P)" "$CLAUDE_PROJECT_DIR" >> {log_path}\ncat >> {log_path}\necho >> {log_path}\n')
    (project_ws / '.haarness/hooks/record.sh').chmod(0o755)
    pc = connect(project_ws)
    in_project = Chat.__new__(Chat)
    in_project.c = pc
    in_project.thread = api(pc, '/threads', {'title': 'Folder in a project', 'provider': 'echo', 'model': 'echo-1',
                                             'directory': '2026-10-28/notes'})
    t = in_project.turn('!tool bash ' + json.dumps({'command': 'pwd -P'}))
    check('a project chat with a folder of its own runs its shell there', in_project.finish(t)['status'] == 'completed'
          and in_project.thread['directory'] in in_project.transcript(), in_project.thread)
    project_runs = [(line.split('|', 2)[0], line.split('|', 2)[1], json.loads(line.split('|', 2)[2]))
                    for line in project_log.read_text().splitlines()] if project_log.exists() else []
    check('its project hooks found their script, ran in its folder and were told the project folder',
          [run[2]['hook_event_name'] for run in project_runs] == ['PreToolUse', 'PostToolUse']
          and all(run[0] == run[2]['cwd'] == in_project.thread['directory'] and run[1] == str(project_ws)
                  for run in project_runs), project_runs)
    # Its own data folder: the global hooks here must not reach the workspaces above.
    home_env = dict(env, HAARNESS_DATA_DIR=str(root / 'home-data'), HAARNESS_SERVER_TOKEN='')
    chat_home = root / 'app' / 'Chat folders'
    chat_home.mkdir(parents=True)
    folder_log = root / 'folder-hooks.log'
    record = shlex.join(['sh', '-c', 'printf "%s|%s|" "$(pwd -P)" "$CLAUDE_PROJECT_DIR" >> "$1"; cat >> "$1"; echo >> "$1"',
                         'record', str(folder_log)])
    recorder = [{'matcher': 'Bash', 'hooks': [{'type': 'command', 'command': record}]}]
    write(root / 'home-data/config/config.toml', '[defaults]\nprovider="echo"\nmodel="echo-1"\napproval_mode="review"\n')
    write(root / 'home-data/config/haarness.json', json.dumps({'hooks': {'PreToolUse': recorder, 'PostToolUse': recorder}}))
    hc = json.loads(subprocess.run([binary, '--workspace', str(chat_home), '--chat-home', 'desktop'], input='{"action":"connect"}',
                                   env=home_env, text=True, capture_output=True, check=True, timeout=120).stdout)
    named = api(hc, '/threads', {'title': 'Named folder', 'directory': '2026-10-28/notes'})
    fresh = api(hc, '/threads', {'title': 'No folder named'})
    for thread in (named, fresh):
        folder_chat = Chat.__new__(Chat)
        folder_chat.c, folder_chat.thread = hc, thread
        t = folder_chat.turn('!tool bash ' + json.dumps({'command': 'pwd -P'}))
        check(f'{thread["title"]}: the shell runs in its folder', folder_chat.finish(t)['status'] == 'completed'
              and thread['directory'] in folder_chat.transcript(), thread)
    runs = []
    for line in folder_log.read_text().splitlines():
        ran_in, project_dir, payload = line.split('|', 2)
        runs.append((ran_in, project_dir, json.loads(payload)))
    (root / 'folder-hooks.json').write_text(json.dumps(runs, indent=1))
    for thread in (named, fresh):
        mine = [run for run in runs if run[2]['session_id'] == thread['id']]
        check(f'{thread["title"]}: PreToolUse and PostToolUse ran in the chat folder, not the chat home',
              [run[2]['hook_event_name'] for run in mine] == ['PreToolUse', 'PostToolUse']
              and all(run[0] == run[1] == run[2]['cwd'] == thread['directory'] for run in mine), mine)
        check(f'{thread["title"]}: the payloads name the agent',
              all(run[2]['agent'] == thread['agent'] and run[2]['tool_name'] == 'bash' for run in mine), mine)
    check('the chat given no folder got one in the chat home', fresh['directory'].startswith(str(chat_home.resolve())), fresh)
    print('PASS', len(checks), 'checks.', flush=True)
finally:
    subprocess.run(['tmux', 'kill-session', '-t', name], capture_output=True)
    for ws in connections:
        try:
            cli(ws, 'serve', '--stop')
        except Exception as error:
            print('stop failed', ws, error, flush=True)
    if home_env is not None:
        subprocess.run([binary, '--workspace', str(root / 'app' / 'Chat folders'), 'serve', '--stop'], env=home_env,
                       capture_output=True, timeout=35)
    e2e_scratch.settle(root)
