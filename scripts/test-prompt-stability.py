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
   wrote: the plan chat's edit and write ceiling refuses those tools when called, it does not
   take them out of the list (no browser is attached, so neither is sent the browser tools);
6. a tool the model loaded with `load_tool` stays in the chat's tool list after the daemon
   restarts and in a fork of the chat: the next request's tools are byte-identical to the last
   request's before, so the cached prefix holds;
7. a chat whose model loaded every deferred tool (what is left would no longer pay for the
   loader, so Auto decided afresh would send every schema) keeps its tool list, the loader
   included, after a restart;
8. with skills and an MCP server whose tools wait behind `load_tool`, the `skill` tool goes with
   the first request and names every skill the model may load (a long list cut to its budget,
   person-only skills left out), `load_tool` asked for it says it is already there and
   loads `skill_manage`, a skill name that is no skill is answered with the names, and the
   chat's requests send byte-identical tools before that load and after it;
9. an agent whose file lists `skills` sends their text after its own prompt, with the skill's
   folder and the chat's id filled in and its command named rather than run, leaves out a
   manual-only and a missing skill, and sends a byte-identical system prompt on the next turn;
10. with an MCP server of 25 tools, `load_tool` asked for the server's wildcard and one native
   tool's exact id loads that tool first and 19 of the server's, and its answer names the
   matches it left out.

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
# A schema lazy loading defers by default: `load_tool` loads it in section 6.
LOADED = 'schedule'
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


def last_typed(body):
    """The newest text a person typed into the request, reminders left out."""
    for message in reversed(body['messages']):
        if message['role'] != 'user' or not isinstance(message['content'], list):
            continue
        typed = [block.get('text', '') for block in message['content']
                 if block.get('type') == 'text' and not block.get('text', '').startswith('<system-reminder>')]
        if typed:
            return typed[-1]
    return ''


def load_answer(body):
    """The newest `load_tool` result, read as JSON."""
    for block in reversed(body['messages'][-1]['content']):
        if block.get('type') == 'tool_result':
            content = block.get('content')
            text = content if isinstance(content, str) else ''.join(part.get('text', '') for part in content or [])
            try:
                return json.loads(text)
            except ValueError:
                return {}
    return {}


def loaded_last(body):
    """The ids the newest `load_tool` result loaded."""
    return load_answer(body).get('loaded', [])


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
        if 'LOAD_A_TOOL' in prompt and not answered:
            return self.call('load_tool', {'queries': [LOADED]})
        if 'LOAD_MANY' in prompt and not answered:
            return self.call('load_tool', {'queries': ['mcp__docs__*', LOADED]})
        if 'ASK_FOR_SKILLS' in prompt and not answered:
            return self.call('load_tool', {'queries': ['skill']})
        if 'WRONG_SKILL' in prompt and not answered:
            return self.call('skill', {'name': 'fixture-procedure'})
        # Load every deferred tool, twenty a call, until a load finds none.
        if 'LOAD_EVERY_TOOL' in last_typed(body) and (not answered or loaded_last(body)):
            return self.call('load_tool', {'queries': ['*']})
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
        self.call('task', {'prompt': prompt, 'agent': 'general', 'description': 'list files'})

    def call(self, name, arguments):
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.end_headers()
        self.stream([{'type': 'message_start', 'message': {'id': f'msg_e2e_{len(REQUESTS)}', 'usage': {'input_tokens': 40, 'output_tokens': 0}}},
                     {'type': 'content_block_start', 'index': 0,
                      'content_block': {'type': 'tool_use', 'id': f'toolu_{len(REQUESTS)}', 'name': name, 'input': {}}},
                     {'type': 'content_block_delta', 'index': 0, 'delta': {'type': 'input_json_delta', 'partial_json': json.dumps(arguments)}},
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
    assert 'edit' in plan_tools and 'write' in plan_tools, plan_tools
    # No browser is attached to this server, so none of its tools are sent.
    assert not any(name.startswith('browser_') for name in plan_tools), plan_tools
    if SYSTEM_FOLDER:
        assert 'BASE_PROMPT_MARKER' in system_text(under_plan), 'the subagent lost the base prompt file'
        after = {path: path.read_bytes() for path in team.rglob('*') if path.is_file()}
        assert after == base_before, 'the system folder was written'
        checks.append({'system folder': ['system.md appended to the main instructions on the wire',
                                       'compaction.md prepended to the summary prompt', 'system folder unchanged']})
        print('PASS system folder prompt files on the wire, system folder unchanged', flush=True)
    checks.append({'subagent prefix': [f'{len(plan_tools)} tools identical under a plan and a build chat',
                                       'system identical', 'edit and write listed under the plan chat, no browser tools without a browser']})
    print(f'PASS subagents of a plan chat and a build chat send one prefix ({len(plan_tools)} tools)', flush=True)

    # 6. A loaded tool outlasts a restart and carries into a fork. Default rules, so `load_tool`
    # is offered; lazy loading as configured by default.
    tools_workspace = root / 'tools-workspace'
    (tools_workspace / '.haarness').mkdir(parents=True, exist_ok=True)
    (tools_workspace / '.haarness/config.toml').write_text(
        f'[defaults]\nprovider="anthropic"\nmodel="{MODEL}"\n[lsp]\nenabled=false\n')
    run(['git', 'init', '-q', str(tools_workspace)])
    tools_args = [str(BINARY), '--workspace', str(tools_workspace), '--trust-project']

    def tool_requests(offset):
        """The chat's own requests since `offset`: a title request carries no tools."""
        return [body for body in REQUESTS[offset:] if body.get('tools')]

    def names(body):
        return [tool['name'] for tool in body['tools']]

    first_connection = connection
    try:
        connection = json.loads(run(tools_args + ['desktop'], input='{"action":"connect"}', env=env, timeout=120))
        loads = chat('Loaded tool fixture')
        offset = len(REQUESTS)
        assert turn(loads, 'LOAD_A_TOOL please')['status'] == 'completed'
        sent = tool_requests(offset)
        assert len(sent) == 2, [names(body) for body in sent]
        assert 'load_tool' in names(sent[0]) and LOADED not in names(sent[0]), names(sent[0])
        loaded = sent[-1]
        assert LOADED in names(loaded), names(loaded)
        subprocess.run(tools_args + ['serve', '--stop'], env=env, capture_output=True, timeout=60)
        connection = json.loads(run(tools_args + ['desktop'], input='{"action":"connect"}', env=env, timeout=120))
        offset = len(REQUESTS)
        assert turn(loads, 'AFTER_RESTART go on')['status'] == 'completed'
        after = tool_requests(offset)[0]
        assert json.dumps(after['tools']) == json.dumps(loaded['tools']), \
            f'the tools changed across the restart: {sorted(set(names(loaded)) ^ set(names(after)))}'
        assert json.dumps(clean(after['system'])) == json.dumps(clean(loaded['system'])), 'the system prompt changed'
        assert clean(after['messages'][:len(loaded['messages'])]) == clean(loaded['messages']), 'an earlier message changed'
        fork = api(f'/threads/{loads}/fork', {})['id']
        offset = len(REQUESTS)
        assert turn(fork, 'IN_THE_FORK go on')['status'] == 'completed'
        forked = tool_requests(offset)[0]
        assert json.dumps(forked['tools']) == json.dumps(loaded['tools']), \
            f'the fork has other tools: {sorted(set(names(loaded)) ^ set(names(forked)))}'
        (EVIDENCE / 'loaded-tool-requests.json').write_text(json.dumps([loaded, after, forked], indent=2))
    finally:
        subprocess.run(tools_args + ['serve', '--stop'], env=env, capture_output=True, timeout=60)
        connection = first_connection
    checks.append({'loaded tool': [f'{LOADED} loaded', f'{len(names(loaded))} tools byte-identical after a restart',
                                   'system and messages unchanged', 'the fork sends the same tools']})
    print(f'PASS a loaded tool stays in the list after a restart and in a fork ({len(names(loaded))} tools)', flush=True)

    # 7. Auto keeps deferring once the loads left too little to defer, across a restart.
    try:
        connection = json.loads(run(tools_args + ['desktop'], input='{"action":"connect"}', env=env, timeout=120))
        everything = chat('Every tool fixture')
        offset = len(REQUESTS)
        assert turn(everything, 'LOAD_EVERY_TOOL please')['status'] == 'completed'
        sent = tool_requests(offset)
        assert len(sent) >= 3, [names(body) for body in sent]
        first, last = sent[0], sent[-1]
        assert 'load_tool' in names(first) and len(names(last)) > len(names(first)), names(last)
        assert 'load_tool' in names(last), 'the chat stopped deferring in the same process'
        catalog = api(f'/tools?threadId={everything}')
        assert catalog['lazyActive'], catalog['lazyActive']
        subprocess.run(tools_args + ['serve', '--stop'], env=env, capture_output=True, timeout=60)
        connection = json.loads(run(tools_args + ['desktop'], input='{"action":"connect"}', env=env, timeout=120))
        offset = len(REQUESTS)
        assert turn(everything, 'AFTER_EVERY_LOAD go on')['status'] == 'completed'
        again = tool_requests(offset)[0]
        assert json.dumps(again['tools']) == json.dumps(last['tools']), \
            f'the tools changed across the restart: {sorted(set(names(last)) ^ set(names(again)))}'
        (EVIDENCE / 'every-tool-requests.json').write_text(json.dumps([first, last, again], indent=2))
    finally:
        subprocess.run(tools_args + ['serve', '--stop'], env=env, capture_output=True, timeout=60)
        connection = first_connection
    checks.append({'kept deferring': [f'{len(sent) - 1} loads, {len(names(last))} tools with the loader',
                                      'byte-identical after a restart']})
    print(f'PASS Auto keeps deferring after the loads and a restart ({len(names(last))} tools)', flush=True)

    # 8. Skills stay listed while lazy loading defers an MCP server's tools.
    skills_workspace = root / 'skills-workspace'
    (skills_workspace / '.haarness').mkdir(parents=True, exist_ok=True)
    (skills_workspace / '.haarness/config.toml').write_text(
        f'[defaults]\nprovider="anthropic"\nmodel="{MODEL}"\n[lsp]\nenabled=false\n')
    server = root / 'mcp-fixture.py'
    server.write_text("""import json, sys
for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request:
        continue
    if request["method"] == "initialize":
        result = {"protocolVersion": "2024-11-05", "capabilities": {"tools": {}}, "serverInfo": {"name": "fixture", "version": "1"}}
    elif request["method"] == "tools/list":
        result = {"tools": [{"name": f"lookup_{i}", "description": "Look up a fixture record by its key and return it",
                             "inputSchema": {"type": "object", "properties": {"key": {"type": "string"}}}} for i in range(10)]}
    else:
        result = {"content": [{"type": "text", "text": "fixture"}]}
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)
""")
    (skills_workspace / '.haarness/haarness.json').write_text(
        json.dumps({'mcp': {'fixture': {'type': 'local', 'command': [sys.executable, str(server)]}}}))
    skill_names = [f'fixture-skill-{index:02d}' for index in range(40)]
    for name in skill_names:
        (skills_workspace / f'.haarness/skills/{name}').mkdir(parents=True, exist_ok=True)
        (skills_workspace / f'.haarness/skills/{name}/SKILL.md').write_text(
            f'---\nname: {name}\ndescription: {name} explains a careful procedure. ' + 'Further detail. ' * 30
            + '\n---\nFollow the fixture procedure.\n')
    (skills_workspace / '.haarness/skills/person-only').mkdir(parents=True, exist_ok=True)
    (skills_workspace / '.haarness/skills/person-only/SKILL.md').write_text(
        '---\nname: person-only\ndescription: PERSON_ONLY_SKILL\ndisable-model-invocation: true\n---\nBody\n')
    run(['git', 'init', '-q', str(skills_workspace)])
    skills_args = [str(BINARY), '--workspace', str(skills_workspace), '--trust-project']
    first_connection = connection
    try:
        connection = json.loads(run(skills_args + ['desktop'], input='{"action":"connect"}', env=env, timeout=120))
        until(lambda: any(item['name'] == 'fixture' and item['status'] == 'connected' for item in api('/mcp')),
              'the MCP fixture did not connect')
        listed = chat('Skill list fixture')
        offset = len(REQUESTS)
        assert turn(listed, 'FIRST skills turn')['status'] == 'completed'
        assert turn(listed, 'WRONG_SKILL please')['status'] == 'completed'
        assert turn(listed, 'ASK_FOR_SKILLS please')['status'] == 'completed'
        assert turn(listed, 'LAST skills turn')['status'] == 'completed'
        sent = tool_requests(offset)
        assert len(sent) == 6, [names(body) for body in sent]
        first = sent[0]
        catalog = api(f'/tools?threadId={listed}')
        deferred = [entry['id'] for entry in catalog['tools'] if entry['enabled'] and not entry['advertised']]
        assert catalog['lazyActive'] and 'load_tool' in names(first), names(first)
        assert any(name.startswith('mcp__fixture__') for name in deferred), deferred
        assert not any(name.startswith('mcp__') for name in names(first)), names(first)
        assert 'skill' in names(first), names(first)
        description = next(tool['description'] for tool in first['tools'] if tool['name'] == 'skill')
        missing = [name for name in skill_names if name not in description]
        assert not missing, missing
        assert 'person-only' not in description and 'PERSON_ONLY_SKILL' not in description, description
        assert ' More skills: ' in description, description[-400:]
        listing = description[description.index(' Available skills:'):]
        assert len(listing.encode()) <= 8 * 1024, len(listing.encode())
        def answer(body):
            block = next(block for block in body['messages'][-1]['content'] if block.get('type') == 'tool_result')
            content = block['content']
            return content if isinstance(content, str) else ''.join(part.get('text', '') for part in content)
        wrong = answer(sent[2])
        assert 'not found: skill fixture-procedure.' in wrong and 'fixture-skill-00' in wrong, wrong
        assert 'person-only' not in wrong and 'load_tool' not in wrong, wrong
        loaded = json.loads(answer(sent[4]))
        assert 'Already in your tool list' in loaded['message'] and loaded['available'] == ['skill'], loaded
        assert [tool['id'] for tool in loaded['loaded']] == ['skill_manage'], loaded
        for body in sent[1:4]:
            assert json.dumps(body['tools']) == json.dumps(first['tools']), \
                f'the tools changed in the chat: {sorted(set(names(first)) ^ set(names(body)))}'
        # The load adds skill_manage on the next round, and it stays.
        for body in sent[4:]:
            assert names(body) == sorted(names(first)[:-1] + ['skill_manage']) + ['load_tool'], names(body)
        assert json.dumps(sent[5]['tools']) == json.dumps(sent[4]['tools']), 'the tools changed after the load'
        (EVIDENCE / 'skill-list-requests.json').write_text(json.dumps(sent, indent=2))
    finally:
        subprocess.run(skills_args + ['serve', '--stop'], env=env, capture_output=True, timeout=60)
        connection = first_connection
    checks.append({'skill list': [f'{len(skill_names)} skills named with MCP tools deferred', 'person-only skill left out',
                                  f'list {len(listing.encode())} bytes', 'an unknown skill name answered with the names',
                                  'load_tool says skill is there and loads skill_manage',
                                  '4 requests with byte-identical tools before that load, 2 after']})
    print(f'PASS the skill tool lists {len(skill_names)} skills while lazy loading defers MCP tools', flush=True)

    # 9. An agent that lists `skills` starts with their text in its system prompt, unchanged from
    # one request of the chat to the next, and none of their commands runs.
    agent_skills_workspace = root / 'agent-skills-workspace'
    (agent_skills_workspace / '.haarness/agents').mkdir(parents=True, exist_ok=True)
    (agent_skills_workspace / '.haarness/config.toml').write_text(
        f'[defaults]\nprovider="anthropic"\nmodel="{MODEL}"\n[lsp]\nenabled=false\n')
    notes = agent_skills_workspace / '.haarness/skills/house-notes'
    notes.mkdir(parents=True, exist_ok=True)
    (notes / 'SKILL.md').write_text('---\nname: house-notes\ndescription: House notes\n---\n'
                                    'HOUSE_NOTES in ${CLAUDE_SKILL_DIR} for ${CLAUDE_SESSION_ID} at ${CLAUDE_PROJECT_DIR}\n!`touch never-ran.txt`\n')
    manual = agent_skills_workspace / '.haarness/skills/by-hand'
    manual.mkdir(parents=True, exist_ok=True)
    (manual / 'SKILL.md').write_text('---\nname: by-hand\ndescription: By hand\ndisable-model-invocation: true\n---\nBY_HAND_ONLY\n')
    (agent_skills_workspace / '.haarness/agents/noted.md').write_text(
        '---\ndescription: Starts with the house notes\nmode: all\nskills: [house-notes, by-hand, nowhere]\n---\nFollow the notes.\n')
    run(['git', 'init', '-q', str(agent_skills_workspace)])
    agent_skills_args = [str(BINARY), '--workspace', str(agent_skills_workspace), '--trust-project']
    first_connection = connection
    try:
        connection = json.loads(run(agent_skills_args + ['desktop'], input='{"action":"connect"}', env=env, timeout=120))
        noted = api('/threads', {'title': 'Noted', 'provider': 'anthropic', 'model': MODEL, 'agent': 'noted'})['id']
        offset = len(REQUESTS)
        assert turn(noted, 'FIRST noted turn')['status'] == 'completed'
        assert turn(noted, 'SECOND noted turn')['status'] == 'completed'
        sent = [body for body in REQUESTS[offset:] if body.get('tools')]
        first, second = sent[0], sent[1]
        system = system_text(first)
        assert f'Follow the notes.\n\nThese skills are already loaded' in system, system[:3000]
        assert f'HOUSE_NOTES in {notes} for {noted} at {agent_skills_workspace}' in system, system[:3000]
        assert '(skipped: `touch never-ran.txt` does not run for a skill an agent starts with' in system, system[:3000]
        assert 'BY_HAND_ONLY' not in system and 'nowhere' not in system, system[:3000]
        assert json.dumps(first['system']) == json.dumps(second['system']), 'the preloaded skills changed between turns'
        assert not (agent_skills_workspace / 'never-ran.txt').exists(), 'a preloaded skill ran its command'
        (EVIDENCE / 'preloaded-skill-requests.json').write_text(json.dumps([first, second], indent=2))
    finally:
        subprocess.run(agent_skills_args + ['serve', '--stop'], env=env, capture_output=True, timeout=60)
        connection = first_connection
    checks.append({'preloaded skills': ['skill text after the agent prompt', 'folder and chat id filled in',
                                        'its command named, not run', 'manual-only and missing left out',
                                        'system byte-identical across turns']})
    print('PASS an agent starts with the skills it lists, the same on every request', flush=True)

    # 10. An MCP server with more tools than one load takes: a tool named by its exact id loads
    # ahead of the server's wildcard in the same call, and the answer names what it left out.
    many_workspace = root / 'many-workspace'
    (many_workspace / '.haarness').mkdir(parents=True, exist_ok=True)
    (many_workspace / '.haarness/config.toml').write_text(
        f'[defaults]\nprovider="anthropic"\nmodel="{MODEL}"\n[lsp]\nenabled=false\n')
    many_server = root / 'mcp-many.py'
    many_server.write_text(server.read_text().replace('range(10)', 'range(25)'))
    (many_workspace / '.haarness/haarness.json').write_text(
        json.dumps({'mcp': {'docs': {'type': 'local', 'command': [sys.executable, str(many_server)]}}}))
    run(['git', 'init', '-q', str(many_workspace)])
    many_args = [str(BINARY), '--workspace', str(many_workspace), '--trust-project']
    first_connection = connection
    try:
        connection = json.loads(run(many_args + ['desktop'], input='{"action":"connect"}', env=env, timeout=120))
        until(lambda: any(item['name'] == 'docs' and item['status'] == 'connected' for item in api('/mcp')),
              'the MCP fixture with many tools did not connect')
        many = chat('Many tools fixture')
        offset = len(REQUESTS)
        assert turn(many, 'LOAD_MANY please')['status'] == 'completed'
        sent = tool_requests(offset)
        assert len(sent) == 2, [names(body) for body in sent]
        answer = load_answer(sent[1])
        ids = [tool['id'] for tool in answer['loaded']]
        lookups = [f'mcp__docs__lookup_{index}' for index in range(25)]
        assert ids == [LOADED] + sorted(lookups)[:19], ids
        # The word also finds cloud_schedule, after the server's tools.
        left_out = ', '.join(sorted(lookups)[19:] + ['cloud_schedule'])
        assert f'past the 20 tools one call loads, so not loaded: {left_out}. Load the ones you need by exact id.' \
            in answer['message'], answer['message']
        assert set(ids) <= set(names(sent[1])), sorted(set(ids) - set(names(sent[1])))
        (EVIDENCE / 'many-tools-requests.json').write_text(json.dumps(sent, indent=2))
    finally:
        subprocess.run(many_args + ['serve', '--stop'], env=env, capture_output=True, timeout=60)
        connection = first_connection
    checks.append({'many tools': [f'{LOADED} named exactly loads ahead of mcp__docs__*',
                                  '20 loaded, the 7 left out named']})
    print('PASS an exact id loads ahead of a wildcard, and the matches past twenty are named', flush=True)
    (EVIDENCE / 'results.json').write_text(json.dumps(checks, indent=2))
finally:
    subprocess.run(['tmux', 'kill-session', '-t', session], capture_output=True)
    if connection:
        subprocess.run(args + ['serve', '--stop'], env=env, capture_output=True, timeout=60)
    service.shutdown()
    if holder:
        holder.cleanup()
