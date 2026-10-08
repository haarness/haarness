#!/usr/bin/env python3
"""Real Haarness server + real OpenTUI in tmux: Auto/Review permission lifecycle."""
import json, os, pathlib, shlex, subprocess, tempfile, time, urllib.request, uuid

binary = pathlib.Path(os.environ.get('HAARNESS_BIN', 'target/release/haarness')).resolve()
root = pathlib.Path(tempfile.mkdtemp(prefix='haarness-auto-e2e-')).resolve()
workspace = root / 'workspace'
workspace.mkdir()
(workspace / '.haarness').mkdir()
(workspace / '.haarness' / 'config.toml').write_text('[defaults]\nprovider="echo"\nmodel="echo-1"\n[permissions]\nbash = { "*" = "ask", "printf denied-proof*" = "deny" }\n')
env = dict(os.environ, HAARNESS_DATA_DIR=str(root/'data'), HAARNESS_TEST_PROVIDER='1', HAARNESS_DEFAULT_PROVIDER='echo', HAARNESS_DEFAULT_MODEL='echo-1')
name = 'haarness-auto-' + str(os.getpid())
endpoint = None
checks = []

def run(args, **kwargs):
    return subprocess.run(args, check=True, text=True, capture_output=True, **kwargs).stdout

def connect():
    return json.loads(run([str(binary), '--workspace', str(workspace), '--trust-project', 'desktop'], env=env, input='{"action":"connect"}', timeout=120))

def api(path, value=None, method=None):
    req = urllib.request.Request(endpoint['url']+path, data=None if value is None else json.dumps(value).encode(), headers={'Authorization':'Bearer '+endpoint['token'], 'Content-Type':'application/json'}, method=method)
    with urllib.request.urlopen(req, timeout=10) as reply:
        body = reply.read()
        return json.loads(body) if body else None

def until(check, label, seconds=25):
    end = time.monotonic()+seconds
    while time.monotonic()<end:
        if check():
            return
        time.sleep(.08)
    raise AssertionError(label)

def capture():
    return run(['tmux','capture-pane','-p','-t',name+':0.0'])

def keys(*keys):
    run(['tmux','send-keys','-t',name+':0.0',*keys])

def command(text):
    keys('C-u')
    run(['tmux','send-keys','-l','-t',name+':0.0',text])
    keys('Enter')

def launch(thread, mode='review'):
    args=['env',*[k+'='+env[k] for k in ['HAARNESS_DATA_DIR','HAARNESS_TEST_PROVIDER','HAARNESS_DEFAULT_PROVIDER','HAARNESS_DEFAULT_MODEL']],str(binary),'--workspace',str(workspace),'--trust-project','--thread',thread]
    run(['tmux','new-session','-d','-x','160','-y','40','-s',name,shlex.join(args)])
    until(lambda:mode in capture(), mode+' mode not visible at launch')

def start(thread, text):
    return api('/v1/threads/'+thread+'/turns', {'requestId':str(uuid.uuid4()),'text':text})

def finished():
    until(lambda:not api('/v1/turns/active'), 'turn did not finish')

def proof(file, expected):
    path = workspace/file
    until(lambda:path.exists() and path.read_text()==expected, 'missing execution proof '+file)
    finished()

def check(label):
    checks.append(label)
    print('PASS '+label, flush=True)

try:
    endpoint = connect()
    thread = api('/v1/threads', {'title':'Auto mode verification','provider':'echo','model':'echo-1'})['id']
    text = '!tool bash '+json.dumps({'command':'printf once-proof > once.txt'})
    start(thread,text)
    until(lambda:len(api('/v1/permissions'))==1,'initial permission missing')
    launch(thread)
    keys('C-y')
    until(lambda:'auto' in capture(),'Ctrl+Y did not enable Auto')
    proof('once.txt','once-proof')
    assert api('/v1/threads/'+thread+'/approval-mode')['mode']=='auto'
    check('Ctrl+Y sets Auto on the server and answers the request already waiting')
    keys('C-y')
    until(lambda:'review' in capture(),'Ctrl+Y did not return to Review')
    start(thread,'!tool bash '+json.dumps({'command':'printf u-proof > u.txt'}))
    until(lambda:len(api('/v1/permissions'))==1,'Review did not ask')
    time.sleep(.3)
    keys('u')
    proof('u.txt','u-proof')
    until(lambda:'auto' in capture(),'Once + Auto did not enable Auto')
    check('Allow once + Auto executes the pending request and enables Auto')
    start(thread,text)
    finished()
    assert not api('/v1/permissions')
    check('new requests auto-approve once')
    other = api('/v1/threads', {'title':'Background review verification','provider':'echo','model':'echo-1'})['id']
    start(other,'!tool bash '+json.dumps({'command':'printf background-proof > background.txt'}))
    until(lambda:len(api('/v1/permissions'))==1,'another chat did not ask')
    time.sleep(.3)
    assert not (workspace/'background.txt').exists()
    pending=api('/v1/permissions')
    assert pending[0]['threadId']==other
    api('/v1/permissions/'+pending[0]['permissionId'],{'decision':'reject'})
    finished()
    start(thread,'!tool task '+json.dumps({'agent':'general','description':'Auto child','prompt':'!tool bash '+json.dumps({'command':'printf child-proof > child.txt'})}))
    proof('child.txt','child-proof')
    check('another chat stays in Review while this chat and its agents run in Auto')
    start(thread,'!tool question '+json.dumps({'prompt':'User input stays manual','choices':['Continue']}))
    until(lambda:len(api('/v1/questions'))==1,'question missing')
    time.sleep(.2)
    questions=api('/v1/questions')
    assert len(questions)==1
    api('/v1/questions/'+questions[0]['questionId'],{'answer':'Continue'})
    finished()
    start(thread,'!tool bash '+json.dumps({'command':'printf denied-proof > denied.txt'}))
    finished()
    assert not (workspace/'denied.txt').exists()
    assert 'denied' in json.dumps(api('/v1/threads/'+thread+'/items')).lower()
    check('questions remain manual and explicit deny rules win')
    command('/permissions-mode review')
    until(lambda:'asks before acting' in capture(),'Review command did not work')
    start(thread,text)
    until(lambda:len(api('/v1/permissions'))==1,'Auto incorrectly learned an Always rule')
    keys('n')
    finished()
    check('Review prompts again for the exact same command; no Always rule is stored')
    # Another client switches the chat; the open TUI follows.
    api('/v1/threads/'+thread+'/approval-mode',{'mode':'auto'},'PUT')
    until(lambda:'auto' in capture(),'TUI did not follow a change made elsewhere')
    check('the TUI shows a mode changed by another client')
    run([str(binary),'--workspace',str(workspace),'serve','--stop'],env=env,timeout=30)
    endpoint=connect()
    start(thread,'!tool bash '+json.dumps({'command':'printf reconnect-proof > reconnect.txt'}))
    proof('reconnect.txt','reconnect-proof')
    check('Auto survives a real daemon restart and reconnect')
    run(['tmux','kill-session','-t',name])
    start(thread,'!tool bash '+json.dumps({'command':'printf exit-proof > exit.txt'}))
    proof('exit.txt','exit-proof')
    launch(thread,'auto')
    check('Auto belongs to the chat: it keeps working after the TUI exits and shows on relaunch')
    command('/new')
    until(lambda:api('/v1/threads') and any(item['title']=='New thread' for item in api('/v1/threads')),'new chat missing')
    fresh=[item['id'] for item in api('/v1/threads') if item['title']=='New thread'][-1]
    until(lambda:api('/v1/threads/'+fresh+'/approval-mode')['mode']=='auto','/new did not start in Auto')
    check('/new starts in the open chat\'s mode')
    print(f'{len(checks)} real-server TUI scenarios passed. Evidence: {root}',flush=True)
    (root/'tui.txt').write_text(capture())
finally:
    subprocess.run(['tmux','kill-session','-t',name],capture_output=True)
    subprocess.run([str(binary),'--workspace',str(workspace),'serve','--stop'],env=env,capture_output=True,timeout=30)
