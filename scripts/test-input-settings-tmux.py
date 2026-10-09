#!/usr/bin/env python3
"""Owned daemon + attached tmux chat, scoped JSON settings and live delivery proof."""
import json, os, shlex, subprocess, sys, time, urllib.request, urllib.error
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'sdks/tests'))
from server import Fixture, BINARY

def run(*args, **kwargs): return subprocess.check_output(args, text=True, **kwargs)
def until(check, message):
    deadline = time.monotonic() + 40
    while time.monotonic() < deadline:
        result = check()
        if result: return result
        time.sleep(.1)
    raise AssertionError(message)

server = Fixture()
session = f'haarness-input-settings-{os.getpid()}'
evidence = []
try:
    config = server.workspace / '.haarness/config.toml'
    config.write_text(config.read_text().replace('bash="ask"', 'bash="allow"'))
    server.start()
    global_file = server.data / 'config/haarness.jsonc'
    global_file.write_text('{\n// keep global comment\n"input":{"delivery":"queue"},"unrelated":123\n}\n')
    project_file = server.workspace / '.haarness/haarness.jsonc'
    project_file.write_text('{\n// keep project comment\n"input":{"other":true},"unrelated":456\n}\n')
    def api(path, body=None, expected=200, auth=True):
        headers = {'Content-Type': 'application/json'}
        if auth: headers['Authorization'] = 'Bearer ' + server.token
        req = urllib.request.Request(server.url+'/v1'+path, data=None if body is None else json.dumps(body).encode(), headers=headers)
        try:
            with urllib.request.urlopen(req, timeout=10) as response:
                status=response.status; raw=response.read(); result=json.loads(raw) if raw else None
        except urllib.error.HTTPError as error:
            status=error.code; result=error.read().decode()
        assert status == expected, (status, expected, result)
        return result
    def setting(action='get', **kwargs): return api('/manage/input-settings', {'action':action, **kwargs})['data']
    assert setting()['delivery'] == 'queue'
    assert 'input-settings.v1' in api('/info')['capabilities']
    api('/manage/input-settings', {'action':'toggle'}, expected=401, auth=False)
    before = global_file.read_bytes()
    # Attach to the owned server. Config belongs to that server, regardless of
    # the client's current working directory or its private application state.
    command = shlex.join(['env', f'HAARNESS_DATA_DIR={server.data}', 'HAARNESS_TEST_PROVIDER=1', f'HAARNESS_SERVER_TOKEN={server.token}', str(BINARY), 'attach', server.url])
    run('tmux','new-session','-d','-x','170','-y','52','-s',session,command)
    def pane(): return run('tmux','capture-pane','-p','-t',session)
    def text(value): run('tmux','send-keys','-l','-t',session,value)
    def keys(*values): run('tmux','send-keys','-t',session,*values)
    def send(value): text(value if value.startswith('/') else ' '+value); keys('Enter')
    until(lambda:'h a a r n e s s' in pane(), 'TUI did not open')
    send('/input-mode')
    until(lambda:setting()['delivery']=='steer', 'project toggle did not override inherited queue')
    assert global_file.read_bytes() == before
    assert '// keep project comment' in project_file.read_text()
    assert json.loads(project_file.read_text().replace('// keep project comment',''))['unrelated']==456
    until(lambda:'Input mode: steer' in pane(), 'dedicated command confirmation missing')
    evidence.append('DEDICATED INPUT-MODE COMMAND\n'+pane())
    send('!tool bash {"command":"sleep 20; printf real-input-settings-tool"}')
    turn=until(lambda:api('/turns/active'), 'chat did not start')[0]
    tid=turn['threadId']
    queue=lambda:api(f'/threads/{tid}/input/queue')['items']
    items=lambda:api(f'/threads/{tid}/items')
    send('/input-mode queue')
    until(lambda:setting()['delivery']=='queue','explicit queue mode did not save during active chat')
    send('queued by ordinary Enter')
    until(lambda:len(queue())==1,'normal Enter did not queue')
    assert not any(i['data'].get('text','').strip()=='queued by ordinary Enter' for i in items())
    send('/input-mode')
    until(lambda:setting()['delivery']=='steer','slash command was deferred or toggle failed')
    assert len(queue())==1, 'changing preference delivered an existing queued message'
    send('steering by ordinary Enter')
    until(lambda:any(i['data'].get('text','').strip()=='steering by ordinary Enter' for i in items()),'normal Enter did not steer')
    text('explicit Alt Enter queue'); keys('M-Enter')
    until(lambda:len(queue())==2,'explicit queue shortcut regressed')
    evidence.append('ACTIVE CHAT STEER AND QUEUE\n'+pane())
    until(lambda:not api('/turns/active'),'chat failed to finish')
    until(lambda:not queue(),'deferred messages failed to drain')
    answers=[i['data'].get('text','') for i in items() if i['data']['type']=='assistant_message']
    assert any('queued by ordinary Enter' in t for t in answers), answers
    assert any('steering by ordinary Enter' in t for t in answers), answers
    send('/input-mode reset')
    until(lambda:setting()['scopeDelivery'] is None,'reset did not remove override')
    assert setting()['delivery']=='queue'
    send('idle queue starts immediately')
    until(lambda:any(i['data']['type']=='assistant_message' and 'idle queue starts immediately' in i['data'].get('text','') for i in items()),'idle queue did not run')
    # An external client edits Global while the TUI stays connected.
    setting('set', scope='global', delivery='steer')
    assert setting()['delivery']=='steer'
    send('/input-mode queue')
    until(lambda:setting()['scopeDelivery']=='queue','local override was not restored')
    setting('set', scope='global', delivery='queue')
    setting('set', scope='global', delivery='steer')
    assert setting()['delivery']=='queue', 'global edit beat local override'
    assert '// keep global comment' in global_file.read_text()
    evidence.append('COMPLETED CHAT\n'+pane())
    run('tmux','kill-session','-t',session)
    # Restart only this owned server; persisted settings and chat must survive.
    server.process.terminate();server.process.wait(timeout=15);server.process.stdout.close();server.log.close();server.start()
    assert setting()['delivery']=='queue'
    assert len([i for i in items() if i['data']['type']=='assistant_message'])>=3
    Path('/tmp/haarness-input-settings-tmux.txt').write_text('\n\n'.join(evidence))
    print('PASS: real attached TUI chat/tool execution, dedicated slash command, project/global precedence, comment preservation, toggle/set/reset, idle queue, active steer/queue, slash immediacy, explicit Alt+Enter and restart persistence')
except BaseException:
    try: Path('/tmp/haarness-input-settings-tmux-failure.txt').write_text(pane())
    except Exception: pass
    raise
finally:
    subprocess.run(['tmux','kill-session','-t',session],capture_output=True)
    server.close()
