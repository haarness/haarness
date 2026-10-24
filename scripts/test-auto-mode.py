#!/usr/bin/env python3
"""Real Haarness server + real OpenTUI in tmux: Auto/Review permission lifecycle,
the approval card at several terminal sizes, the --auto flag, and the mode a chat
with no setting of its own starts in (Review when the person's configuration says
so, whatever the project says; Auto when no file sets it).

HAARNESS_E2E_FRAMES names a folder for captured frames (ANSI and text);
by default they land beside the other evidence."""
import json, os, pathlib, shlex, subprocess, tempfile, time, urllib.request, uuid
import e2e_scratch

binary = pathlib.Path(os.environ.get('HAARNESS_BIN', 'target/release/haarness')).resolve()
root = pathlib.Path(tempfile.mkdtemp(prefix='haarness-auto-e2e-')).resolve()
workspace = root / 'workspace'
workspace.mkdir()
(workspace / '.haarness').mkdir()
rules = '[defaults]\nprovider="echo"\nmodel="echo-1"\n[permissions]\nbash = { "*" = "ask", "printf denied-proof*" = "deny" }\n'
(workspace / '.haarness' / 'config.toml').write_text(rules)
# The person's configuration asks for Review and the project for Auto: Review in any file wins,
# so this folder's chats start in Review.
(root / 'data' / 'config').mkdir(parents=True)
(root / 'data' / 'config' / 'haarness.json').write_text(json.dumps({'defaults': {'approval_mode': 'review'}}))
(workspace / 'haarness.json').write_text(json.dumps({'defaults': {'approval_mode': 'auto'}}))
env = dict(os.environ, HAARNESS_DATA_DIR=str(root/'data'), HAARNESS_TEST_PROVIDER='1', HAARNESS_DEFAULT_PROVIDER='echo', HAARNESS_DEFAULT_MODEL='echo-1')
# The same rules in a folder, on data of its own, where no file sets a mode: its chats start in Auto.
unset = root / 'unset'
(unset / '.haarness').mkdir(parents=True)
(unset / '.haarness' / 'config.toml').write_text(rules)
unset_env = dict(env, HAARNESS_DATA_DIR=str(root/'unset-data'))
name = 'haarness-auto-' + str(os.getpid())
endpoint = None
checks = []
frames = pathlib.Path(os.environ.get('HAARNESS_E2E_FRAMES') or root/'frames')
frames.mkdir(parents=True, exist_ok=True)

def run(args, **kwargs):
    return subprocess.run(args, check=True, text=True, capture_output=True, **kwargs).stdout

def connect(folder=workspace, environment=env):
    return json.loads(run([str(binary), '--workspace', str(folder), '--trust-project', 'desktop'], env=environment, input='{"action":"connect"}', timeout=120))

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
    try:
        pane = capture()
    except subprocess.CalledProcessError:
        pane = '(no pane)'
    raise AssertionError(label+'\n'+pane)

def capture():
    return run(['tmux','capture-pane','-p','-t',name+':0.0'])

def keys(*keys):
    run(['tmux','send-keys','-t',name+':0.0',*keys])

def command(text):
    keys('C-u')
    run(['tmux','send-keys','-l','-t',name+':0.0',text])
    # Enter before the last keys are drawn can complete a shorter command.
    until(lambda:text in capture(),'typed command not shown: '+text)
    keys('Enter')

def card(pane):
    """The approval card's rows, from its ⚠ title to its answers: the
    transcript above repeats the command as it was sent."""
    rows=pane.split('\n')
    tops=[index for index,row in enumerate(rows) if '⚠ ' in row]
    if not tops:
        return ''
    end=next((index for index in range(tops[-1],len(rows)) if 'Allow once' in rows[index] or 'Confirm' in rows[index]),len(rows)-1)
    return '\n'.join(rows[tops[-1]:end+1])

def launch(thread, mode='review', width=160, height=40, extra=(), folder=workspace, environment=env):
    args=['env',*[k+'='+environment[k] for k in ['HAARNESS_DATA_DIR','HAARNESS_TEST_PROVIDER','HAARNESS_DEFAULT_PROVIDER','HAARNESS_DEFAULT_MODEL']],str(binary),'--workspace',str(folder),'--trust-project',*extra,'--thread',thread]
    run(['tmux','new-session','-d','-x',str(width),'-y',str(height),'-s',name,shlex.join(args)])
    until(lambda:mode in capture(), mode+' mode not visible at launch')

def resize(width, height):
    run(['tmux','resize-window','-t',name+':0','-x',str(width),'-y',str(height)])

def save(label):
    """Keep what the pane shows, colours included, for a person to look at."""
    (frames/(label+'.ansi')).write_text(run(['tmux','capture-pane','-p','-e','-t',name+':0.0']))
    (frames/(label+'.txt')).write_text(capture())

def start(thread, text):
    return api('/v1/threads/'+thread+'/turns', {'requestId':str(uuid.uuid4()),'text':text})

def finished():
    until(lambda:not api('/v1/turns/active'), 'turn did not finish')

def proof(file, expected, folder=workspace):
    path = folder/file
    until(lambda:path.exists() and path.read_text()==expected, 'missing execution proof '+file)
    finished()

def check(label):
    checks.append(label)
    print('PASS '+label, flush=True)

try:
    endpoint = connect()
    thread = api('/v1/threads', {'title':'Auto mode verification','provider':'echo','model':'echo-1'})['id']
    mode = api('/v1/threads/'+thread+'/approval-mode')
    assert mode['mode']=='review' and not mode['explicit'], mode
    check("a new chat starts in Review when the person's configuration says so, though the project says auto")
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
    until(lambda:'switch this chat to Auto' in capture(),'Once + Auto was not staged')
    assert not (workspace/'u.txt').exists()
    # A second u at typing speed is a word ("uuid"), not a confirmation.
    time.sleep(.5)
    keys('u')
    proof('u.txt','u-proof')
    until(lambda:'auto' in capture(),'Once + Auto did not enable Auto')
    check('Allow once + Auto asks to confirm, then executes the pending request and enables Auto')
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
    until(lambda:'asks before risky steps' in capture(),'Review command did not work')
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
    until(lambda:api('/v1/threads') and any(item['title'] in ('New chat','New thread') for item in api('/v1/threads')),'new chat missing')
    fresh=[item['id'] for item in api('/v1/threads') if item['title'] in ('New chat','New thread')][-1]
    until(lambda:api('/v1/threads/'+fresh+'/approval-mode')['mode']=='auto','/new did not start in Auto')
    check('/new starts in the open chat\'s mode')

    # A long command is read in full on the card, at every size, while its
    # answers stay on screen; under 12 rows the card shrinks to one line.
    run(['tmux','kill-session','-t',name])
    review=api('/v1/threads', {'title':'Long command verification','provider':'echo','model':'echo-1'})['id']
    long_command='\n'.join(f'printf "line-{n:02}\\n" >> multi.txt' for n in range(1,13))
    start(review,'!tool bash '+json.dumps({'command':long_command}))
    until(lambda:len(api('/v1/permissions'))==1,'the long command did not ask')
    launch(review,width=160,height=44)
    answers=lambda pane:all(label in pane for label in ('Allow once','Reject'))
    def whole_card(test,label):
        """The card once `test` holds and its answers are drawn, from that
        same capture: under load a capture can land between its rows."""
        seen={}
        def ready():
            seen['card']=card(capture())
            return test(seen['card']) and answers(seen['card'])
        until(ready,label)
        return seen['card']
    for width,height in [(160,44),(100,24),(80,20),(80,12)]:
        resize(width,height)
        shown=whole_card(lambda top:'$ printf "line-01' in top and ' of 12' in top,f'{width}x{height}: the card did not open on the first lines')
        assert 'line-12' not in shown, shown
        if (width,height) in ((160,44),(80,12)):
            save(f'card-{width}x{height}-top')
        keys('-N','12','Down')
        shown=whole_card(lambda end:'line-12' in end and '–12 of 12' in end,f'{width}x{height}: Down did not reach the last line')
        assert 'line-01' not in shown, shown
        if (width,height) in ((160,44),(80,12)):
            save(f'card-{width}x{height}-end')
        keys('-N','12','Up')
        until(lambda:'$ printf "line-01' in card(capture()) and 'line-12' not in card(capture()),f'{width}x{height}: Up did not return to the first line')
    resize(160,44)
    # Press ctrl+e once the TUI has redrawn at the new width (its rule spans
    # the window), not while it is still handling the resize.
    until(lambda:'━'*150 in capture(),'the TUI did not redraw at 160 columns')
    keys('C-e')
    until(lambda:'ctrl+e collapse' in card(capture()),'ctrl+e did not expand the command')
    shown=card(capture())
    assert all(f'line-{n:02}' in shown for n in range(1,13)) and answers(shown), shown
    save('card-160x44-expanded')
    keys('C-e')
    until(lambda:'ctrl+e expand' in card(capture()),'ctrl+e did not collapse the command')
    assert not (workspace/'multi.txt').exists()
    check('a long command scrolls and expands inside the card at 160x44, 100x24, 80x20 and 80x12, its answers always on screen')
    resize(80,11)
    # The compact row names the request with its command; the card's title
    # does not.
    until(lambda:'⚠ Run a shell command: printf' in capture() and len(capture().rstrip('\n').split('\n'))<=11,'the compact frame hid the request')
    pane=capture()
    rows=pane.rstrip('\n').split('\n')
    assert 'Allow once  y' in pane and 'Reject  n' in pane, pane
    assert 'working' not in rows[-1] and 'Run a shell command' in rows[-1], pane
    save('compact-80x11')
    keys('y')
    proof('multi.txt',''.join(f'line-{n:02}\n' for n in range(1,13)))
    check('under 12 rows the request, its answers and the approval hint stay on screen, and y runs it')

    # --auto: the chat goes to Auto on the server, so every client sees it.
    run(['tmux','kill-session','-t',name])
    flagged=api('/v1/threads', {'title':'Auto flag verification','provider':'echo','model':'echo-1'})['id']
    start(flagged,'!tool bash '+json.dumps({'command':'printf flag-proof > flag.txt'}))
    until(lambda:len(api('/v1/permissions'))==1,'a new chat did not ask before --auto')
    launch(flagged,'approvals: auto',extra=['--auto'])
    proof('flag.txt','flag-proof')
    assert api('/v1/threads/'+flagged+'/approval-mode')['mode']=='auto'
    check('haarness --auto opens the chat in Auto on the server and releases the request waiting in it')
    plain=subprocess.run([str(binary),'--workspace',str(workspace),'run','--title','Plain run','!tool bash '+json.dumps({'command':'printf plain-proof > plain.txt'})],env=env,input='',text=True,capture_output=True,timeout=120)
    finished()
    assert not (workspace/'plain.txt').exists(), plain.stderr
    run([str(binary),'--workspace',str(workspace),'run','--auto','--title','Run auto flag','!tool bash '+json.dumps({'command':'printf run-auto-proof > run-auto.txt'})],env=env,input='',timeout=120)
    proof('run-auto.txt','run-auto-proof')
    ran=[item['id'] for item in api('/v1/threads') if item['title']=='Run auto flag']
    assert len(ran)==1 and api('/v1/threads/'+ran[0]+'/approval-mode')['mode']=='auto', ran
    check('haarness run --auto puts its chat in Auto, so a run nobody watches gets its requests approved; a plain run is refused')

    # Where no file sets a mode, a new chat is in Auto: the reviewer approves an
    # ask once or blocks it, and deny rules still refuse.
    run(['tmux','kill-session','-t',name])
    endpoint = connect(unset, unset_env)
    fresh = api('/v1/threads', {'title':'No setting verification','provider':'echo','model':'echo-1'})['id']
    mode = api('/v1/threads/'+fresh+'/approval-mode')
    assert mode['mode']=='auto' and not mode['explicit'], mode
    start(fresh,'!tool bash '+json.dumps({'command':'printf unset-proof > unset.txt'}))
    proof('unset.txt','unset-proof',unset)
    assert not api('/v1/permissions')
    start(fresh,'!tool bash '+json.dumps({'command':'printf blocked-proof > blocked.txt'})+' [review:deny]')
    finished()
    assert not (unset/'blocked.txt').exists()
    assert 'Auto review blocked this action' in json.dumps(api('/v1/threads/'+fresh+'/items'))
    start(fresh,'!tool bash '+json.dumps({'command':'printf denied-proof > denied.txt'}))
    finished()
    assert not (unset/'denied.txt').exists() and not api('/v1/permissions')
    check('with no setting in any file a new chat is in Auto: the reviewer approves an ask once or blocks it, and a deny rule still refuses')
    launch(fresh,'approvals: auto',folder=unset,environment=unset_env)
    check('the TUI opens a chat with no setting in Auto')
    plain=subprocess.run([str(binary),'--workspace',str(unset),'run','--title','Unset run','!tool bash '+json.dumps({'command':'printf unset-run-proof > unset-run.txt'})],env=unset_env,input='',text=True,capture_output=True,timeout=120)
    assert plain.returncode==0, plain.stderr
    proof('unset-run.txt','unset-run-proof',unset)
    check('a plain haarness run there gets its ask answered by the reviewer instead of refused')
    print(f'{len(checks)} real-server TUI scenarios passed.',flush=True)
    (root/'tui.txt').write_text(capture())
finally:
    subprocess.run(['tmux','kill-session','-t',name],capture_output=True)
    subprocess.run([str(binary),'--workspace',str(workspace),'serve','--stop'],env=env,capture_output=True,timeout=30)
    subprocess.run([str(binary),'--workspace',str(unset),'serve','--stop'],env=unset_env,capture_output=True,timeout=30)
    e2e_scratch.settle(root)
