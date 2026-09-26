#!/usr/bin/env python3
"""Opt-in real Cloud/Fly tool test through a disposable daemon and tmux TUI.

Requires HAARNESS_CLOUD_TOOLS_LIVE=1, HAARNESS_CLOUD_API_URL, and an existing
HAARNESS_CLOUD_CONFIG_DIR login. Creates and deletes only its own sessions/template.
A real Cloud-backed orchestrator chooses the freestyle and template launch tools.
The echo provider deterministically selects follow-up tools; remote replies use
an actual Cloud model. Evidence contains IDs/results, never account credentials.
"""
import json
import os
from pathlib import Path
import shlex
import subprocess
import tempfile
import time
import urllib.request
import urllib.error
import uuid

REPO = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get('HAARNESS_BIN', REPO / 'target/debug/haarness')).resolve()
OUT = Path(os.environ.get('HAARNESS_CLOUD_TOOLS_EVIDENCE', REPO / 'target/cloud-tools-live'))
MODEL = os.environ.get('HAARNESS_CLOUD_TOOLS_MODEL', 'openai/gpt-5-mini')
ORCHESTRATOR = os.environ.get('HAARNESS_CLOUD_TOOLS_ORCHESTRATOR', 'openai/gpt-5-mini')
if os.environ.get('HAARNESS_CLOUD_TOOLS_LIVE') != '1':
    raise SystemExit('Opt in with HAARNESS_CLOUD_TOOLS_LIVE=1; this test starts billable test sessions.')
for required in ['HAARNESS_CLOUD_API_URL', 'HAARNESS_CLOUD_CONFIG_DIR']:
    if not os.environ.get(required):
        raise SystemExit(f'{required} is required')
OUT.mkdir(parents=True, exist_ok=True)


def run(args, **kwargs):
    process = subprocess.run(args, capture_output=True, text=True, **kwargs)
    if process.returncode:
        raise RuntimeError(process.stderr.strip() or f'Command exited {process.returncode}')
    return process.stdout


def until(check, label, timeout=180):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(.1)
    raise AssertionError(label)


with tempfile.TemporaryDirectory(prefix='haarness-cloud-tools-live-') as temporary:
    root = Path(temporary).resolve()
    workspace = root / 'workspace'
    (workspace / '.haarness').mkdir(parents=True)
    (workspace / '.haarness/config.toml').write_text('''[defaults]
provider="echo"
model="echo-1"
[permissions]
cloud_agent_manage="allow"
cloud_agent_session="allow"
cloud_chat="allow"
cloud_schedule="allow"
[lazy_tools]
mode="off"
[lsp]
enabled=false
''')
    run(['git', 'init', '-q', str(workspace)])
    env = {k: os.environ[k] for k in ['PATH', 'HOME', 'USER', 'LANG', 'TMPDIR',
           'HAARNESS_CLOUD_API_URL', 'HAARNESS_CLOUD_CONFIG_DIR'] if k in os.environ}
    env.update(HAARNESS_DATA_DIR=str(root / 'data'), HAARNESS_HOME=str(root / 'home'),
               HAARNESS_TEST_PROVIDER='1', HAARNESS_LOG='warn,haarness_account::cloud::events=debug')
    args = [str(BINARY), '--workspace', str(workspace)]
    endpoint = json.loads(run(args + ['desktop'], env=env, input='{"action":"connect"}', timeout=120))
    tmux = f'haarness-cloud-tools-{os.getpid()}'
    sessions, agent_id, checks = [], None, []
    deleted, cleanup_errors = set(), []

    def api(path, body=None, method=None):
        req = urllib.request.Request(endpoint['url'] + '/v1' + path, method=method,
            data=None if body is None else json.dumps(body).encode(),
            headers={'Authorization': 'Bearer ' + endpoint['token'], 'Content-Type': 'application/json'})
        with urllib.request.urlopen(req, timeout=30) as response:
            value = response.read()
            return json.loads(value) if value else None

    def new_local(title):
        return api('/threads', {'title': title, 'provider': 'echo', 'model': 'echo-1'})['id']

    local = new_local('Reactive Cloud tools verification')

    def start_tool(name, data, thread=None):
        return api(f'/threads/{thread or local}/turns', {'requestId': str(uuid.uuid4()),
            'text': '!tool ' + name + ' ' + json.dumps(data)})

    def result(turn, thread=None, timeout=180):
        rows = until(lambda: [r['data'] for r in api(f'/threads/{thread or local}/items')
            if r.get('turnId') == turn and r['data']['type'] == 'tool_result'], 'tool result missing', timeout)
        row = rows[-1]
        assert not row.get('is_error'), row
        until(lambda: api(f'/turns/{turn}')['status'] not in ['running', 'pending', 'queued'], 'local turn still active')
        return json.loads(row['output'])

    def tool(name, data, thread=None, timeout=180):
        return result(start_tool(name, data, thread)['id'], thread, timeout)

    def session(action, **fields):
        return tool('cloud_agent_session', {'action': action, **fields})

    def launch_with_model(prompt, label, thread=None, new=True):
        thread = thread or api('/threads', {'title': label, 'provider': 'haarness', 'model': ORCHESTRATOR})['id']
        turn = api(f'/threads/{thread}/turns', {'requestId': str(uuid.uuid4()), 'text': prompt})['id']

        def completed():
            # Record every newly created resource as soon as its tool returns, even on failure.
            for row in api(f'/threads/{thread}/items'):
                data = row['data']
                if data['type'] == 'tool_result' and data.get('name') == 'cloud_agent_session' and not data.get('is_error'):
                    output = json.loads(data['output'])
                    sid = output.get('sessionId')
                    if sid and sid not in sessions:
                        sessions.append(sid)
            state = api(f'/turns/{turn}')
            return state if state['status'] not in ['running', 'pending', 'queued'] else None

        final = until(completed, 'real orchestrator did not finish', 240)
        rows = [r for r in api(f'/threads/{thread}/items') if r.get('turnId') == turn]
        assert final['status'] == 'completed', final
        results = [r['data'] for r in rows if r['data']['type'] == 'tool_result']
        assert results and not any(r.get('is_error') for r in results), results
        calls = [r['data'] for r in rows if r['data']['type'] == 'tool_call']
        launches = [c for c in calls if c['name'] == 'cloud_agent_session' and c['arguments'].get('action') in ['ask', 'create'] and not c['arguments'].get('sessionId')]
        assert len(launches) == int(new), calls
        asks = [json.loads(r['output']) for r in results if r['name'] in ['cloud_agent_session','cloud_chat'] and json.loads(r['output']).get('turnId')]
        assert asks, results
        (OUT / (label + '-tool-calls.json')).write_text(json.dumps(calls, indent=2))
        return asks[-1], calls, launches[0]['arguments'] if new else {}, thread

    def capture():
        return run(['tmux', 'capture-pane', '-p', '-t', tmux])

    def proof(name, value=None):
        checks.append(name)
        if value is not None:
            (OUT / (name + '.json')).write_text(json.dumps(value, indent=2))
        print('PASS:', name, flush=True)

    def delete_owned(sid):
        assert sid in sessions
        if sid in deleted:
            return
        operation = session('delete', sessionId=sid)['operation']['id']
        uuid.UUID(operation)
        credentials = Path(env['HAARNESS_CLOUD_CONFIG_DIR']) / 'credentials.json'

        class NoRedirect(urllib.request.HTTPRedirectHandler):
            def redirect_request(self, *args, **kwargs):
                return None

        def deletion_finished():
            saved = json.loads(credentials.read_text())
            assert saved['apiUrl'].rstrip('/') == env['HAARNESS_CLOUD_API_URL'].rstrip('/')
            req = urllib.request.Request(saved['apiUrl'].rstrip('/') + '/api/v1/operations/' + operation,
                headers={'Authorization': 'Bearer ' + saved['accessToken']})
            with urllib.request.build_opener(NoRedirect).open(req, timeout=20) as response:
                state = json.load(response)
            assert state['status'] not in ['failed', 'cancelled'], state
            if state['status'] == 'succeeded':
                return state
            time.sleep(1)

        state = until(deletion_finished, 'test session deletion incomplete', 120)
        deleted.add(sid)
        proof('deleted-' + sid, state)

    try:
        command = shlex.join(['env', *[f'{k}={v}' for k, v in env.items()], *args, '--thread', local])
        run(['tmux', 'new-session', '-d', '-x', '155', '-y', '48', '-s', tmux, command])
        until(lambda: 'echo' in capture().lower(), 'TUI did not open', 30)
        models = tool('cloud_agent_manage', {'action': 'models', 'search': MODEL})
        assert any(m['id'] == MODEL for m in models['models']), models
        proof('model-discovery')

        # Freestyle launch: no cloudAgentId anywhere in the create arguments.
        name = 'Reactive tools ' + str(uuid.uuid4())[:8]
        prompt = f"""Start a fresh Cloud session called {name + ' freestyle'!r} without using a saved Cloud agent.
Use {MODEL} and allow shell commands in this disposable test session. Ask it to run uname -s
and reply with FREESTYLE_PUSH_OK plus the operating system. Wait up to 180 seconds and bring back
its answer and session ID. Please create only one session, and don't run the command locally."""
        reply, calls, launch, freestyle_chat = launch_with_model(prompt, 'freestyle-real-orchestrator')
        sid = reply['sessionId']
        assert not launch.get('cloudAgentId'), launch
        created = session('get', sessionId=sid)
        assert created['session'].get('cloudAgentId') is None, created
        assert created['session']['title'] == name + ' freestyle', created
        proof('freestyle-start-websocket', created)
        assert reply['status'] == 'completed', reply
        assert 'FREESTYLE_PUSH_OK' in json.dumps(reply['messages']) and 'Linux' in json.dumps(reply['messages']), reply
        remote_thread = reply['threadId']
        proof('freestyle-real-model-reply', reply)

        # Submit from the actual tmux input, then verify the rendered result.
        command = '!tool cloud_agent_session ' + json.dumps({'action': 'messages', 'sessionId': sid, 'threadId': remote_thread})
        before = {r['id'] for r in api(f'/threads/{local}/items')}
        run(['tmux', 'send-keys', '-l', '-t', tmux, ' ' + command])
        run(['tmux', 'send-keys', '-t', tmux, 'Enter'])
        row = until(lambda: next((r for r in api(f'/threads/{local}/items') if r['id'] not in before and r['data']['type'] == 'tool_result'), None), 'tmux tool result missing')
        result(row['turnId'])
        until(lambda: 'FREESTYLE_PUSH_OK' in capture(), 'tmux did not render remote reply')
        (OUT / 'tmux-messages.txt').write_text(capture())
        proof('tmux-tool-messages')

        delayed = session('ask', sessionId=sid, threadId=remote_thread,
            message="Call bash exactly ONCE with: sleep 4; printf 'FREESTYLE_DELAY_OK\\n'. After success, call no more tools and finish with the single line FREESTYLE_DELAY_OK.", timeoutSeconds=1)
        assert delayed['status'] in ['running', 'pending', 'queued', 'in_progress'], delayed
        continued = session('wait', sessionId=sid, threadId=remote_thread, turnId=delayed['turnId'], timeoutSeconds=120)
        assert continued['status'] == 'completed' and 'FREESTYLE_DELAY_OK' in json.dumps(continued['messages']), continued
        proof('timeout-then-wait-no-resubmission', continued)

        # Cancelling an observation must leave the remote turn alone.
        pending = session('ask', sessionId=sid, threadId=remote_thread,
            message="Use bash to run sleep 30, then reply LONG_WAIT_OK.", wait=False, timeoutSeconds=120)
        watcher = new_local('Cancel Cloud observation')
        waiting = start_tool('cloud_agent_session', {'action': 'wait', 'sessionId': sid,
            'threadId': remote_thread, 'turnId': pending['turnId']}, watcher)
        until(lambda: any(r['data']['type'] == 'tool_call' for r in api(f'/threads/{watcher}/items')), 'wait tool not invoked')
        time.sleep(.5)
        api(f"/turns/{waiting['id']}/interrupt", {})
        until(lambda: api(f"/turns/{waiting['id']}")['status'] == 'interrupted', 'local observation not interrupted')
        still_running = session('wait', sessionId=sid, threadId=remote_thread, turnId=pending['turnId'], timeoutSeconds=1)
        assert still_running['status'] in ['running', 'pending', 'queued', 'in_progress'], still_running
        session('abort', sessionId=sid, threadId=remote_thread, turnId=pending['turnId'])
        stopped_turn = session('wait', sessionId=sid, threadId=remote_thread, turnId=pending['turnId'])
        assert stopped_turn['status'] == 'interrupted', stopped_turn
        proof('cancel-observer-then-explicit-abort', stopped_turn)

        # The real model must create another chat, preserving the existing machine/filesystem.
        second, chat_calls, _, _ = launch_with_model(
            f"In Cloud session {sid}, open a second conversation named 'Separate review'. This must share the same machine and files and have its own history. Ask the new conversation to reply SECOND_CHAT_OK and bring back its reply. Do not create another Cloud session.",
            'second-chat-natural-request', new=False)
        assert second['sessionId'] == sid and second['chatId'] != remote_thread, second
        assert any(c['name']=='cloud_chat' and c['arguments']['action']=='create' for c in chat_calls), chat_calls
        assert 'SECOND_CHAT_OK' in json.dumps(second['messages']), second
        proof('model-distinguishes-session-and-chat', second)
        chat_listing=api('/cloud/chats',{'action':'list','sessionId':sid},'POST')
        assert len(chat_listing['chats'])==2,chat_listing
        # A natural Cloud scheduling request must not install a timer on this computer.
        from datetime import datetime, timezone, timedelta
        scheduled_at=(datetime.now(timezone.utc)+timedelta(seconds=60)).isoformat()
        orchestrator=api('/threads',{'title':'Cloud schedule wording','provider':'haarness','model':ORCHESTRATOR})['id']
        turn=api(f'/threads/{orchestrator}/turns',{'requestId':str(uuid.uuid4()),'text':f"At {scheduled_at}, have chat {second['chatId']} in Cloud session {sid} reply CLOUD_SCHEDULE_OK once. It must run in the cloud even if my computer is off. Schedule it now and report its ID; do not send it immediately or create another machine."})['id']
        final=until(lambda: (r if (r:=api(f'/turns/{turn}'))['status'] not in ['running','pending','queued'] else None),'schedule model timeout',180)
        assert final['status']=='completed',final
        scheduled_rows=[r['data'] for r in api(f'/threads/{orchestrator}/items') if r.get('turnId')==turn]
        scheduled_calls=[r for r in scheduled_rows if r['type']=='tool_call']
        assert any(c['name']=='cloud_schedule' and c['arguments']['action']=='create' for c in scheduled_calls),scheduled_calls
        assert not any(c['name']=='schedule' for c in scheduled_calls),scheduled_calls
        created_schedules=[json.loads(r['output']) for r in scheduled_rows if r['type']=='tool_result' and r['name']=='cloud_schedule' and not r.get('is_error') and '"id"' in r['output']]
        scheduled=next(r for r in created_schedules if r.get('chatId')==second['chatId'])
        proof('model-selects-cloud-schedule',scheduled_calls)
        def scheduled_done():
            history=api('/cloud/schedules',{'action':'history','id':scheduled['id']},'POST')
            for outcome in history['items']:
                if outcome['status'] in ['completed','failed','cancelled','interrupted','needs_attention']:
                    assert outcome['status']=='completed',outcome
                    return outcome
            time.sleep(1)
        outcome=until(scheduled_done,'Cloud timed run did not finish',180)
        scheduled_reply=tool('cloud_chat',{'action':'messages','sessionId':sid,'chatId':second['chatId']})
        assert 'CLOUD_SCHEDULE_OK' in json.dumps(scheduled_reply),scheduled_reply
        proof('real-timed-cloud-chat-run',outcome)
        tool('cloud_schedule',{'action':'delete','id':scheduled['id']})
        # Read-only API routes expose the same identities as the model tools.
        assert api('/cloud/sessions',{'action':'get','sessionId':sid},'POST')['session']['id']==sid
        assert api('/cloud/agents',{'action':'list'},'POST').get('items') is not None

        # Save, list, and use a reusable agent through the same LLM-visible tools.
        current = session('get', sessionId=sid)['session']
        saved = session('save_agent', sessionId=sid, version=current['version'], name=name + ' template')
        agent_id = saved['id']
        listed = tool('cloud_agent_manage', {'action': 'list', 'search': name + ' template'})
        selected = next(a for a in listed['items'] if a['id'] == agent_id)
        # Release this test's first runtime before starting another; its saved template persists.
        delete_owned(sid)
        prompt = f"""Find my saved Cloud agent named {name + ' template'!r}. Use it to start one new session
called {name + ' templated'!r}, have it reply TEMPLATE_PUSH_OK, and bring back its answer and session ID.
Wait up to 180 seconds if needed. Don't edit the template or start extra sessions."""
        reply, calls, launch, template_chat = launch_with_model(prompt, 'template-real-orchestrator')
        assert any(c['name'] == 'cloud_agent_manage' and c['arguments']['action'] == 'list' for c in calls), calls
        assert launch['cloudAgentId'] == selected['id'], launch
        templated = session('get', sessionId=reply['sessionId'])
        assert templated['session']['cloudAgentId'] == agent_id, templated
        assert reply['status'] == 'completed' and 'TEMPLATE_PUSH_OK' in json.dumps(reply['messages']), reply
        proof('list-template-and-launch-real-session', reply)
        template_sid = reply['sessionId']
        followup, calls, _, _ = launch_with_model(
            'Continue in that same Cloud session and ask it to reply SAME_SESSION_PUSH_OK. Wait for its reply; do not start another session.',
            'same-session-natural-followup', thread=template_chat, new=False)
        assert followup['sessionId'] == template_sid and 'SAME_SESSION_PUSH_OK' in json.dumps(followup['messages']), followup
        proof('natural-followup-reuses-session', followup)
        delete_owned(template_sid)

        alternative, calls, launch, _ = launch_with_model(
            f"I don't have an agent template for this. Can you ask a new Cloud session called {name + ' no template'!r} to reply NO_TEMPLATE_NEEDED_OK? Use {MODEL}, wait for the answer and give me its session ID. Only one session please.",
            'no-template-natural-request')
        assert not launch.get('cloudAgentId'), launch
        assert alternative['status'] == 'completed' and 'NO_TEMPLATE_NEEDED_OK' in json.dumps(alternative['messages']), alternative
        proof('no-template-needed-natural-request', alternative)

        logs = '\n'.join(p.read_text(errors='replace') for p in (root / 'data').rglob('haarness.log*') if p.is_file())
        connected = [line for line in logs.splitlines() if 'Cloud event stream connected' in line]
        assert any('websocket' in line for line in connected), 'no real control-plane WebSocket connection'
        assert any('sse' in line for line in connected), 'no real runtime stream connection'
        (OUT / 'transports.txt').write_text('\n'.join(connected) + '\n')
        proof('real-websocket-and-runtime-stream')
    finally:
        # Exact IDs captured from this run only. Leave all pre-existing work untouched.
        for sid in sessions:
            try:
                delete_owned(sid)
            except Exception as error:
                print(f'CLEANUP NEEDED session {sid}: {error}', flush=True)
                cleanup_errors.append(sid)
        if agent_id:
            try:
                tool('cloud_agent_manage', {'action': 'delete', 'id': agent_id})
            except Exception as error:
                print(f'CLEANUP NEEDED agent {agent_id}: {error}', flush=True)
                cleanup_errors.append(agent_id)
        (OUT / 'summary.json').write_text(json.dumps({'checks': checks, 'sessions': sessions, 'agent': agent_id, 'deleted': sorted(deleted)}, indent=2))
        subprocess.run(['tmux', 'kill-session', '-t', tmux], capture_output=True)
        run(args + ['serve', '--stop'], env=env, timeout=30)

        if cleanup_errors:
            raise RuntimeError('Cleanup incomplete for: ' + ', '.join(cleanup_errors))
