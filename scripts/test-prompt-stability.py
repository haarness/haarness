#!/usr/bin/env python3
"""Real daemon + TUI proof that safety stops are explained and that the prompt prefix stays stable.

A mock Anthropic Messages server records every request body. Against it, a debug `haarness`
daemon in a private data folder shows that:

1. a `refusal` stop with no text fails the turn (not a blank reply), is not retried, reaches
   `StopFailure` as `content_filter`, is logged with the provider's request id, and the TUI
   shows the new message; a refusal after some text keeps that text; a side question that
   meets a refusal answers HTTP 502 with code `provider_blocked`;
2. with a `UserPromptSubmit` hook that adds a timestamp, two turns send a byte-identical
   `system` and the second request starts with the first one's messages; each stamp is in
   the messages, not the system prompt;
3. a chat that began days ago (its `created_at` edited in the store) is told today's date
   once, and the next turn replays that reminder instead of adding another;
4. a compaction summary request carries no `cache_control` (nothing reads its prefix back, so
   it must not pay a cache write), while the chat's own requests keep their breakpoints;
5. a `general` subagent started from a plan chat and one started from a build chat send a
   byte-identical tool list and system prompt, so the second reads the cached prefix the first
   wrote: the plan chat's browser and write ceiling refuses those tools when called, it does not
   take them out of the list.

With --system-folder the daemon also starts with a system configuration folder
(HAARNESS_SYSTEM_CONFIG_DIR) whose prompt files add to the main instructions and the
summary prompt: every check above must hold with them, the additions must be on the wire,
and the system folder must be left exactly as it was.

Usage: python3 scripts/test-prompt-stability.py [--system-folder]   (HAARNESS_BIN picks the binary,
HAARNESS_E2E_ROOT a folder to keep the fixture in, HAARNESS_E2E_EVIDENCE the evidence folder).
"""
import datetime
import http.server
import json
import os
from pathlib import Path
import shlex
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request
import uuid
from e2e_env import ISOLATED

REPO = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get('HAARNESS_BIN', REPO / 'target/debug/haarness')).resolve()
EVIDENCE = Path(os.environ.get('HAARNESS_E2E_EVIDENCE', REPO / 'target/prompt-stability/e2e'))
EVIDENCE.mkdir(parents=True, exist_ok=True)
MODEL = 'claude-sonnet-4-6'
REQUESTS = []
SYSTEM_FOLDER = '--system-folder' in sys.argv[1:]
if SYSTEM_FOLDER:
    EVIDENCE = EVIDENCE / 'system-folder'
    EVIDENCE.mkdir(parents=True, exist_ok=True)


def run(args, **kwargs):
    return subprocess.run(args, check=True, capture_output=True, text=True, **kwargs).stdout


def until(check, label, timeout=30):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(.05)
    raise AssertionError(label)


def clean(value):
    """Drop `cache_control`: its breakpoints move to the newest messages by design."""
    if isinstance(value, list):
        return [clean(item) for item in value]
    if isinstance(value, dict):
        return {key: clean(item) for key, item in value.items() if key != 'cache_control'}
    return value


def texts(body):
    return [block.get('text', '') for message in body['messages'] for block in message['content']
            if isinstance(message['content'], list)]


class Anthropic(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.end_headers()
        self.wfile.write(json.dumps({'data': [{'id': MODEL, 'display_name': 'Fixture Sonnet'}]}).encode())

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        REQUESTS.append(body)
        prompt = ' '.join(texts(body)[-4:])
        last = body['messages'][-1]['content']
        answered = isinstance(last, list) and any(block.get('type') == 'tool_result' for block in last)
        dispatch = next((word for word in ('DISPATCH_PLAN', 'DISPATCH_BUILD') if word in prompt), None)
        if dispatch and not answered:
            return self.task(body, 'CHILD_FROM_' + dispatch.split('_')[1] + ' list the project files')
        refuse = 'REFUSE' in prompt
        partial = 'REFUSE_PARTIAL' in prompt
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.send_header('request-id', f'req_e2e_{len(REQUESTS)}')
        self.end_headers()
        events = [{'type': 'message_start', 'message': {'id': f'msg_e2e_{len(REQUESTS)}', 'usage': {'input_tokens': 40, 'output_tokens': 0}}}]
        text = 'PARTIAL_BEFORE_REFUSAL' if partial else None if refuse else 'STABLE_ANSWER'
        if text:
            events += [{'type': 'content_block_start', 'index': 0, 'content_block': {'type': 'text', 'text': ''}},
                       {'type': 'content_block_delta', 'index': 0, 'delta': {'type': 'text_delta', 'text': text}},
                       {'type': 'content_block_stop', 'index': 0}]
        events += [{'type': 'message_delta', 'delta': {'stop_reason': 'refusal' if refuse else 'end_turn', 'stop_sequence': None},
                    'usage': {'output_tokens': 3}},
                   {'type': 'message_stop'}]
        self.stream(events)

    def stream(self, events):
        for event in events:
            self.wfile.write(('event: ' + event['type'] + '\ndata: ' + json.dumps(event) + '\n\n').encode())
        self.wfile.flush()

    def task(self, body, prompt):
        """Delegate `prompt` to a `general` subagent."""
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.end_headers()
        call = {'prompt': prompt, 'agent': 'general', 'description': 'list files'}
        self.stream([{'type': 'message_start', 'message': {'id': f'msg_e2e_{len(REQUESTS)}', 'usage': {'input_tokens': 40, 'output_tokens': 0}}},
                     {'type': 'content_block_start', 'index': 0,
                      'content_block': {'type': 'tool_use', 'id': f'toolu_{len(REQUESTS)}', 'name': 'task', 'input': {}}},
                     {'type': 'content_block_delta', 'index': 0, 'delta': {'type': 'input_json_delta', 'partial_json': json.dumps(call)}},
                     {'type': 'content_block_stop', 'index': 0},
                     {'type': 'message_delta', 'delta': {'stop_reason': 'tool_use', 'stop_sequence': None}, 'usage': {'output_tokens': 3}},
                     {'type': 'message_stop'}])


service = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Anthropic)
threading.Thread(target=service.serve_forever, daemon=True).start()
checks = []
keep = os.environ.get('HAARNESS_E2E_ROOT')
holder = None if keep else tempfile.TemporaryDirectory(prefix='haarness-prompt-stability-')
root = Path(keep or holder.name).resolve()
root.mkdir(parents=True, exist_ok=True)
home, workspace, data = root / 'home', root / 'workspace', root / 'data'
session = 'haarness-stability-' + str(uuid.uuid4())[:8]
env = {**ISOLATED, 'PATH': '/usr/bin:/bin:/usr/sbin:/sbin', 'HOME': str(home), 'HAARNESS_HOME': str(home),
       'HAARNESS_DATA_DIR': str(data), 'USER': os.environ.get('USER', 'fixture'), 'LANG': 'en_US.UTF-8',
       'ANTHROPIC_API_KEY': 'fixture-key', 'ANTHROPIC_BASE_URL': f'http://127.0.0.1:{service.server_port}/v1'}
args = [str(BINARY), '--workspace', str(workspace), '--trust-project']
connection = None
team = root / 'team'
if SYSTEM_FOLDER:
    (team / 'prompts').mkdir(parents=True, exist_ok=True)
    (team / 'prompts/system.md').write_text('BASE_PROMPT_MARKER: follow the team checklist.\n')
    (team / 'prompts/compaction.md').write_text('---\nmode: prepend\n---\nBASE_SUMMARY_MARKER\n')
    env['HAARNESS_SYSTEM_CONFIG_DIR'] = str(team)
    base_before = {path: path.read_bytes() for path in team.rglob('*') if path.is_file()}


def system_text(body):
    return ''.join(block['text'] for block in body['system']) if isinstance(body['system'], list) else body['system']


try:
    (home / '.local/bin').mkdir(parents=True, exist_ok=True)
    (workspace / '.haarness').mkdir(parents=True, exist_ok=True)
    (workspace / '.haarness/config.toml').write_text(
        # Review, as the TUI's status line it waits for shows; chats start
        # in Auto otherwise.
        f'[defaults]\nprovider="anthropic"\nmodel="{MODEL}"\napproval_mode="review"\n[permissions]\n"*"="deny"\n[lsp]\nenabled=false\n')
    stamp = f"{sys.executable} -c \"import json,time; print(json.dumps({{'additionalContext': 'HOOK_STAMP %d' % time.time_ns()}}))\""
    (workspace / 'haarness.json').write_text(json.dumps({'hooks': {
        'UserPromptSubmit': [{'hooks': [{'type': 'command', 'command': stamp}]}],
        'StopFailure': [{'matcher': 'content_filter', 'hooks': [{'type': 'command', 'command': f"cat >> '{root / 'stop-failure.jsonl'}'; echo >> '{root / 'stop-failure.jsonl'}'"}]}],
    }}))
    run(['git', 'init', '-q', str(workspace)])
    connection = json.loads(run(args + ['desktop'], input='{"action":"connect"}', env=env, timeout=120))

    def api(path, body=None, method=None):
        request = urllib.request.Request(connection['url'] + '/v1' + path,
                                         data=None if body is None else json.dumps(body).encode(), method=method,
                                         headers={'Authorization': 'Bearer ' + connection['token'], 'Content-Type': 'application/json'})
        with urllib.request.urlopen(request, timeout=30) as response:
            raw = response.read()
            return json.loads(raw) if raw else None

    def chat(title):
        return api('/threads', {'title': title, 'provider': 'anthropic', 'model': MODEL})['id']

    def finished(identifier):
        value = api(f'/turns/{identifier}')
        return value if value['status'] not in ('queued', 'running') else None

    def turn(thread, text):
        identifier = api(f'/threads/{thread}/turns', {'requestId': str(uuid.uuid4()), 'text': text})['id']
        return until(lambda: finished(identifier), f'turn {text!r} did not finish')

    def items(thread):
        return api(f'/threads/{thread}/items')

    # 1. Safety stops.
    refused = chat('Refusal fixture')
    command = shlex.join(['env', *[f'{key}={value}' for key, value in env.items()], *args, '--thread', refused])
    run(['tmux', 'new-session', '-d', '-x', '150', '-y', '42', '-s', session, command])

    def pane():
        return run(['tmux', 'capture-pane', '-p', '-J', '-t', session])
    until(lambda: 'review' in pane().lower(), 'TUI failed to start', timeout=60)
    offset = len(REQUESTS)
    run(['tmux', 'send-keys', '-l', '-t', session, 'REFUSE_EMPTY please answer'])
    run(['tmux', 'send-keys', '-t', session, 'Enter'])
    until(lambda: 'safety check stopped' in pane(), 'TUI did not explain the refusal')
    until(lambda: not api('/turns/active'), 'refused turn stayed active')
    (EVIDENCE / 'refusal-tui.txt').write_text(pane())
    assert len(REQUESTS) - offset == 1, f'a refusal was retried: {len(REQUESTS) - offset} requests'
    run(['tmux', 'kill-session', '-t', session])
    transcript = items(refused)
    errors = [item for item in transcript if item['data']['type'] == 'error']
    assert len(errors) == 1, errors
    assert finished(errors[0]['turnId'])['status'] == 'failed', 'the refused turn did not fail'
    errors = [item['data'] for item in errors]
    assert not errors[0]['retryable'], errors
    assert errors[0]['message'].startswith("The provider's safety check stopped this response (refusal)."), errors
    assert 'rephrase the request or choose another model' in errors[0]['message'], errors
    assert not any(item['data']['type'] == 'assistant_message' for item in transcript), 'a refusal became a blank reply'
    stop = until(lambda: (root / 'stop-failure.jsonl').exists() and (root / 'stop-failure.jsonl').read_text().strip(), 'StopFailure hook did not run')
    stop = json.loads(stop.splitlines()[0])
    assert stop['error'] == 'content_filter', stop
    logs = until(lambda: [path for path in root.rglob('*.log*') if 'provider safety check stopped the response' in path.read_text(errors='ignore')],
                 'no log line for the refusal')
    line = next(line for line in logs[0].read_text(errors='ignore').splitlines() if 'provider safety check stopped the response' in line)
    assert 'refusal' in line and f'req_e2e_{offset + 1}' in line and f'msg_e2e_{offset + 1}' in line, line
    (EVIDENCE / 'refusal-log-line.txt').write_text(line + '\n')

    partial = chat('Partial refusal fixture')
    assert turn(partial, 'REFUSE_PARTIAL please answer')['status'] == 'failed'

    transcript = items(partial)
    assert any(item['data'].get('text') == 'PARTIAL_BEFORE_REFUSAL' for item in transcript if item['data']['type'] == 'assistant_message'), transcript
    assert any(item['data']['type'] == 'error' and 'safety check stopped' in item['data']['message'] for item in transcript), transcript
    try:
        api(f'/threads/{partial}/btw', {'question': 'REFUSE_EMPTY side question'})
        raise AssertionError('a refused side question succeeded')
    except urllib.error.HTTPError as error:
        failure = json.loads(error.read())
        assert error.code == 502 and failure['code'] == 'provider_blocked' and not failure['retryable'], failure
    checks.append({'refusal': ['TUI message', 'one request', 'error item not retryable', 'StopFailure content_filter',
                               'request id logged', 'partial text kept', 'btw 502 provider_blocked']})
    print('PASS refusal stops', flush=True)

    # 2. Hook context stays out of the system prompt.
    stable = chat('Stable prefix fixture')
    offset = len(REQUESTS)
    assert turn(stable, 'FIRST stable turn')['status'] == 'completed'
    assert turn(stable, 'SECOND stable turn')['status'] == 'completed'
    first, second = REQUESTS[offset:offset + 2]
    assert len(REQUESTS) == offset + 2, 'extra requests in the stable chat'
    assert json.dumps(first['system']) == json.dumps(second['system']), 'system prompt changed between turns'
    assert first['tools'] == second['tools'], 'tool schemas changed between turns'
    assert clean(second['messages'][:len(first['messages'])]) == clean(first['messages']), 'an earlier message changed'
    stamps = [text for text in texts(second) if 'HOOK_STAMP' in text]
    assert len(stamps) == 2 and stamps[0] != stamps[1], stamps
    assert all(text.startswith('<system-reminder>\nContext from hooks:\nHOOK_STAMP ') for text in stamps), stamps
    assert 'HOOK_STAMP' not in json.dumps(first['system']), 'hook context reached the system prompt'
    if SYSTEM_FOLDER:
        added = 'not an OS sandbox.\n\nBASE_PROMPT_MARKER: follow the team checklist.\n\nYou are powered by'
        assert added in system_text(first), system_text(first)[:600]
    else:
        assert 'BASE_PROMPT_MARKER' not in json.dumps(first), 'a base prompt file applied without a system folder'
    prompts = texts(second)
    assert prompts.index(stamps[0]) == prompts.index('FIRST stable turn') + 1, prompts
    shown = [item for item in items(stable) if item['data']['type'] == 'user_message' and not item['data'].get('synthetic')]
    assert [item['data']['text'] for item in shown] == ['FIRST stable turn', 'SECOND stable turn'], shown
    # The TUI shows the two prompts and answers, not the stored reminders.
    command = shlex.join(['env', *[f'{key}={value}' for key, value in env.items()], *args, '--thread', stable])
    run(['tmux', 'new-session', '-d', '-x', '150', '-y', '42', '-s', session, command])
    until(lambda: 'SECOND stable turn' in pane() and pane().count('STABLE_ANSWER') == 2, 'TUI did not show the stable chat', timeout=60)
    shown = pane()
    (EVIDENCE / 'stable-tui.txt').write_text(shown)
    assert 'HOOK_STAMP' not in shown and 'system-reminder' not in shown, shown
    run(['tmux', 'kill-session', '-t', session])
    (EVIDENCE / 'stable-requests.json').write_text(json.dumps([first, second], indent=2))
    checks.append({'stable prefix': ['system byte-identical', 'tools identical', 'messages prefix identical',
                                     'hook stamp after its prompt', 'reminders hidden from the API transcript and the TUI']})
    print('PASS stable system prompt with per-turn hook context', flush=True)

    # 3. A compaction summary writes no cache entry.
    offset = len(REQUESTS)
    api(f'/threads/{stable}/compact', {})
    summaries = REQUESTS[offset:]
    assert len(summaries) == 1, f'{len(summaries)} compaction requests'
    assert 'Summarize this conversation for continuation' in json.dumps(summaries[0]['messages'][-1]), summaries[0]['messages'][-1]
    assert 'cache_control' not in json.dumps(summaries[0]), 'the compaction summary marked cache breakpoints'
    if SYSTEM_FOLDER:
        assert system_text(summaries[0]).startswith('BASE_SUMMARY_MARKER\n\nYou are Haarness'), summaries[0]['system']
    assert 'cache_control' in json.dumps(first['system']) and 'cache_control' in json.dumps(second['messages']), 'a turn lost its breakpoints'
    offset = len(REQUESTS)
    assert turn(stable, 'THIRD stable turn')['status'] == 'completed'
    assert 'cache_control' in json.dumps(REQUESTS[offset]), 'the turn after a compaction lost its breakpoints'
    (EVIDENCE / 'compaction-request.json').write_text(json.dumps(summaries[0], indent=2))
    checks.append({'compaction': ['summary request has no cache_control', 'turns before and after keep theirs']})
    print('PASS compaction summary writes no cache entry', flush=True)

    # 4. A chat begun days ago is told today's date once.
    old = chat('Old chat fixture')
    began = (datetime.datetime.now(datetime.timezone.utc) - datetime.timedelta(days=5)).replace(microsecond=0)
    database = next(path for path in data.rglob('haarness.db')
                    if sqlite3.connect(path).execute('SELECT 1 FROM threads WHERE id=?', (old,)).fetchone())
    with sqlite3.connect(database) as store:
        store.execute('UPDATE threads SET created_at=? WHERE id=?', (began.isoformat().replace('+00:00', 'Z'), old))
    offset = len(REQUESTS)
    turn(old, 'FIRST old turn')
    turn(old, 'SECOND old turn')
    first, second = REQUESTS[offset:offset + 2]
    today = datetime.date.today().isoformat()
    began_local = began.astimezone().date().isoformat()
    system = json.dumps(first['system'])
    assert f"Today's date: {began_local}" in system, system[:2000]
    assert json.dumps(second['system']) == system, 'system prompt changed'
    told = f"Today's date is now {today}."
    for body in (first, second):
        assert sum(text.count(told) for text in texts(body)) == 1, texts(body)
    assert clean(second['messages'][:len(first['messages'])]) == clean(first['messages']), 'an earlier message changed'
    (EVIDENCE / 'date-requests.json').write_text(json.dumps([first, second], indent=2))
    checks.append({'date reminder': [f'system says {began_local}', f'told {today} once', 'replayed unchanged']})
    print('PASS date reminder for an old chat', flush=True)

    # 5. Subagents of a plan chat and of a build chat share one cached prefix. This workspace
    # denies every tool, so it runs in a second one with the default rules.
    agents_workspace = root / 'agents-workspace'
    (agents_workspace / '.haarness').mkdir(parents=True, exist_ok=True)
    (agents_workspace / '.haarness/config.toml').write_text(
        f'[defaults]\nprovider="anthropic"\nmodel="{MODEL}"\n[lsp]\nenabled=false\n')
    run(['git', 'init', '-q', str(agents_workspace)])
    agents_args = [str(BINARY), '--workspace', str(agents_workspace), '--trust-project']
    second_connection = json.loads(run(agents_args + ['desktop'], input='{"action":"connect"}', env=env, timeout=120))
    try:
        first_connection, connection = connection, second_connection
        children = {}
        for agent in ('plan', 'build'):
            parent = api('/threads', {'title': f'{agent} parent', 'provider': 'anthropic', 'model': MODEL, 'agent': agent})['id']
            assert turn(parent, f'DISPATCH_{agent.upper()} please')['status'] == 'completed'
            marker = f'CHILD_FROM_{agent.upper()}'
            children[agent] = next(body for body in REQUESTS if any(text.startswith(marker) for text in texts(body)))
    finally:
        connection = first_connection
        subprocess.run(agents_args + ['serve', '--stop'], env=env, capture_output=True, timeout=60)
    under_plan, under_build = children['plan'], children['build']
    (EVIDENCE / 'subagent-requests.json').write_text(json.dumps(children, indent=2))
    plan_tools = [tool['name'] for tool in under_plan['tools']]
    build_tools = [tool['name'] for tool in under_build['tools']]
    missing = sorted(set(build_tools) - set(plan_tools))
    assert not missing, f'the plan chat\'s subagent lacks {missing}'
    assert clean(under_plan['tools']) == clean(under_build['tools']), 'the subagents sent different tool lists'
    assert json.dumps(clean(under_plan['system'])) == json.dumps(clean(under_build['system'])), 'the subagents sent different system prompts'
    assert 'browser_navigate' in plan_tools and 'write' in plan_tools, plan_tools
    if SYSTEM_FOLDER:
        assert 'BASE_PROMPT_MARKER' in system_text(under_plan), 'the subagent lost the base prompt file'
        after = {path: path.read_bytes() for path in team.rglob('*') if path.is_file()}
        assert after == base_before, 'the system folder was written'
        checks.append({'system folder': ['system.md appended to the main instructions on the wire',
                                       'compaction.md prepended to the summary prompt', 'system folder unchanged']})
        print('PASS system folder prompt files on the wire, system folder unchanged', flush=True)
    checks.append({'subagent prefix': [f'{len(plan_tools)} tools identical under a plan and a build chat',
                                       'system identical', 'browser_navigate and write listed under the plan chat']})
    print(f'PASS subagents of a plan chat and a build chat send one prefix ({len(plan_tools)} tools)', flush=True)
    (EVIDENCE / 'results.json').write_text(json.dumps(checks, indent=2))
finally:
    subprocess.run(['tmux', 'kill-session', '-t', session], capture_output=True)
    if connection:
        subprocess.run(args + ['serve', '--stop'], env=env, capture_output=True, timeout=60)
    service.shutdown()
    if holder:
        holder.cleanup()
